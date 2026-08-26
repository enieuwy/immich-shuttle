use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::{Duration, Instant},
};

use tauri::Emitter;
use uuid::Uuid;

use crate::{
    models::{
        history::RecordStatus,
        job::{FileError, ImportInput, ImportJob, JobProgress, JobStatus, Organization},
        media::{ScanProgress, ScanStatus, ScanSummary},
    },
    services::{
        immich_client::ImmichClient,
        keychain, logs, media_scanner, profile_store,
        sidecar_runner::{run_upload, UploadRequest},
        source_guard, staging, url_resolver, wipe,
    },
};

static JOBS: LazyLock<Mutex<Vec<ImportJob>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static PENDING_WIPE: LazyLock<Mutex<HashMap<String, PendingWipe>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static RUNNING_IMPORTS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Marks the post-run finalization phase that `RUNNING_IMPORTS` deliberately
/// does not cover. `import_await_terminal` must observe this set as clear too.
static FINALIZING_IMPORTS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static JOB_INPUTS: LazyLock<Mutex<HashMap<String, ImportInput>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const MAX_RETAINED_TERMINAL_JOBS: usize = 500;
const SCAN_DEADLINE: Duration = Duration::from_secs(60 * 60);
/// Ceiling for `import_await_terminal`'s IPC-supplied `timeout_ms`. Ample
/// above the 30_000ms production ever passes; exists only so an untrusted
/// huge value can't push `Instant::now() + Duration::from_millis(..)` past
/// `Instant`'s `Add` overflow panic.
const MAX_AWAIT_TERMINAL_TIMEOUT_MS: u64 = 600_000;
const IMPORT_WORKER_PANIC_ERROR: &str = "Import worker stopped unexpectedly.";
const PENDING_WIPE_STORE_ERROR: &str =
    "Could not prepare wipe confirmation; source files were kept on disk. Import the source again to retry the delete.";

fn mark_worker_panic(job_id: &str) {
    let marked = {
        let mut jobs = match JOBS.lock() {
            Ok(jobs) => jobs,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else {
            return;
        };
        if !matches!(job.status, JobStatus::Running) {
            return;
        }

        job.status = JobStatus::Failed;
        job.progress.errors = job.progress.errors.saturating_add(1);
        job.error = Some(IMPORT_WORKER_PANIC_ERROR.to_string());
        job.summary = None;
        job.awaiting_wipe_confirmation = false;
        job.pending_wipe_count = 0;
        true
    };

    // The prompt is gone, so any payload this run registered is unreachable —
    // and it holds the profile's API key. Drop it rather than leave it resident
    // until an unrelated dismiss or eviction happens to clear it. Taken after
    // the `JOBS` guard is released, matching `finalize_job`'s lock order.
    if marked {
        if let Ok(mut pending) = PENDING_WIPE.lock() {
            pending.remove(job_id);
        }
    }
}

static IMPORT_START_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static ACTIVE_SCAN: LazyLock<Mutex<Option<Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(None));
static ACTIVE_FORECAST: LazyLock<Mutex<Option<Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Clears the active forecast only when this forecast still owns the slot.
struct ActiveForecastGuard {
    cancellation: Arc<AtomicBool>,
}

impl Drop for ActiveForecastGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE_FORECAST.lock() {
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.cancellation))
            {
                active.take();
            }
        }
    }
}

struct PendingWipe {
    paths: Vec<String>,
    server_url: String,
    api_key: String,
}
/// Removes a worker's liveness markers if its body unwinds before normal
/// finalization. `JOB_INPUTS` deliberately stays intact because `import_retry`
/// needs the saved request after a failed or panicking run.
struct ImportWorkerGuard {
    job_id: String,
}

impl ImportWorkerGuard {
    fn new(job_id: String) -> Self {
        Self { job_id }
    }
}

impl Drop for ImportWorkerGuard {
    fn drop(&mut self) {
        {
            let mut running = match RUNNING_IMPORTS.lock() {
                Ok(running) => running,
                Err(poisoned) => poisoned.into_inner(),
            };
            running.remove(&self.job_id);
        }

        {
            let mut finalizing = match FINALIZING_IMPORTS.lock() {
                Ok(finalizing) => finalizing,
                Err(poisoned) => poisoned.into_inner(),
            };
            finalizing.remove(&self.job_id);
        }

        mark_worker_panic(&self.job_id);
    }
}

fn collapse_overlapping_roots(paths: Vec<String>) -> Vec<String> {
    let roots: Vec<(String, Option<PathBuf>)> = paths
        .into_iter()
        .map(|path| {
            let canonical = std::fs::canonicalize(&path).ok();
            (path, canonical)
        })
        .collect();
    let mut seen_raw_paths = HashSet::new();

    roots
        .iter()
        .enumerate()
        .filter_map(|(index, (path, canonical_path))| {
            if !seen_raw_paths.insert(path.clone()) {
                return None;
            }
            let Some(canonical_path) = canonical_path.as_ref() else {
                return Some(path.clone());
            };
            if roots[..index]
                .iter()
                .any(|(_, other)| other.as_ref() == Some(canonical_path))
            {
                return None;
            }
            if roots
                .iter()
                .enumerate()
                .any(|(other_index, (_, ancestor))| {
                    other_index != index
                        && ancestor.as_ref().is_some_and(|ancestor| {
                            ancestor != canonical_path && canonical_path.starts_with(ancestor)
                        })
                })
            {
                return None;
            }
            Some(canonical_path.to_string_lossy().into_owned())
        })
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Error returned when a cancel arrives for a job that already finished.
///
/// The frontend matches this text to treat the cancel as a no-op rather than a
/// failure, so app quit is not blocked by a race it already won (`shutdown.ts`,
/// `isAlreadyTerminal`). Rewording it changes that behaviour, so the wording is
/// pinned by a test in this module.
pub const TERMINAL_CANCEL_ERROR: &str = "Cannot cancel a terminal import";

/// Prefix of the error returned when a job id is not in `JOBS`.
///
/// The frontend matches this text to tell "the job was evicted" apart from a
/// real IPC failure, so app quit can proceed instead of waiting for a job that
/// no longer exists (`shutdown.ts`, `isJobAlreadyGone`). Rewording it changes
/// that behaviour, so the wording is pinned by a test in this module.
pub const JOB_NOT_FOUND_ERROR: &str = "Job not found:";

fn get_job(job_id: &str) -> Result<ImportJob, String> {
    let jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    jobs.iter()
        .find(|j| j.id == job_id)
        .cloned()
        .ok_or_else(|| format!("{JOB_NOT_FOUND_ERROR} {job_id}"))
}

fn is_active(status: &JobStatus) -> bool {
    matches!(status, JobStatus::Running | JobStatus::Pending)
}

fn is_terminal(status: &JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
    )
}

fn has_active_import() -> Result<bool, String> {
    let jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    let has_active_job = jobs.iter().any(|job| is_active(&job.status));
    drop(jobs);
    let running = RUNNING_IMPORTS
        .lock()
        .map_err(|_| "Could not lock running imports state".to_string())?;
    Ok(has_active_job || !running.is_empty())
}
#[cfg(target_os = "macos")]
/// Reports whether an import worker or its post-run finalization is still live.
///
/// A poisoned lock fails safe: shutdown must not proceed while the worker state
/// cannot be read with confidence.
pub fn has_live_import_worker() -> bool {
    if has_active_import().unwrap_or(true) {
        return true;
    }
    FINALIZING_IMPORTS
        .lock()
        .map(|finalizing| !finalizing.is_empty())
        .unwrap_or(true)
}

/// Register a fake finalizing worker so another module can exercise a guard that
/// depends on worker liveness. Test-only: production liveness comes from the
/// worker's own registration.
#[cfg(all(test, target_os = "macos"))]
pub fn mark_worker_live_for_test(job_id: &str) {
    FINALIZING_IMPORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(job_id.to_string());
}

#[cfg(all(test, target_os = "macos"))]
pub fn clear_worker_live_for_test(job_id: &str) {
    FINALIZING_IMPORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(job_id);
}

fn evict_old_terminal_jobs(jobs: &mut Vec<ImportJob>) -> Vec<String> {
    // The 500-job cap bounds only clearable terminal jobs. Unanswered wipe
    // prompts are bounded by what the user has not answered, and evicting one
    // would strand verified-uploaded originals with no way back to the prompt.
    let terminal_count = jobs.iter().filter(|job| is_clearable(job)).count();
    let excess = terminal_count.saturating_sub(MAX_RETAINED_TERMINAL_JOBS);
    if excess == 0 {
        return Vec::new();
    }

    let evicted: HashSet<String> = jobs
        .iter()
        .filter(|job| is_clearable(job))
        .take(excess)
        .map(|job| job.id.clone())
        .collect();
    jobs.retain(|job| !evicted.contains(&job.id));
    evicted.into_iter().collect()
}

fn remove_job_state(job_ids: &[String]) {
    if let Ok(mut inputs) = JOB_INPUTS.lock() {
        for id in job_ids {
            inputs.remove(id);
        }
    }
    if let Ok(mut pending) = PENDING_WIPE.lock() {
        for id in job_ids {
            pending.remove(id);
        }
    }
}

fn insert_initial_job(
    job: ImportJob,
    input: ImportInput,
    cancel_flag: Arc<AtomicBool>,
) -> Result<(), String> {
    let evicted_ids = {
        let mut running = RUNNING_IMPORTS
            .lock()
            .map_err(|_| "Could not lock running imports state".to_string())?;
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "Could not lock import job state".to_string())?;
        if !running.is_empty() || jobs.iter().any(|existing| is_active(&existing.status)) {
            return Err("An import is already running".to_string());
        }
        let mut inputs = JOB_INPUTS
            .lock()
            .map_err(|_| "Could not lock import inputs".to_string())?;

        let job_id = job.id.clone();
        inputs.insert(job_id.clone(), input);
        jobs.push(job);
        running.insert(job_id, cancel_flag);
        evict_old_terminal_jobs(&mut jobs)
    };
    remove_job_state(&evicted_ids);
    Ok(())
}

fn set_job(job: ImportJob) -> Result<(), String> {
    let evicted_ids = {
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "Could not lock import job state".to_string())?;
        let Some(index) = jobs.iter().position(|existing| existing.id == job.id) else {
            return Ok(());
        };

        let terminal = is_terminal(&job.status);
        jobs[index] = job;
        // Terminal jobs are ordered by their last state transition so eviction
        // keeps the most recently completed/cancelled/failed jobs.
        if terminal {
            let job = jobs.remove(index);
            jobs.push(job);
        }
        evict_old_terminal_jobs(&mut jobs)
    };
    remove_job_state(&evicted_ids);
    Ok(())
}

/// Commit a worker's terminal state for a job, refusing to move it out of
/// `Cancelled`, and return the state that is actually stored.
///
/// `import_cancel` publishes `Cancelled` while the worker is still winding down,
/// so the worker's own final write can land afterwards. Without this guard that
/// write revives a cancelled import as Completed/Failed and — worse — leaves the
/// wipe payload in place, offering the user's originals for deletion after they
/// asked the run to stop. The status check and the write share one lock hold so a
/// cancellation cannot slip between them.
fn finalize_job(update: ImportJob) -> ImportJob {
    let mut cancelled_id: Option<String> = None;
    let mut evicted_ids: Vec<String> = Vec::new();

    let stored = {
        let Ok(mut jobs) = JOBS.lock() else {
            return update;
        };
        let Some(index) = jobs.iter().position(|existing| existing.id == update.id) else {
            return update;
        };

        if matches!(jobs[index].status, JobStatus::Cancelled)
            && !matches!(update.status, JobStatus::Cancelled)
        {
            cancelled_id = Some(update.id);
            jobs[index].clone()
        } else if matches!(jobs[index].status, JobStatus::Failed)
            && jobs[index].error.as_deref() == Some(IMPORT_WORKER_PANIC_ERROR)
        {
            // A panic guard publishes this terminal state while unwinding.
            // Never let a late finalization write revive the job.
            jobs[index].clone()
        } else {
            let terminal = is_terminal(&update.status);
            let stored = update.clone();
            jobs[index] = update;
            // Terminal jobs move to the end so eviction keeps the most recent.
            if terminal {
                let job = jobs.remove(index);
                jobs.push(job);
            }
            evicted_ids = evict_old_terminal_jobs(&mut jobs);
            stored
        }
    };

    match cancelled_id {
        // The cancel path already dropped its own payload; drop anything this run
        // registered so a cancelled import cannot reach the wipe prompt.
        Some(id) => {
            if let Ok(mut pending) = PENDING_WIPE.lock() {
                pending.remove(&id);
            }
        }
        None => remove_job_state(&evicted_ids),
    }

    stored
}

/// Re-verify renderer-supplied selected paths against the user-approved source
/// roots. The frontend sends `select_files` over IPC, so a compromised or buggy
/// renderer could point staging at files outside the chosen folders; we reject
/// any entry that does not canonicalize to a path nested under a source root.
fn validate_selected_under_sources(
    select_files: &[String],
    source_paths: &[String],
) -> Result<(), String> {
    let roots: Vec<PathBuf> = source_paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)))
        .collect();
    for sel in select_files {
        let canon = std::fs::canonicalize(sel)
            .map_err(|e| format!("Selected file is not accessible: {sel} ({e})"))?;
        if !roots.iter().any(|root| canon.starts_with(root)) {
            return Err(format!(
                "Selected file is outside the chosen source folders: {sel}"
            ));
        }
    }
    Ok(())
}

/// Drop paths that do not resolve under one of the approved source roots,
/// returning the kept paths and the number discarded.
///
/// Unlike `validate_selected_under_sources` this filters instead of failing: the
/// input is the run log's own account of what it uploaded, where a single
/// unresolvable entry (a file moved or unplugged mid-run) must not block the wipe
/// for everything else.
///
/// This is the last containment check before originals become deletion
/// candidates. It matters because `file=` values in the run log are
/// attacker-influenced: console-slog writes attribute values unescaped, so a
/// filename or directory containing a newline on untrusted media can forge a
/// whole `INF uploaded successfully file=/somewhere/else` record. This only
/// rules out a forged path outside every source root — a forged record that
/// instead names a different, real file already sitting under the SAME root
/// survives untouched. The damage from that case is bounded downstream, not
/// here: `verify_uploaded` still requires a live SHA-1 match against the
/// server before a file is queued, `wipe_files` moves originals to Trash
/// rather than unlinking them, and the user must still confirm the wipe.
fn retain_paths_under_sources(paths: Vec<String>, source_paths: &[String]) -> (Vec<String>, usize) {
    let roots: Vec<PathBuf> = source_paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)))
        .collect();
    let total = paths.len();
    let kept: Vec<String> = paths
        .into_iter()
        .filter(|path| {
            std::fs::canonicalize(path)
                .is_ok_and(|canon| roots.iter().any(|root| canon.starts_with(root)))
        })
        .collect();
    let dropped = total - kept.len();
    (kept, dropped)
}

/// Translate paths that immich-go logged under the temporary staging root.
/// Unmapped paths are dropped because the run log is not trusted enough to guess
/// which user file should become a deletion candidate.
fn translate_staged_path(path: &str, links: &staging::StagingPathMap) -> Option<String> {
    links
        .original_for(Path::new(path))
        .map(|original| original.to_string_lossy().into_owned())
}

fn translate_staged_paths(
    paths: Vec<String>,
    links: &staging::StagingPathMap,
) -> (Vec<String>, usize) {
    let total = paths.len();
    let translated = paths
        .into_iter()
        .filter_map(|path| translate_staged_path(&path, links))
        .collect::<Vec<_>>();
    let dropped = total - translated.len();
    (translated, dropped)
}

fn translate_staged_file_errors(
    errors: Vec<FileError>,
    links: &staging::StagingPathMap,
) -> (Vec<FileError>, usize) {
    let total = errors.len();
    let translated = errors
        .into_iter()
        .filter_map(|mut error| {
            error.file = translate_staged_path(&error.file, links)?;
            Some(error)
        })
        .collect::<Vec<_>>();
    let dropped = total - translated.len();
    (translated, dropped)
}

/// Return log contents and a visible error when the completed run log cannot be
/// read. Keeping this pure lets the failure path stay covered without I/O.
fn read_run_log(result: Result<String, std::io::Error>) -> (String, Option<String>) {
    match result {
        Ok(contents) => (contents, None),
        Err(error) => (
            String::new(),
            Some(format!("Could not read import run log: {error}")),
        ),
    }
}

/// Final classification of an import process that ran to completion.
struct RunOutcome {
    status: JobStatus,
    /// Whether the uploaded originals may proceed to the verify-then-delete wipe.
    wipe_eligible: bool,
    /// Whether this run may advance the source's "only new since last import"
    /// checkpoint. Deliberately stricter than `status`: see below.
    checkpoint_eligible: bool,
}

/// Decide the final job status, wipe eligibility, and checkpoint eligibility from
/// a completed run's tallies. Kept pure (no globals, no I/O) because this is the
/// verify-before-delete safety surface: a regression here could delete originals
/// after a failed run or wrongly withhold deletion.
///
/// A run is a failure only when nothing landed on the server (no uploads, no
/// duplicate matches) AND it ended badly (non-zero exit or per-file errors); a
/// partial run that uploaded or matched duplicates succeeds, surfacing errors.
/// Wipe is eligible only for a successful run with keep-files off and at least
/// one completed path.
///
/// The checkpoint is separate and stricter because advancing it is a silent,
/// irreversible narrowing of every later import: it becomes a capture-date floor
/// passed to immich-go, and media added afterwards with an older capture date
/// falls below it and is never offered again. So it requires positive evidence
/// that this run actually processed the source — at least one landed asset and no
/// aggregate scan error — and that evidence must have survived containment (see
/// `completed_paths_len` below): a forged log entry naming a file that does not
/// exist on disk cannot supply it. A zero-asset run (empty card, or filters that
/// excluded everything) is still `Completed`, because nothing went wrong; it just
/// has not earned the right to raise the floor. Erring this way costs a re-scan
/// that server-side dedupe makes harmless; erring the other way loses photos.
fn classify_completed_run(
    uploaded: u32,
    duplicates: u32,
    exit_nonzero: bool,
    file_errors_len: usize,
    keep_files: bool,
    completed_paths_len: usize,
    scan_errors: u32,
) -> RunOutcome {
    let landed = uploaded > 0 || duplicates > 0;
    let failed = !landed && (exit_nonzero || file_errors_len > 0);
    let status = if failed {
        JobStatus::Failed
    } else {
        JobStatus::Completed
    };
    let wipe_eligible = !failed && !keep_files && completed_paths_len > 0;
    // `landed` alone is not sufficient evidence for the checkpoint: it is read
    // straight off the immich-go log's uploaded/duplicate tallies, and those are
    // resolved against an invocation root by string matching only — no
    // filesystem check. A record forged under a root (see
    // `retain_paths_under_sources`) can inflate `landed` without ever
    // surviving containment. `completed_paths_len` is exactly the count that
    // did survive containment (via `retain_paths_under_sources`, or via the
    // staging link map's successful-path translation for staged imports — see
    // the call site), so require it here too. `landed` keeps its existing role
    // in `failed` above unchanged, so ordinary run status is unaffected; only
    // the stricter checkpoint gate gains this extra requirement.
    let checkpoint_eligible = !failed
        && landed
        && completed_paths_len > 0
        && file_errors_len == 0
        && !exit_nonzero
        && scan_errors == 0;
    RunOutcome {
        status,
        wipe_eligible,
        checkpoint_eligible,
    }
}

fn wipe_prompt_state(
    wipe_eligible: bool,
    pending_wipe_stored: bool,
    pending_wipe_count: usize,
) -> (bool, u32) {
    if wipe_eligible && pending_wipe_stored {
        (true, pending_wipe_count as u32)
    } else {
        (false, 0)
    }
}

#[tauri::command]
pub async fn import_start(app: tauri::AppHandle, input: ImportInput) -> Result<String, String> {
    if input.source_paths.is_empty() {
        return Err("At least one source path is required".to_string());
    }

    let profile = profile_store::get_profile(&input.profile_id)?;
    let api_key = keychain::require_api_key(&input.profile_id)?;

    let source_paths = collapse_overlapping_roots(input.source_paths.clone());
    let record_source_paths = source_paths.clone();
    // Selected (staged) imports honor the same keep/delete toggle as whole-folder
    // imports; the post-wipe SHA-1 verification guards deletion either way.
    let keep_files = input.keep_files;
    let stack_raw_jpeg = input.stack_raw_jpeg;
    let stack_burst = input.stack_burst;
    let date_range = input.date_range.clone();
    // The UI limits this to 1..=20; re-clamp here since the value arrives over
    // IPC and must not be trusted to be in range (unbounded values would be
    // forwarded straight to immich-go's --concurrent-tasks).
    let concurrent_tasks = input.concurrent_tasks.map(|n| n.clamp(1, 20));
    let album_ids = input.album_ids.clone();
    let into_album = input.into_album.clone();
    let organization = input.organization;
    // `on_errors` arrives over IPC. Refuse an unknown value instead of dropping
    // it: the fallback is immich-go's default of stopping at the first per-file
    // error, so a typo would silently invert "keep going on errors" and abort a
    // long unattended run. This boundary already rejects out-of-scope paths and
    // bad album roles rather than normalizing them.
    let on_errors = match input.on_errors.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(value) if value == "stop" || value == "continue" || value.parse::<u32>().is_ok() => {
            Some(value.to_string())
        }
        Some(value) => {
            return Err(format!(
                "Unsupported error mode: {value}. Expected \"stop\", \"continue\", or a number."
            ))
        }
    };
    let overwrite = input.overwrite;
    let tags: Vec<String> = input
        .tags
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let session_tag = input.session_tag;
    let include_type = parse_include_type(input.include_type.as_deref())?;
    let include_extensions = normalize_extensions(&input.include_extensions);
    let exclude_extensions = normalize_extensions(&input.exclude_extensions);
    // immich-go uploads into a single album per run (`--into-album`), so more
    // than one id is not a request this command can honour. Only element 0 was
    // ever read; refuse the rest rather than discard it silently.
    if album_ids.len() > 1 {
        return Err(format!(
            "An import targets one album, but {} were selected.",
            album_ids.len()
        ));
    }
    // The "Open in Immich" deep-link points at a specific album only when the run
    // actually targets one: SingleAlbum mode AND a non-empty --into-album name
    // (folder/tag modes fan out; an unresolved selection sends no into_album, so
    // the card must not claim an album the upload never populated).
    let into_album_active = into_album
        .as_deref()
        .map(|a| !a.trim().is_empty())
        .unwrap_or(false);
    // Provisional: the id the picker chose, shown while the run is in flight. The
    // album the upload actually populated is resolved from its name at
    // finalization, because the NAME is what immich-go targets.
    let provisional_album_id = if organization == Organization::SingleAlbum && into_album_active {
        album_ids.first().cloned()
    } else {
        None
    };
    let select_files = input.select_files.clone().unwrap_or_default();
    let staging_requested = !select_files.is_empty();
    if staging_requested {
        validate_selected_under_sources(&select_files, &source_paths)?;
    }

    let job_id = Uuid::new_v4().to_string();
    let log_path = logs::logs_dir()?.join(format!("run-{job_id}.log"));
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let initial = ImportJob {
        id: job_id.clone(),
        status: JobStatus::Running,
        progress: JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        },
        error: None,
        summary: None,
        awaiting_wipe_confirmation: false,
        pending_wipe_count: 0,
        file_errors: Vec::new(),
        profile_id: input.profile_id.clone(),
        album_id: provisional_album_id.clone(),
    };

    // Publish a job only after all fallible setup has succeeded. The admission
    // lock serializes the check and insertion so two IPC calls cannot both begin.
    {
        let _start_lock = IMPORT_START_LOCK
            .lock()
            .map_err(|_| "Could not lock import start state".to_string())?;
        if has_active_import()? {
            return Err("An import is already running".to_string());
        }
        logs::append_log(
            "app.log",
            &format!(
                "import_start job_id={job_id} profile_id={}",
                input.profile_id
            ),
        )?;
        insert_initial_job(initial, input.clone(), cancel_flag.clone())?;
    }

    let started_at = now_ms();
    let job_id_clone = job_id.clone();
    let api_key_clone = api_key;
    let app_clone = app.clone();
    let device_uuid = format!("immich-shuttle-{}", Uuid::new_v4());
    // The full request is persisted to history for "Import again". Move the
    // original input in (it is unused after admission) and drop the one-time
    // staged subset, avoiding a retained copy plus a deep clone of select_files.
    let mut history_request = input;
    history_request.select_files = None;

    tauri::async_runtime::spawn(async move {
        // Keep liveness state recoverable if any worker operation panics before
        // the explicit normal-path removals below run.
        let _worker_guard = ImportWorkerGuard::new(job_id_clone.clone());
        // Used at finalization by both the wipe payload and the album lookup.
        let api_key_for_finalization = api_key_clone.clone();
        let mut staging_dir = if staging_requested {
            let selected_files = select_files.clone();
            let cancel_flag_for_staging = cancel_flag.clone();
            match tauri::async_runtime::spawn_blocking(move || {
                staging::create_staging_dir(&selected_files, Some(cancel_flag_for_staging.as_ref()))
            })
            .await
            {
                Ok(Ok(dir)) => Some(dir),
                Ok(Err(e)) => {
                    if let Ok(mut running) = RUNNING_IMPORTS.lock() {
                        running.remove(&job_id_clone);
                    }
                    if cancel_flag.load(Ordering::Relaxed) {
                        return;
                    }
                    // `finalize_job` preserves a cancellation published while
                    // staging reported its failure.
                    let _ = finalize_job(ImportJob {
                        id: job_id_clone.clone(),
                        status: JobStatus::Failed,
                        progress: JobProgress {
                            total: 0,
                            uploaded: 0,
                            duplicates: 0,
                            errors: 1,
                        },
                        error: Some(format!("Could not stage selected files: {e}")),
                        summary: None,
                        awaiting_wipe_confirmation: false,
                        pending_wipe_count: 0,
                        file_errors: Vec::new(),
                        profile_id: profile.id.clone(),
                        album_id: None,
                    });
                    return;
                }
                Err(e) => {
                    if let Ok(mut running) = RUNNING_IMPORTS.lock() {
                        running.remove(&job_id_clone);
                    }
                    // A staging task that fails to join must not revive a
                    // cancelled run as `Failed`, so use `finalize_job`.
                    let _ = finalize_job(ImportJob {
                        id: job_id_clone.clone(),
                        status: JobStatus::Failed,
                        progress: JobProgress {
                            total: 0,
                            uploaded: 0,
                            duplicates: 0,
                            errors: 1,
                        },
                        error: Some(format!("Staging task failed: {e}")),
                        summary: None,
                        awaiting_wipe_confirmation: false,
                        pending_wipe_count: 0,
                        file_errors: Vec::new(),
                        profile_id: profile.id.clone(),
                        album_id: None,
                    });
                    return;
                }
            }
        } else {
            None
        };
        let staged_import = staging_dir.is_some();
        let upload_paths: Vec<String> = match &staging_dir {
            Some(dir) => vec![dir.path().to_string_lossy().to_string()],
            None => source_paths.clone(),
        };
        // The paths immich-go is actually invoked against: the temp staging dir
        // for a staged (hand-picked) import, otherwise the user's source paths.
        // Cloned here (before the run loop below consumes `upload_paths`) so both
        // the live log parser (via `UploadRequest.log_source_roots`) and the
        // final authoritative parse agree on what the process actually saw —
        // `source_paths` alone would miss every `file=` record under a staging
        // symlink dir and silently zero out uploaded/duplicate counts.
        let invocation_roots = upload_paths.clone();
        // Resolve the reachable endpoint inside the task: the LAN/WAN probe can
        // take up to a few seconds, so keep it off the IPC path that returns the
        // job id to the frontend.
        let server_url = url_resolver::resolve_server_url(&profile).await;
        let request = UploadRequest {
            job_id: job_id_clone.clone(),
            server_url,
            api_key: api_key_clone,
            source_path: upload_paths[0].clone(),
            log_path,
            log_source_roots: invocation_roots.clone(),
            device_uuid,
            cancel_flag: cancel_flag.clone(),
            stack_raw_jpeg,
            stack_burst,
            date_range,
            concurrent_tasks,
            into_album,
            organization,
            on_errors,
            overwrite,
            tags,
            session_tag,
            include_type,
            include_extensions,
            exclude_extensions,
        };
        let mut error_lines: Vec<String> = Vec::new();
        let mut exit_nonzero = false;
        let mut cancelled = false;
        let mut spawn_error: Option<String> = None;

        let mut request = request;
        for path in upload_paths {
            request.source_path = path;
            match run_upload(app_clone.clone(), request.clone()).await {
                Ok(run) => {
                    error_lines.extend(run.error_lines);
                    exit_nonzero |= run.exit_nonzero;
                }
                Err(err) => {
                    if err == "Cancelled by user" {
                        cancelled = true;
                    } else {
                        spawn_error = Some(err);
                    }
                    break;
                }
            }
        }

        // Take the map before cleanup consumes the guard. The log is parsed after
        // the temporary directory is gone, so only this map can restore user paths.
        let staged_links = staging_dir
            .as_mut()
            .map(|dir| dir.take_links())
            .unwrap_or_default();
        if let Some(dir) = staging_dir {
            let _ = tauri::async_runtime::spawn_blocking(move || {
                staging::cleanup_staging_dir(dir);
            })
            .await;
        }

        // Keep post-run work visible after the run leaves `RUNNING_IMPORTS`, so
        // shutdown waits for log parsing and history persistence.
        if let Ok(mut finalizing) = FINALIZING_IMPORTS.lock() {
            finalizing.insert(job_id_clone.clone());
        }
        if let Ok(mut running) = RUNNING_IMPORTS.lock() {
            running.remove(&job_id_clone);
        }

        // immich-go writes per-file events to the run log (stdout only carries a
        // `\r`-refreshed aggregate that can't be read reliably through the pipe).
        // The log is O_APPEND across multi-path runs, so one read afterwards
        // yields the authoritative totals, completed paths, and per-file errors.
        let (log_contents, log_read_error) =
            read_run_log(std::fs::read_to_string(&request.log_path));
        if let Some(error) = log_read_error.as_ref() {
            let _ = logs::append_log(
                "app.log",
                &format!("import_run_log_read_failed job_id={job_id_clone} error={error}"),
            );
        }
        let parsed_file_errors = crate::services::stdout_parser::parse_error_log(
            &log_contents,
            if staged_import {
                &invocation_roots
            } else {
                &source_paths
            },
        );
        let (file_errors, dropped_staged_errors) = if staged_import {
            translate_staged_file_errors(parsed_file_errors, &staged_links)
        } else {
            (parsed_file_errors, 0)
        };
        if dropped_staged_errors > 0 {
            let _ = logs::append_log(
                "app.log",
                &format!(
                    "import_staged_paths_unmapped job_id={job_id_clone} errors={dropped_staged_errors}"
                ),
            );
        }
        let run =
            crate::services::stdout_parser::parse_run_progress(&log_contents, &invocation_roots);
        // A non-zero count here means some `file=` records in the run log did
        // not resolve against any invocation root. Resolution is now mandatory
        // for the uploaded/duplicate tallies and the checkpoint gate (see
        // `classify_completed_run`), so an immich-go log-format drift would
        // otherwise silently zero every count with no signal anywhere; log it
        // so drift shows up as a named anomaly instead.
        if run.unresolved_file_events > 0 {
            let _ = logs::append_log(
                "app.log",
                &format!(
                    "import_unresolved_file_events job_id={job_id_clone} count={}",
                    run.unresolved_file_events
                ),
            );
        }
        // parse_run_progress counts every distinct errored file (uncapped);
        // file_errors is capped at MAX_FILE_ERRORS for the UI payload. Keep the
        // true count so the final tally never undercounts a mass-failure run.
        let progress = run.progress;
        // `run.completed_paths` is authoritative for wipe candidates. A staged
        // run reports temporary destinations, so restore only paths in the
        // successful-link map captured before cleanup. An unmapped destination
        // is dropped rather than guessed, because it cannot safely name an
        // original file.
        let completed_asset_paths = if staged_import {
            let (translated, dropped) = translate_staged_paths(run.completed_paths, &staged_links);
            if dropped > 0 {
                let _ = logs::append_log(
                    "app.log",
                    &format!(
                        "import_staged_completed_paths_unmapped job_id={job_id_clone} dropped={dropped}"
                    ),
                );
            }
            translated
        } else {
            // Re-contain what the log claims was uploaded: these paths become
            // deletion candidates, and the log is not a trusted channel.
            let (kept, dropped) = retain_paths_under_sources(run.completed_paths, &source_paths);
            if dropped > 0 {
                let _ = logs::append_log(
                    "app.log",
                    &format!(
                        "import_completed_paths_outside_sources job_id={job_id_clone} dropped={dropped}"
                    ),
                );
            }
            kept
        };

        // The sidecar can exit on its own between the loop's last cancel check and
        // a cancel request arriving, so run_upload returns Ok and `cancelled` stays
        // false even though the user did cancel. Re-read the flag here, before any
        // wipe payload is built: a cancelled run must never become wipe-eligible.
        let cancelled = cancelled || cancel_flag.load(Ordering::Relaxed);

        // Only a completed run can earn the checkpoint; cancelled/failed stay false.
        let mut checkpoint_eligible = false;
        let update = if cancelled {
            ImportJob {
                id: job_id_clone.clone(),
                status: JobStatus::Cancelled,
                progress: JobProgress {
                    total: 0,
                    uploaded: 0,
                    duplicates: 0,
                    errors: 0,
                },
                error: None,
                summary: Some("Import cancelled by user.".to_string()),
                awaiting_wipe_confirmation: false,
                pending_wipe_count: 0,
                file_errors: Vec::new(),
                profile_id: profile.id.clone(),
                album_id: None,
            }
        } else if let Some(err) = spawn_error {
            ImportJob {
                id: job_id_clone.clone(),
                status: JobStatus::Failed,
                progress,
                error: Some(err),
                summary: None,
                awaiting_wipe_confirmation: false,
                pending_wipe_count: 0,
                file_errors: file_errors.clone(),
                profile_id: profile.id.clone(),
                album_id: None,
            }
        } else {
            // The process ran to completion. classify_completed_run owns the
            // failure + verify-before-delete decision so it can be unit-tested in
            // isolation from the async command body.
            let RunOutcome {
                status,
                wipe_eligible,
                checkpoint_eligible: eligible,
            } = classify_completed_run(
                progress.uploaded,
                progress.duplicates,
                exit_nonzero,
                file_errors.len() + dropped_staged_errors,
                keep_files,
                completed_asset_paths.len(),
                run.scan_errors,
            );
            checkpoint_eligible = eligible;
            let failed = matches!(status, JobStatus::Failed);
            let mut pending_wipe_stored = false;
            let mut pending_wipe_store_failed = false;
            if wipe_eligible {
                match PENDING_WIPE.lock() {
                    Ok(mut pending) => {
                        pending.insert(
                            job_id_clone.clone(),
                            PendingWipe {
                                paths: completed_asset_paths.clone(),
                                // Verify against the URL the upload actually used
                                // (post-failover), not the primary configured one,
                                // or the existence check can hit the wrong server.
                                server_url: request.server_url.clone(),
                                api_key: api_key_for_finalization.clone(),
                            },
                        );
                        pending_wipe_stored = true;
                    }
                    Err(_) => {
                        pending_wipe_store_failed = true;
                        let _ = logs::append_log(
                            "app.log",
                            &format!(
                                "import_wipe_pending_store_failed job_id={} pending_count={}",
                                job_id_clone,
                                completed_asset_paths.len()
                            ),
                        );
                    }
                }
            }

            let error = if pending_wipe_store_failed {
                Some(PENDING_WIPE_STORE_ERROR.to_string())
            } else if let Some(log_error) = log_read_error {
                Some(log_error)
            } else if failed {
                let tail: Vec<&str> = error_lines
                    .iter()
                    .rev()
                    .take(3)
                    .map(|s| s.as_str())
                    .collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                Some(if tail.is_empty() {
                    "immich-go reported errors during upload".to_string()
                } else {
                    tail.join(" | ")
                })
            } else if !file_errors.is_empty() {
                Some(format!(
                    "{} file(s) could not be uploaded; see the error list.",
                    file_errors.len()
                ))
            } else {
                None
            };

            let (awaiting_wipe_confirmation, pending_wipe_count) = wipe_prompt_state(
                wipe_eligible,
                pending_wipe_stored,
                completed_asset_paths.len(),
            );
            let summary = if failed {
                None
            } else {
                let head = format!(
                    "Upload completed. {} uploaded, {} duplicates, {} errors.",
                    progress.uploaded, progress.duplicates, progress.errors
                );
                Some(if keep_files {
                    format!("{head} Files kept on disk.")
                } else if awaiting_wipe_confirmation {
                    format!("{head} Awaiting wipe confirmation.")
                } else if pending_wipe_store_failed {
                    format!("{head} Source files were kept on disk because wipe confirmation could not be prepared.")
                } else {
                    head
                })
            };

            // The deep link must name the album the upload actually populated.
            // immich-go targets albums by NAME and creates one that does not
            // exist yet, so the picker's id is only a hint: a device-rule run
            // supplies the name with no id at all, and a recorded id goes stale
            // when the album is deleted and recreated. Resolve the name against
            // the server that received the upload (post-failover, same reason as
            // the wipe payload above) and prefer that answer.
            let resolved_album_id = if failed {
                None
            } else {
                resolve_album_id_by_name(
                    &request.server_url,
                    &api_key_for_finalization,
                    request.into_album.as_deref(),
                )
                .await
            };

            ImportJob {
                id: job_id_clone.clone(),
                status,
                progress,
                error,
                summary,
                awaiting_wipe_confirmation,
                pending_wipe_count,
                file_errors: file_errors.clone(),
                profile_id: profile.id.clone(),
                album_id: resolved_album_id.or_else(|| provisional_album_id.clone()),
            }
        };

        for fe in &file_errors {
            let _ = logs::append_log(
                "app.log",
                &format!(
                    "import_error job_id={} file={} reason={}",
                    job_id_clone, fe.file, fe.reason
                ),
            );
        }

        let _ = logs::append_log(
            "app.log",
            &format!(
                "import_complete job_id={} status={:?} uploaded={} total={} errors={}",
                update.id,
                update.status,
                update.progress.uploaded,
                update.progress.total,
                update.progress.errors
            ),
        );
        let _ = logs::rotate_recent_logs(5);
        // A cancel request that lands during finalization has already written
        // Cancelled; finalize_job refuses to move the job back out of it and
        // returns what is actually stored, so the log/history below record the
        // outcome the user was shown rather than the one this task computed.
        let update = finalize_job(update);
        let checkpoint_eligible =
            checkpoint_eligible && matches!(update.status, JobStatus::Completed);
        let status = match &update.status {
            JobStatus::Completed => RecordStatus::Completed,
            JobStatus::Cancelled => RecordStatus::Cancelled,
            _ => RecordStatus::Failed,
        };
        if let Err(err) = crate::services::store::append_history(
            &app_clone,
            crate::models::history::ImportRecord {
                id: update.id.clone(),
                started_at,
                finished_at: now_ms(),
                profile_id: profile.id.clone(),
                source_paths: record_source_paths.clone(),
                // The album this run actually landed in, resolved from the name
                // immich-go targeted, rather than whatever id the picker sent.
                album_ids: update.album_id.clone().into_iter().collect(),
                status,
                total: update.progress.total,
                uploaded: update.progress.uploaded,
                duplicates: update.progress.duplicates,
                errors: update.progress.errors,
                // Persist the request (source/options) so History can replay it.
                request: Some(history_request),
            },
            checkpoint_eligible,
        ) {
            let _ = logs::append_log(
                "app.log",
                &format!(
                    "import_history_persist_failed job_id={} error={err}",
                    update.id
                ),
            );
            let mut job_with_warning = update.clone();
            let warning = "Warning: import history could not be saved.";
            job_with_warning.summary = Some(match job_with_warning.summary.take() {
                Some(summary) => format!("{summary} {warning}"),
                None => warning.to_string(),
            });
            let _ = set_job(job_with_warning);
        }
        // Finalization is complete only after the history write and warning
        // update above, so shutdown may now observe both maps as clear.
        if let Ok(mut finalizing) = FINALIZING_IMPORTS.lock() {
            finalizing.remove(&job_id_clone);
        }
    });

    Ok(job_id)
}

/// Cap on how many files a forecast will hash in one pass; beyond this the
/// result is marked truncated so the UI can show a lower bound instead of
/// hashing an unbounded card on a "Check server" click.
const FORECAST_MAX_FILES: usize = 5000;

/// Read-only preflight: how many of the selected/scanned files the server
/// already holds vs. would upload. Reuses the SHA-1 + bulk-upload-check path;
/// safe to run repeatedly and never mutates anything.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn import_forecast(
    profile_id: String,
    source_paths: Vec<String>,
    select_files: Option<Vec<String>>,
    include_type: Option<String>,
    include_extensions: Vec<String>,
    exclude_extensions: Vec<String>,
) -> Result<wipe::ForecastResult, String> {
    let cancellation = Arc::new(AtomicBool::new(false));
    let previous = {
        let mut active = ACTIVE_FORECAST
            .lock()
            .map_err(|_| "Could not lock active forecast state".to_string())?;
        active.replace(cancellation.clone())
    };
    if let Some(previous) = previous {
        previous.store(true, Ordering::Relaxed);
    }
    let _forecast_guard = ActiveForecastGuard {
        cancellation: cancellation.clone(),
    };

    // Same path scope the preview commands enforce. This command opens and
    // SHA-1s every path the renderer names, then sends those hashes and the
    // absolute paths to the configured server. The approved-root set is
    // recorded from paths the renderer itself supplies to `scan_sources_stream`,
    // so a compromised renderer can self-authorize any path — this guard does
    // not defend against that. What it does catch is path confusion (a stale
    // or mistyped `source_paths` argument that no longer matches what was
    // actually scanned) and previewing anything that was never scanned in the
    // first place.
    for path in &source_paths {
        if !source_guard::is_within_approved(path) {
            return Err(format!(
                "Source is outside the chosen source folders: {path}"
            ));
        }
    }
    if let Some(files) = select_files.as_deref() {
        validate_selected_under_sources(files, &source_paths)?;
    }

    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::require_api_key(&profile_id)?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    if server_url.is_empty() {
        return Err("No reachable Immich server URL for this profile.".to_string());
    }

    // Apply the same type/extension filters immich-go would, so the forecast
    // counts the files that will actually upload (both filters are extension-based
    // on immich-go's side, so this matches). Date/only-new is intentionally NOT
    // applied here — it needs per-file EXIF — so the UI flags the estimate.
    let include_type = parse_include_type(include_type.as_deref())?;
    let include_extensions = normalize_extensions(&include_extensions);
    let exclude_extensions = normalize_extensions(&exclude_extensions);
    let keep = move |path: &str| -> bool {
        let ext = media_scanner::extension_of(path);
        if let Some(kind) = include_type.as_deref() {
            let is_video = media_scanner::is_video_ext(&ext);
            if (kind == "VIDEO") != is_video {
                return false;
            }
        }
        if !include_extensions.is_empty() && !include_extensions.iter().any(|e| e == &ext) {
            return false;
        }
        if exclude_extensions.iter().any(|e| e == &ext) {
            return false;
        }
        true
    };

    // Prefer an explicit selection; otherwise scan the sources (bounded) to find
    // candidate media. Scanning reads the filesystem, so keep it off the runtime.
    let (candidates, truncated, skipped_unreadable) = match select_files {
        Some(mut files) => {
            files.retain(|p| keep(p));
            let truncated = files.len() > FORECAST_MAX_FILES;
            files.truncate(FORECAST_MAX_FILES);
            (files, truncated, 0usize)
        }
        None => {
            let paths = collapse_overlapping_roots(source_paths);
            let scan_roots =
                media_scanner::acquire_scan_roots(media_scanner::ScanPurpose::Forecast, &paths)?;
            let scan_cancellation = cancellation.clone();
            tauri::async_runtime::spawn_blocking(move || {
                // This guard must drop on the walking task. Abandoning a blocked
                // `WalkDir` call is inherent, so one hung source can leak one
                // blocking-pool thread but cannot stack a thread per forecast.
                let _scan_roots = scan_roots;
                let deadline = Instant::now() + SCAN_DEADLINE;
                let mut files: Vec<String> = Vec::new();
                let mut seen = 0usize;
                let mut skipped = 0usize;
                for path in paths {
                    let source_path = PathBuf::from(&path);
                    // Propagate scan failures/timeouts instead of forecasting a
                    // partial (often empty) set as if it were complete.
                    let scan_skipped = media_scanner::scan_directory_streaming(
                        &source_path,
                        Some(scan_cancellation.as_ref()),
                        Some(deadline),
                        &mut |batch| {
                            for file in batch {
                                if !keep(&file.path) {
                                    continue;
                                }
                                seen += 1;
                                if files.len() < FORECAST_MAX_FILES {
                                    files.push(file.path);
                                }
                            }
                        },
                    )
                    .map_err(|e| format!("Could not scan {path}: {e}"))?;
                    skipped += scan_skipped;
                }
                // Truncated only when the cap actually discarded a candidate.
                Ok::<_, String>((files, seen > FORECAST_MAX_FILES, skipped))
            })
            .await
            .map_err(|e| format!("Scan task failed: {e}"))??
        }
    };

    let mut result = wipe::forecast_upload(
        &server_url,
        &api_key,
        &candidates,
        Some(cancellation.clone()),
    )
    .await?;
    result.truncated = result.truncated || truncated;
    result.unreadable += skipped_unreadable;
    Ok(result)
}

/// Best-effort lookup of the album id for an exact album name.
///
/// This only backs the "Open in Immich" deep link, so every failure returns
/// `None` and leaves the run's outcome untouched. An ambiguous name also returns
/// `None`: Immich permits duplicate album names, and immich-go's `--into-album`
/// gives no way to tell which one it used, so linking to a guess would send the
/// user to an album that may not hold the upload.
async fn resolve_album_id_by_name(
    server_url: &str,
    api_key: &str,
    name: Option<&str>,
) -> Option<String> {
    let name = name.map(str::trim).filter(|name| !name.is_empty())?;
    if server_url.is_empty() {
        return None;
    }
    let albums = ImmichClient::new(server_url, api_key)
        .list_albums(None)
        .await
        .ok()?;
    let mut matched = albums.into_iter().filter(|album| album.album_name == name);
    let first = matched.next()?;
    match matched.next() {
        Some(_) => None,
        None => Some(first.id),
    }
}

/// immich-go accepts only VIDEO or IMAGE for `--include-type`.
///
/// An unrecognized value is refused rather than dropped. Dropping it removes the
/// media-kind filter entirely, so a typo would upload the kinds the user
/// filtered out — and on a delete-after-import run, then delete them from the
/// card. Refusing keeps the run's semantics equal to what was asked for.
fn parse_include_type(value: Option<&str>) -> Result<Option<String>, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => match value.to_ascii_uppercase().as_str() {
            "VIDEO" => Ok(Some("VIDEO".to_string())),
            "IMAGE" => Ok(Some("IMAGE".to_string())),
            _ => Err(format!(
                "Unsupported media type filter: {value}. Expected \"VIDEO\" or \"IMAGE\"."
            )),
        },
    }
}

/// Normalize extensions to immich-go's leading-dot, lowercase form, dropping blanks.
fn normalize_extensions(exts: &[String]) -> Vec<String> {
    exts.iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{e}"))
        .collect()
}

/// Retain only confirmed files whose Trash move failed. Changed files stay out
/// of this retry payload because the verify-before-wipe safety gate kept them.
fn retry_pending_wipe(pending: PendingWipe, failed_paths: Vec<String>) -> Option<PendingWipe> {
    if failed_paths.is_empty() {
        return None;
    }
    Some(PendingWipe {
        paths: failed_paths,
        server_url: pending.server_url,
        api_key: pending.api_key,
    })
}

#[tauri::command]
pub async fn import_confirm_wipe(job_id: String, confirm: bool) -> Result<ImportJob, String> {
    let mut job = get_job(&job_id)?;

    if !job.awaiting_wipe_confirmation {
        return Err(format!("Job does not need wipe confirmation: {job_id}"));
    }

    let pending = PENDING_WIPE
        .lock()
        .map_err(|_| "Could not lock pending wipe state".to_string())?
        .remove(&job_id)
        .ok_or_else(|| format!("No pending wipe payload for job: {job_id}"))?;

    let pending_count = pending.paths.len();
    // When verification fails we keep every file AND leave the job actionable so
    // the user can retry once the server is reachable again (previously the
    // payload was dropped, making retry impossible).
    let mut retry_pending: Option<PendingWipe> = None;

    if confirm {
        match wipe::verify_uploaded(&pending.server_url, &pending.api_key, &pending.paths).await {
            Ok(verified) => {
                let confirmed_count = verified.confirmed.len();
                let unverified_count = verified.unverified.len();
                match tauri::async_runtime::spawn_blocking(move || {
                    wipe::wipe_files(&verified.confirmed)
                })
                .await
                {
                    Ok(wipe_result) => {
                        let kept = wipe_result.failed
                            + wipe_result.skipped
                            + wipe_result.changed
                            + unverified_count;
                        // A file kept because it changed after verification is the
                        // safety gate doing its job, so name it explicitly rather
                        // than folding it into an anonymous "kept" total.
                        let changed_note = if wipe_result.changed > 0 {
                            format!(
                                " {} changed after verification and were kept.",
                                wipe_result.changed
                            )
                        } else {
                            String::new()
                        };
                        job.summary = Some(format!(
                            "Verified {} of {} files on the server and deleted {}. Kept {} ({} not found on server).{}",
                            confirmed_count,
                            pending_count,
                            wipe_result.deleted,
                            kept,
                            unverified_count,
                            changed_note,
                        ));
                        job.error = if wipe_result.failed > 0 {
                            Some(format!(
                                "Wipe completed with errors: deleted={}, failed={}, skipped={}",
                                wipe_result.deleted, wipe_result.failed, wipe_result.skipped
                            ))
                        } else if wipe_result.changed > 0 {
                            Some(format!(
                                "{} file(s) changed after they were verified and were kept for safety.",
                                wipe_result.changed
                            ))
                        } else if unverified_count > 0 {
                            Some(format!(
                                "{unverified_count} file(s) were not found on the server and were kept for safety."
                            ))
                        } else if wipe_result.skipped > 0 {
                            // Now that the delete path has no extension gate, this
                            // can only mean the file left the card between the
                            // server check and the delete. Say so rather than
                            // folding it into an anonymous "kept" total.
                            Some(format!(
                                "{} file(s) were gone before they could be deleted.",
                                wipe_result.skipped
                            ))
                        } else {
                            None
                        };
                        let _ = logs::append_log(
                            "app.log",
                            &format!(
                                "import_wipe_verified job_id={} confirmed={} unverified={} deleted={} changed={} skipped={}",
                                job_id, confirmed_count, unverified_count, wipe_result.deleted, wipe_result.changed, wipe_result.skipped
                            ),
                        );
                        if wipe_result.failed > 0 {
                            retry_pending = retry_pending_wipe(pending, wipe_result.failed_paths);
                        }
                    }
                    Err(err) => {
                        job.summary = Some(
                            "Wipe worker stopped before completion. Source files were kept where possible — you can retry the wipe."
                                .to_string(),
                        );
                        job.error = Some(format!("Wipe task failed: {err}"));
                        let _ = logs::append_log(
                            "app.log",
                            &format!("import_wipe_task_failed job_id={job_id} error={err}"),
                        );
                        retry_pending = Some(pending);
                    }
                }
            }
            Err(err) => {
                job.summary = Some(format!(
                    "Could not verify uploads with the server. All {pending_count} files were kept — you can retry the wipe."
                ));
                job.error = Some(format!(
                    "Wipe verification failed: {err}. Source files kept for safety."
                ));
                let _ = logs::append_log(
                    "app.log",
                    &format!("import_wipe_verify_failed job_id={job_id} error={err}"),
                );
                retry_pending = Some(pending);
            }
        }
    } else {
        job.summary = Some(format!("Wipe skipped by user. {pending_count} files kept."));
    }
    if let Some(payload) = retry_pending {
        let retry_count = payload.paths.len();
        // Put the payload back so a later import_confirm_wipe can retry — but
        // only re-offer the prompt if that actually landed. A prompt with no
        // payload behind it cannot be answered, and an awaiting-wipe job
        // refuses both dismiss and eviction, so it would strand the card.
        let stored = match PENDING_WIPE.lock() {
            Ok(mut map) => {
                map.insert(job_id.clone(), payload);
                true
            }
            Err(_) => false,
        };
        let (awaiting, count) = wipe_prompt_state(true, stored, retry_count);
        job.awaiting_wipe_confirmation = awaiting;
        job.pending_wipe_count = count;
        if !stored {
            // The summary set above promises a retry of the wipe. Without a
            // stored payload there is nothing to retry, so replace it rather
            // than leave two contradictory sentences on the card.
            job.summary = Some(format!(
                "All {retry_count} files were kept. The delete list could not be saved, so this run cannot re-offer them."
            ));
            job.error = Some(PENDING_WIPE_STORE_ERROR.to_string());
        }
    } else {
        job.awaiting_wipe_confirmation = false;
        job.pending_wipe_count = 0;
    }
    set_job(job.clone())?;
    // `pending_count` is the size of the payload this confirmation acted on;
    // `retrying` reports how much of it stayed queued for a later attempt.
    let _ = logs::append_log(
        "app.log",
        &format!(
            "import_wipe_confirmed job_id={} confirm={} pending_count={} retrying={}",
            job_id, confirm, pending_count, job.pending_wipe_count
        ),
    );

    Ok(job)
}

#[tauri::command]
pub async fn scan_sources_stream(
    app: tauri::AppHandle,
    paths: Vec<String>,
    scan_id: String,
) -> Result<ScanSummary, String> {
    if paths.is_empty() {
        return Err("At least one path is required".to_string());
    }
    // The full selection defines the approved-root scope for this scan. Swap
    // it atomically so a concurrent path-scoped read never observes an empty
    // scope between clearing and recording the selected roots.
    crate::services::source_guard::replace_roots(&paths);
    let paths = collapse_overlapping_roots(paths);

    let cancellation = Arc::new(AtomicBool::new(false));
    let previous = {
        let mut active = ACTIVE_SCAN
            .lock()
            .map_err(|_| "Could not lock active scan state".to_string())?;
        active.replace(cancellation.clone())
    };
    // Stop the previous walk BEFORE claiming its roots. The claim waits for that
    // walk to return, and a walk that has not been told to stop never will.
    if let Some(previous) = previous {
        previous.store(true, Ordering::Relaxed);
    }
    let scan_roots = media_scanner::acquire_scan_roots(media_scanner::ScanPurpose::Scan, &paths)?;

    let deadline = Instant::now() + SCAN_DEADLINE;
    let scan_cancellation = cancellation.clone();
    let progress = Arc::new(Mutex::new(ScanSummary {
        status: ScanStatus::Complete,
        photo_count: 0,
        video_count: 0,
        total_size_bytes: 0,
        skipped_unreadable: 0,
    }));
    let scan_progress = progress.clone();
    let mut scan_task = tauri::async_runtime::spawn_blocking(move || {
        // This guard must drop on the walking task. Abandoning a blocked
        // `WalkDir` call is inherent, so one hung source can leak one
        // blocking-pool thread but cannot stack a thread per scan.
        let _scan_roots = scan_roots;
        let mut seen_file_paths = HashSet::new();

        for path in paths {
            let source_path = PathBuf::from(path);
            let progress_for_batch = scan_progress.clone();
            let app_for_batch = app.clone();
            let mut progress_error = None;
            let skipped = media_scanner::scan_directory_streaming(
                &source_path,
                Some(scan_cancellation.as_ref()),
                Some(deadline),
                &mut |files| {
                    if scan_cancellation.load(Ordering::Relaxed) {
                        return;
                    }

                    if progress_error.is_some() {
                        return;
                    }
                    let survivors: Vec<_> = files
                        .into_iter()
                        .filter(|file| {
                            let file_path = std::fs::canonicalize(&file.path)
                                .unwrap_or_else(|_| PathBuf::from(&file.path));
                            seen_file_paths.insert(file_path)
                        })
                        .collect();
                    let mut summary = match progress_for_batch.lock() {
                        Ok(summary) => summary,
                        Err(_) => {
                            progress_error = Some("Could not lock scan progress state".to_string());
                            return;
                        }
                    };
                    for file in &survivors {
                        summary.total_size_bytes += file.size_bytes;
                        if file.is_video {
                            summary.video_count += 1;
                        } else {
                            summary.photo_count += 1;
                        }
                    }
                    let payload = ScanProgress {
                        scan_id: scan_id.clone(),
                        files: survivors,
                        photo_count: summary.photo_count,
                        video_count: summary.video_count,
                        total_size_bytes: summary.total_size_bytes,
                        skipped_unreadable: summary.skipped_unreadable,
                    };
                    drop(summary);
                    let _ = app_for_batch.emit("scan-progress", &payload);
                },
            );

            if let Some(error) = progress_error {
                return Err(error);
            }
            match skipped {
                Ok(skipped) => {
                    let mut summary = scan_progress
                        .lock()
                        .map_err(|_| "Could not lock scan progress state".to_string())?;
                    summary.skipped_unreadable += skipped;
                }
                Err(media_scanner::ScanError::Cancelled) => {
                    let mut summary = scan_progress
                        .lock()
                        .map_err(|_| "Could not lock scan progress state".to_string())?
                        .clone();
                    summary.status = ScanStatus::Cancelled;
                    return Ok(summary);
                }
                Err(media_scanner::ScanError::TimedOut) => {
                    let mut summary = scan_progress
                        .lock()
                        .map_err(|_| "Could not lock scan progress state".to_string())?
                        .clone();
                    summary.status = ScanStatus::TimedOut;
                    return Ok(summary);
                }
                Err(media_scanner::ScanError::Failed(error)) => return Err(error),
            }
        }

        scan_progress
            .lock()
            .map(|summary| summary.clone())
            .map_err(|_| "Could not lock scan progress state".to_string())
    });

    let result = tokio::select! {
        joined = tokio::time::timeout(SCAN_DEADLINE, &mut scan_task) => match joined {
            Ok(result) => result.map_err(|error| format!("Scan task failed: {error}"))?,
            Err(_) => {
                cancellation.store(true, Ordering::Relaxed);
                let mut summary = progress
                    .lock()
                    .map_err(|_| "Could not lock scan progress state".to_string())?
                    .clone();
                summary.status = ScanStatus::TimedOut;
                Ok(summary)
            }
        },
        _ = async {
            while !cancellation.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } => {
            cancellation.store(true, Ordering::Relaxed);
            let mut summary = progress
                .lock()
                .map_err(|_| "Could not lock scan progress state".to_string())?
                .clone();
            summary.status = ScanStatus::Cancelled;
            Ok(summary)
        },
    };

    if let Ok(mut active) = ACTIVE_SCAN.lock() {
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &cancellation))
        {
            active.take();
        }
    }

    result
}

#[tauri::command]
pub async fn scan_cancel() -> Result<(), String> {
    {
        let active = ACTIVE_SCAN
            .lock()
            .map_err(|_| "Could not lock active scan state".to_string())?;
        if let Some(cancellation) = active.as_ref() {
            cancellation.store(true, Ordering::Relaxed);
        }
    }
    {
        let active = ACTIVE_FORECAST
            .lock()
            .map_err(|_| "Could not lock active forecast state".to_string())?;
        if let Some(cancellation) = active.as_ref() {
            cancellation.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn import_cancel(job_id: String) -> Result<(), String> {
    let mut job = get_job(&job_id)?;
    match &job.status {
        JobStatus::Running => {
            let running = RUNNING_IMPORTS
                .lock()
                .map_err(|_| "Could not lock running imports state".to_string())?;
            let flag = running
                .get(&job_id)
                .ok_or_else(|| format!("Import is no longer running: {job_id}"))?;
            flag.store(true, Ordering::Relaxed);
        }
        JobStatus::Pending => {}
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled => {
            return Err(format!("{TERMINAL_CANCEL_ERROR}: {job_id}"));
        }
    }

    job.status = JobStatus::Cancelled;
    job.awaiting_wipe_confirmation = false;
    job.pending_wipe_count = 0;
    job.error = None;
    job.summary = Some("Import cancelled by user.".to_string());
    if let Ok(mut pending) = PENDING_WIPE.lock() {
        pending.remove(&job_id);
    }
    set_job(job)
}

/// Wait for a job to reach a terminal status, for its run to exit, and for
/// post-run finalization to finish, then return the final job.
///
/// A terminal *status* is not the same thing as a terminal *worker*.
/// `import_cancel` writes `Cancelled` the instant it is asked to stop, while
/// the `import_start` task spawned for that job keeps running through the run
/// and its shutdown work. `RUNNING_IMPORTS` covers the run itself. The worker
/// removes that entry before reading the run log, registering the wipe payload,
/// writing the terminal state, and appending history; `FINALIZING_IMPORTS`
/// covers that post-run finalization phase. Quitting the app before both sets
/// clear can kill the sidecar mid-upload or lose cleanup and the history record.
/// This command gives a close handler something real to wait on instead of
/// guessing with a fixed timer: `Ok` means the status is terminal and both
/// worker-state maps are clear, so it is safe to exit.
///
/// This is a shutdown path, not a hot loop, so a coarse 100ms poll is fine —
/// there is no cheaper event to wait on since the run and finalization phases
/// are only observable through their respective maps.
#[tauri::command]
pub async fn import_await_terminal(job_id: String, timeout_ms: u64) -> Result<ImportJob, String> {
    // `timeout_ms` arrives over IPC and must not be trusted to be in range:
    // `Instant`'s `Add` panics on overflow, and this runs before `get_job`, so
    // an untrusted huge value would panic the command task for any job id
    // instead of surfacing as an error. Production only ever passes 30_000.
    let timeout_ms = timeout_ms.min(MAX_AWAIT_TERMINAL_TIMEOUT_MS);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let job = get_job(&job_id)?;
        let worker_alive = RUNNING_IMPORTS
            .lock()
            .map_err(|_| "Could not lock running imports state".to_string())?
            .contains_key(&job_id);
        let finalizing = FINALIZING_IMPORTS
            .lock()
            .map_err(|_| "Could not lock finalizing imports state".to_string())?
            .contains(&job_id);
        if is_terminal(&job.status) && !worker_alive && !finalizing {
            return Ok(job);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for import {job_id} to finish shutting down; the import worker is still shutting down"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tauri::command]
pub async fn import_list_jobs() -> Result<Vec<ImportJob>, String> {
    let jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    Ok(jobs.clone())
}

#[tauri::command]
pub async fn import_retry(app: tauri::AppHandle, job_id: String) -> Result<String, String> {
    let job = get_job(&job_id)?;
    if !matches!(&job.status, JobStatus::Failed | JobStatus::Cancelled) {
        return Err(format!(
            "Only failed or cancelled imports can be retried: {job_id}"
        ));
    }
    let input = {
        let inputs = JOB_INPUTS
            .lock()
            .map_err(|_| "Could not lock import inputs".to_string())?;
        inputs.get(&job_id).cloned()
    };
    let input = input.ok_or_else(|| format!("No saved input to retry for job: {job_id}"))?;
    import_start(app, input).await
}

#[tauri::command]
pub async fn import_dismiss(job_id: String) -> Result<Vec<ImportJob>, String> {
    {
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "Could not lock import job state".to_string())?;
        if let Some(job) = jobs.iter().find(|j| j.id == job_id) {
            if matches!(&job.status, JobStatus::Running | JobStatus::Pending) {
                return Err("Cannot dismiss a running import".to_string());
            }
            // The pending payload is the only handle on verified originals, so
            // answer the delete prompt before dismissing this terminal job.
            if job.awaiting_wipe_confirmation {
                return Err(
                    "Cannot dismiss an import while wipe confirmation is pending; answer the delete prompt first"
                        .to_string(),
                );
            }
        }
        jobs.retain(|j| j.id != job_id);
    }
    if let Ok(mut inputs) = JOB_INPUTS.lock() {
        inputs.remove(&job_id);
    }
    if let Ok(mut pending) = PENDING_WIPE.lock() {
        pending.remove(&job_id);
    }
    import_list_jobs().await
}

/// Whether "Clear finished" may drop this job.
///
/// A job still awaiting wipe confirmation is not finished from the user's point
/// of view. Removing it also drops its `PENDING_WIPE` payload, which is the only
/// handle on the verified-uploaded originals — the sources then stay on the card
/// with no way to reach the confirmation prompt again.
fn is_clearable(job: &ImportJob) -> bool {
    !job.awaiting_wipe_confirmation
        && matches!(
            &job.status,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
}

#[tauri::command]
pub async fn import_clear_finished() -> Result<Vec<ImportJob>, String> {
    let removed_ids: Vec<String> = {
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "Could not lock import job state".to_string())?;
        let removed: Vec<String> = jobs
            .iter()
            .filter(|job| is_clearable(job))
            .map(|job| job.id.clone())
            .collect();
        jobs.retain(|job| !is_clearable(job));
        removed
    };
    if let Ok(mut inputs) = JOB_INPUTS.lock() {
        for id in &removed_ids {
            inputs.remove(id);
        }
    }
    if let Ok(mut pending) = PENDING_WIPE.lock() {
        for id in &removed_ids {
            pending.remove(id);
        }
    }
    import_list_jobs().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_failed(o: &RunOutcome) -> bool {
        matches!(o.status, JobStatus::Failed)
    }

    #[test]
    fn collapse_overlapping_roots_keeps_disjoint_ancestors_once() {
        let temp_dir =
            std::env::temp_dir().join(format!("immich-shuttle-collapse-roots-{}", Uuid::new_v4()));
        let parent = temp_dir.join("Photos");
        let child = parent.join("2026");
        let disjoint = temp_dir.join("Documents");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&disjoint).unwrap();

        let collapsed = collapse_overlapping_roots(vec![
            child.to_string_lossy().into_owned(),
            parent.to_string_lossy().into_owned(),
            disjoint.to_string_lossy().into_owned(),
            parent.to_string_lossy().into_owned(),
        ]);

        assert_eq!(
            collapsed,
            vec![
                parent
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                disjoint
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
        std::fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn nothing_landed_and_bad_exit_is_failed_not_wipe_eligible() {
        let o = classify_completed_run(0, 0, true, 0, false, 3, 0);
        assert!(is_failed(&o));
        assert!(!o.wipe_eligible, "a failed run must never be wipe-eligible");
        assert!(!o.checkpoint_eligible);
    }

    #[test]
    fn nothing_landed_with_file_errors_is_failed() {
        let o = classify_completed_run(0, 0, false, 2, false, 3, 0);
        assert!(is_failed(&o));
        assert!(!o.wipe_eligible);
        assert!(!o.checkpoint_eligible);
    }

    #[test]
    fn uploads_present_succeed_despite_errors_and_bad_exit() {
        // A partial run that uploaded something is a success even with per-file
        // errors and a non-zero exit; deletion of the originals stays eligible.
        let o = classify_completed_run(5, 0, true, 4, false, 5, 0);
        assert!(!is_failed(&o));
        assert!(o.wipe_eligible);
        assert!(
            !o.checkpoint_eligible,
            "a partial run must not raise the only-new date floor"
        );
    }

    #[test]
    fn duplicates_only_count_as_landed() {
        // Everything was already on the server (all duplicates): success, and the
        // originals are still eligible for deletion.
        let o = classify_completed_run(0, 7, false, 0, false, 7, 0);
        assert!(!is_failed(&o));
        assert!(o.wipe_eligible);
        assert!(
            o.checkpoint_eligible,
            "the server holds every file, so the source is fully imported"
        );
    }

    #[test]
    fn keep_files_blocks_wipe_on_success() {
        let o = classify_completed_run(5, 0, false, 0, true, 5, 0);
        assert!(!is_failed(&o));
        assert!(!o.wipe_eligible, "keep-files must suppress deletion");
        assert!(o.checkpoint_eligible, "keeping files is not a failure");
    }

    #[test]
    fn staged_run_with_no_uploaded_files_has_no_wipe_candidates() {
        let tmp = std::env::temp_dir().join(format!("import-staged-empty-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("photo.jpg");
        std::fs::write(&source, b"photo").unwrap();
        let selected = vec![source.to_string_lossy().into_owned()];
        let mut staged = staging::create_staging_dir(&selected, None).unwrap();
        let invocation_root = staged.path().to_path_buf();
        let links = staged.take_links();
        let run = crate::services::stdout_parser::parse_run_progress(
            "",
            &[invocation_root.to_string_lossy().into_owned()],
        );
        staging::cleanup_staging_dir(staged);

        let (completed, dropped) = translate_staged_paths(run.completed_paths, &links);
        assert!(completed.is_empty());
        assert_eq!(dropped, 0);
        let outcome = classify_completed_run(
            run.progress.uploaded,
            run.progress.duplicates,
            false,
            0,
            false,
            completed.len(),
            0,
        );
        assert!(!outcome.wipe_eligible);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn staged_run_with_landed_files_has_exactly_their_originals_as_candidates() {
        let tmp = std::env::temp_dir().join(format!("import-staged-landed-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let selected: Vec<String> = ["one.jpg", "two.jpg", "three.jpg"]
            .into_iter()
            .map(|name| {
                let path = tmp.join(name);
                std::fs::write(&path, name.as_bytes()).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let mut staged = staging::create_staging_dir(&selected, None).unwrap();
        let invocation_root = staged.path().to_path_buf();
        let links = staged.take_links();
        let log = links
            .entries()
            .iter()
            .map(|(destination, _)| {
                let relative = destination.strip_prefix(&invocation_root).unwrap();
                format!(
                    "2026-06-24 16:10:00 INF uploaded successfully file={}:{}\n",
                    invocation_root.display(),
                    relative.display()
                )
            })
            .collect::<String>();
        let run = crate::services::stdout_parser::parse_run_progress(
            &log,
            &[invocation_root.to_string_lossy().into_owned()],
        );
        staging::cleanup_staging_dir(staged);

        let (completed, dropped) = translate_staged_paths(run.completed_paths, &links);
        assert_eq!(completed, selected);
        assert_eq!(dropped, 0);
        let outcome = classify_completed_run(
            run.progress.uploaded,
            run.progress.duplicates,
            false,
            0,
            false,
            completed.len(),
            0,
        );
        assert!(outcome.wipe_eligible);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn staged_file_errors_resolve_to_original_paths() {
        let tmp = std::env::temp_dir().join(format!("import-staged-errors-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("failed.jpg");
        std::fs::write(&source, b"failed").unwrap();
        let selected = vec![source.to_string_lossy().into_owned()];
        let mut staged = staging::create_staging_dir(&selected, None).unwrap();
        let invocation_root = staged.path().to_path_buf();
        let links = staged.take_links();
        let (destination, _) = &links.entries()[0];
        let relative = destination.strip_prefix(&invocation_root).unwrap();
        let log = format!(
            "2026-06-24 16:10:00 ERR server error file={}:{} error=upload failed\n",
            invocation_root.display(),
            relative.display()
        );
        let parsed = crate::services::stdout_parser::parse_error_log(
            &log,
            &[invocation_root.to_string_lossy().into_owned()],
        );
        staging::cleanup_staging_dir(staged);

        let (errors, dropped) = translate_staged_file_errors(parsed, &links);
        assert_eq!(dropped, 0);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file, selected[0]);
        assert!(!errors[0].file.contains("immich-shuttle-stage-"));
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn unreadable_run_log_returns_a_visible_error() {
        let (_, error) = read_run_log(Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "permission denied",
        )));
        assert_eq!(
            error.as_deref(),
            Some("Could not read import run log: permission denied")
        );
    }

    #[test]
    fn no_completed_paths_blocks_wipe() {
        // Success but nothing to delete (e.g. immich-go reported no completed
        // local paths): not wipe-eligible, so we never delete on an empty set.
        let o = classify_completed_run(0, 3, false, 0, false, 0, 0);
        assert!(!is_failed(&o));
        assert!(!o.wipe_eligible);
    }

    #[test]
    fn tallies_without_contained_paths_do_not_earn_the_checkpoint() {
        // FORGED-CHECKPOINT: uploaded/duplicates are resolved against
        // invocation roots by string matching alone, with no filesystem
        // check, so a forged `file=` record spelled under a root but naming a
        // file that never existed can inflate these tallies without ever
        // surviving `retain_paths_under_sources` (which canonicalizes and so
        // requires the file to actually exist). completed_paths_len is that
        // survived-containment count, so it must gate the checkpoint too.
        let o = classify_completed_run(5, 0, false, 0, false, 0, 0);
        assert!(!is_failed(&o), "uploads landed, so this is not a failure");
        assert!(!o.wipe_eligible, "nothing survived containment to delete");
        assert!(
            !o.checkpoint_eligible,
            "a tally with no contained-path evidence must not raise the date floor"
        );
    }

    /// An empty card, or filters that excluded every file, is not an error — but it
    /// is no evidence the source was imported either. Advancing the checkpoint here
    /// would set a date floor of "now" and permanently hide any media added later
    /// with an older capture date.
    #[test]
    fn a_zero_asset_run_completes_without_earning_the_checkpoint() {
        let o = classify_completed_run(0, 0, false, 0, false, 0, 0);
        assert!(
            !is_failed(&o),
            "nothing went wrong, so this is not a failure"
        );
        assert!(!o.wipe_eligible);
        assert!(
            !o.checkpoint_eligible,
            "a run that landed nothing must not raise the date floor"
        );
    }

    /// A source immich-go could not enumerate is reported only as an aggregate ERR
    /// line with no `file=`. Files from that source were never seen, so the
    /// checkpoint must not advance past them even though other sources succeeded.
    #[test]
    fn a_scan_error_blocks_the_checkpoint_even_when_uploads_succeeded() {
        let o = classify_completed_run(9, 0, false, 0, false, 9, 1);
        assert!(!is_failed(&o), "the files that did upload still count");
        assert!(o.wipe_eligible, "verified uploads remain deletable");
        assert!(
            !o.checkpoint_eligible,
            "an unreadable source must not be marked fully imported"
        );
    }

    #[test]
    fn a_clean_full_run_earns_the_checkpoint() {
        let o = classify_completed_run(12, 3, false, 0, false, 15, 0);
        assert!(!is_failed(&o));
        assert!(o.wipe_eligible);
        assert!(o.checkpoint_eligible);
    }

    fn terminal_job(id: &str, awaiting_wipe_confirmation: bool) -> ImportJob {
        ImportJob {
            id: id.to_string(),
            status: JobStatus::Completed,
            progress: JobProgress::default(),
            error: None,
            summary: None,
            awaiting_wipe_confirmation,
            pending_wipe_count: if awaiting_wipe_confirmation { 3 } else { 0 },
            file_errors: Vec::new(),
            profile_id: "p1".to_string(),
            album_id: None,
        }
    }

    fn lock_jobs() -> std::sync::MutexGuard<'static, Vec<ImportJob>> {
        JOBS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_running() -> std::sync::MutexGuard<'static, HashMap<String, Arc<AtomicBool>>> {
        RUNNING_IMPORTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_finalizing() -> std::sync::MutexGuard<'static, HashSet<String>> {
        FINALIZING_IMPORTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    /// Dismissing a terminal job must not drop the payload that can still
    /// delete verified originals.
    #[test]
    fn dismiss_refuses_a_job_awaiting_wipe_confirmation_and_keeps_payload() {
        let job_id = format!("dismiss-awaiting-{}", Uuid::new_v4());
        lock_jobs().push(terminal_job(&job_id, true));
        {
            let mut pending = PENDING_WIPE.lock().unwrap();
            pending.insert(
                job_id.clone(),
                PendingWipe {
                    paths: vec!["/tmp/photo.jpg".to_string()],
                    server_url: "https://example.invalid".to_string(),
                    api_key: "key".to_string(),
                },
            );
        }

        let err = tauri::async_runtime::block_on(import_dismiss(job_id.clone())).unwrap_err();
        assert!(
            err.contains("answer the delete prompt first"),
            "the user must be told how to clear the pending wipe: {err}"
        );
        assert!(get_job(&job_id).is_ok());
        assert!(PENDING_WIPE.lock().unwrap().contains_key(&job_id));

        lock_jobs().retain(|job| job.id != job_id);
        PENDING_WIPE.lock().unwrap().remove(&job_id);
    }

    #[test]
    fn dismiss_succeeds_for_a_finished_job_without_pending_wipe() {
        let job_id = format!("dismiss-finished-{}", Uuid::new_v4());
        lock_jobs().push(terminal_job(&job_id, false));

        let jobs = tauri::async_runtime::block_on(import_dismiss(job_id.clone()))
            .expect("a finished job without a wipe prompt is dismissible");
        assert!(!jobs.iter().any(|job| job.id == job_id));
        assert!(get_job(&job_id).is_err());
    }
    /// A worker panic must not leave shutdown waiting forever, while the saved
    /// input remains available for `import_retry`.
    #[test]
    fn worker_guard_marks_a_panicking_running_job_failed_and_keeps_retry_input() {
        let job_id = format!("worker-guard-{}", Uuid::new_v4());
        let input = ImportInput {
            profile_id: "profile".to_string(),
            source_paths: vec!["/tmp/source".to_string()],
            album_ids: Vec::new(),
            keep_files: false,
            stack_raw_jpeg: false,
            stack_burst: false,
            date_range: None,
            concurrent_tasks: None,
            select_files: None,
            into_album: None,
            organization: Organization::SingleAlbum,
            on_errors: None,
            overwrite: false,
            tags: Vec::new(),
            session_tag: false,
            include_type: None,
            include_extensions: Vec::new(),
            exclude_extensions: Vec::new(),
        };
        JOB_INPUTS.lock().unwrap().insert(job_id.clone(), input);
        let mut running_job = terminal_job(&job_id, false);
        running_job.status = JobStatus::Running;
        lock_jobs().push(running_job);
        lock_running().insert(job_id.clone(), Arc::new(AtomicBool::new(false)));
        lock_finalizing().insert(job_id.clone());

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = ImportWorkerGuard::new(job_id.clone());
            panic!("simulate worker panic");
        }));
        assert!(panic.is_err());
        assert!(!lock_running().contains_key(&job_id));
        assert!(!lock_finalizing().contains(&job_id));
        let failed = get_job(&job_id).expect("the panicking worker must publish a terminal job");
        assert!(matches!(failed.status, JobStatus::Failed));
        assert_eq!(
            failed.error.as_deref(),
            Some(IMPORT_WORKER_PANIC_ERROR),
            "panic cleanup must use the stable internal error"
        );
        assert_eq!(failed.progress.errors, 1);
        assert!(!failed.awaiting_wipe_confirmation);
        assert_eq!(failed.pending_wipe_count, 0);
        // No assertion on `has_active_import()`: it is process-global and these
        // tests run in parallel with siblings that publish their own jobs, so it
        // would be testing the other tests. `Failed` above plus the two liveness
        // maps are this job's whole contribution to admission, and asserting
        // `!is_active` on the same status it just matched would prove nothing.
        let mut late_completion = failed.clone();
        late_completion.status = JobStatus::Completed;
        late_completion.error = None;
        let preserved = finalize_job(late_completion);
        assert!(matches!(preserved.status, JobStatus::Failed));
        assert!(JOB_INPUTS.lock().unwrap().contains_key(&job_id));
        lock_jobs().retain(|job| job.id != job_id);
        JOB_INPUTS.lock().unwrap().remove(&job_id);
    }

    #[test]
    fn failed_pending_wipe_insertion_never_publishes_a_wipe_prompt() {
        let (awaiting, count) = wipe_prompt_state(true, false, 3);
        assert!(!awaiting);
        assert_eq!(count, 0);

        let (awaiting, count) = wipe_prompt_state(true, true, 3);
        assert!(awaiting, "a stored payload still publishes the prompt");
        assert_eq!(count, 3);
    }

    #[test]
    fn partial_wipe_retry_payload_contains_only_failed_verified_files() {
        let pending = PendingWipe {
            paths: vec![
                "/tmp/deleted.jpg".to_string(),
                "/tmp/failed.jpg".to_string(),
                "/tmp/changed.jpg".to_string(),
                "/tmp/unverified.jpg".to_string(),
            ],
            server_url: "https://example.invalid".to_string(),
            api_key: "key".to_string(),
        };
        let result = wipe::WipeResult {
            deleted: 1,
            failed: 1,
            skipped: 1,
            changed: 1,
            failed_paths: vec!["/tmp/failed.jpg".to_string()],
            errors: vec!["failed".to_string()],
        };

        let retry = retry_pending_wipe(pending, result.failed_paths).expect("failed file retries");
        assert_eq!(retry.paths, vec!["/tmp/failed.jpg"]);
    }

    /// "Clear finished" must not silently discard the wipe prompt: dropping the
    /// job drops its PENDING_WIPE payload, stranding verified-uploaded originals
    /// on the card with no way back to the confirmation.
    #[test]
    fn clear_finished_keeps_jobs_awaiting_wipe_confirmation() {
        assert!(is_clearable(&terminal_job("done", false)));
        assert!(!is_clearable(&terminal_job("awaiting", true)));

        let mut pending = terminal_job("cancelled-awaiting", true);
        pending.status = JobStatus::Cancelled;
        assert!(!is_clearable(&pending));
    }

    /// The renderer hands `import_forecast` raw paths and the command opens and
    /// hashes every one of them, so it must refuse anything the user never chose
    /// as a source — the same scope `preview_thumbnails` and `import_start`
    /// enforce. The bogus profile id proves the check runs before any profile,
    /// keychain, or filesystem work.
    #[test]
    fn import_forecast_refuses_sources_outside_the_approved_scope() {
        let unapproved =
            std::env::temp_dir().join(format!("immich-shuttle-unapproved-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&unapproved).unwrap();

        let err = tauri::async_runtime::block_on(import_forecast(
            "no-such-profile".to_string(),
            vec![unapproved.to_string_lossy().into_owned()],
            None,
            None,
            Vec::new(),
            Vec::new(),
        ))
        .unwrap_err();

        assert!(
            err.contains("outside the chosen source folders"),
            "expected a source-scope rejection, got: {err}"
        );

        std::fs::remove_dir_all(&unapproved).unwrap();
    }

    /// Isolates the `select_files` branch of the approved-scope guard, which
    /// `import_forecast_refuses_sources_outside_the_approved_scope` above does
    /// not exercise (it passes `select_files: None`). Passing zero
    /// `source_paths` skips the `source_paths` loop entirely — that loop
    /// checks the process-global `APPROVED_ROOTS`, which is also mutated by
    /// `source_guard`'s own tests, so asserting it would pass is
    /// order-dependent. `validate_selected_under_sources` instead checks
    /// `select_files` against the `source_paths` parameter directly, so this
    /// negative case needs none of that global state.
    #[test]
    fn import_forecast_refuses_selected_files_outside_the_approved_scope() {
        let unapproved =
            std::env::temp_dir().join(format!("immich-shuttle-select-outside-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&unapproved).unwrap();
        let outside_file = unapproved.join("photo.jpg");
        std::fs::write(&outside_file, b"x").unwrap();

        let err = tauri::async_runtime::block_on(import_forecast(
            "no-such-profile".to_string(),
            Vec::new(),
            Some(vec![outside_file.to_string_lossy().into_owned()]),
            None,
            Vec::new(),
            Vec::new(),
        ))
        .unwrap_err();

        assert!(
            err.contains("outside the chosen source folders"),
            "expected a select_files-scope rejection, got: {err}"
        );

        std::fs::remove_dir_all(&unapproved).unwrap();
    }

    /// The whole point of `import_await_terminal`: a terminal STATUS is not a
    /// terminal WORKER. `import_cancel` publishes `Cancelled` immediately while
    /// the worker is still resolving a server, cleaning staging, or waiting for
    /// the sidecar to die — and the close handler quits on this call returning.
    /// Drop the `RUNNING_IMPORTS` half of the condition and this test fails,
    /// which is exactly the bug that let the app exit mid-upload.
    #[test]
    fn await_terminal_holds_until_the_worker_is_gone_not_just_the_status() {
        let job_id = format!("await-terminal-{}", Uuid::new_v4());
        let cancel_flag = Arc::new(AtomicBool::new(true));

        // These are the same process-global maps the worker uses; recover from
        // poisoning the way every other holder in this crate does rather than
        // letting an unrelated failed test cascade into this one.

        {
            let mut job = terminal_job(&job_id, false);
            job.status = JobStatus::Cancelled;
            lock_jobs().push(job);
            lock_running().insert(job_id.clone(), cancel_flag);
        }

        // Status is already terminal, but the worker is still registered.
        let err =
            tauri::async_runtime::block_on(import_await_terminal(job_id.clone(), 150)).unwrap_err();
        assert!(
            err.contains("shutting down"),
            "a live worker must keep the caller waiting, got: {err}"
        );

        // The worker deregisters itself on its way out; only then may we quit.
        lock_running().remove(&job_id);
        let job = tauri::async_runtime::block_on(import_await_terminal(job_id.clone(), 2_000))
            .expect("a terminal job with no live worker must resolve");
        assert!(matches!(job.status, JobStatus::Cancelled));

        lock_jobs().retain(|j| j.id != job_id);
    }

    /// The worker deregisters from `RUNNING_IMPORTS` before it reads the run log
    /// and appends history. Without the finalizing half of the condition, the
    /// app can quit mid-finalization and lose the run's history record.
    #[test]
    fn await_terminal_waits_for_the_finalizing_phase_too() {
        let job_id = format!("await-finalizing-{}", Uuid::new_v4());
        let mut job = terminal_job(&job_id, false);
        job.status = JobStatus::Cancelled;
        lock_jobs().push(job);
        lock_running().remove(&job_id);
        lock_finalizing().insert(job_id.clone());

        let err =
            tauri::async_runtime::block_on(import_await_terminal(job_id.clone(), 150)).unwrap_err();
        assert!(
            err.contains("shutting down"),
            "a finalizing worker must keep the caller waiting, got: {err}"
        );

        lock_finalizing().remove(&job_id);
        let job = tauri::async_runtime::block_on(import_await_terminal(job_id.clone(), 2_000))
            .expect("a terminal job with no finalizing worker must resolve");
        assert!(matches!(job.status, JobStatus::Cancelled));
        lock_jobs().retain(|j| j.id != job_id);
    }

    /// Eviction must preserve a terminal job that still owns a wipe prompt:
    /// dropping it also drops the payload needed to confirm deletion.
    #[test]
    fn eviction_keeps_a_job_awaiting_wipe_confirmation() {
        let awaiting_id = format!("eviction-awaiting-{}", Uuid::new_v4());
        let mut jobs = Vec::with_capacity(MAX_RETAINED_TERMINAL_JOBS + 1);
        jobs.push(terminal_job(&awaiting_id, true));
        for index in 0..MAX_RETAINED_TERMINAL_JOBS {
            jobs.push(terminal_job(
                &format!("eviction-clearable-{index}-{}", Uuid::new_v4()),
                false,
            ));
        }

        let evicted = evict_old_terminal_jobs(&mut jobs);
        assert!(
            !evicted.contains(&awaiting_id),
            "eviction must not drop an unanswered wipe prompt"
        );
        assert!(
            jobs.iter().any(|job| job.id == awaiting_id),
            "the job awaiting wipe confirmation must remain in the local vector"
        );
    }

    /// A staging failure can arrive after cancellation publishes `Cancelled`.
    /// `finalize_job` must preserve that state instead of reviving the run as
    /// `Failed`, which would hide the cancellation from the user.
    #[test]
    fn a_cancelled_job_survives_a_staging_failure_write() {
        let job_id = format!("staging-cancelled-{}", Uuid::new_v4());
        let mut cancelled = terminal_job(&job_id, false);
        cancelled.status = JobStatus::Cancelled;
        lock_jobs().push(cancelled);

        let mut failed = terminal_job(&job_id, false);
        failed.status = JobStatus::Failed;
        failed.error = Some("Staging task failed".to_string());
        let returned = finalize_job(failed);
        assert!(matches!(returned.status, JobStatus::Cancelled));
        let stored = get_job(&job_id).expect("the cancelled job must remain stored");
        assert!(matches!(stored.status, JobStatus::Cancelled));

        lock_jobs().retain(|job| job.id != job_id);
        if let Ok(mut pending) = PENDING_WIPE.lock() {
            pending.remove(&job_id);
        }
    }

    #[test]
    fn await_terminal_reports_an_unknown_job() {
        let err =
            tauri::async_runtime::block_on(import_await_terminal("no-such-job".to_string(), 50))
                .unwrap_err();
        assert!(err.contains("no-such-job"), "got: {err}");
    }

    #[test]
    fn await_terminal_clamps_a_huge_timeout_instead_of_panicking() {
        // AWAIT-TERMINAL-PANIC: `Instant::now() + Duration::from_millis(timeout_ms)`
        // panics on overflow, and this ran before `get_job` — so a renderer
        // passing a large u64 over IPC panicked the command task for any job
        // id. Assert the clamp lands before that add, not that the wait
        // actually elapses: the unknown job id makes get_job fail on the very
        // first loop iteration, so this returns almost instantly either way.
        let err = tauri::async_runtime::block_on(import_await_terminal(
            "no-such-job".to_string(),
            u64::MAX,
        ))
        .unwrap_err();
        assert!(err.contains("no-such-job"), "got: {err}");
    }

    #[test]
    fn cross_boundary_error_texts_keep_their_frontend_contract() {
        // `shutdown.ts` substring-matches both of these to decide whether app
        // quit may proceed. A reword here silently changes quit behaviour, and
        // shutdown.test.ts cannot catch it because it builds its own strings.
        assert_eq!(JOB_NOT_FOUND_ERROR, "Job not found:");
        assert_eq!(TERMINAL_CANCEL_ERROR, "Cannot cancel a terminal import");

        let err = get_job("no-such-job").unwrap_err();
        assert!(
            err.starts_with(JOB_NOT_FOUND_ERROR),
            "the frontend matches this prefix, got: {err}"
        );
    }

    #[test]
    fn an_unknown_media_type_filter_is_refused_not_dropped() {
        // Dropping it removes the filter, so a typo would upload the kinds the
        // user excluded — and delete them from the card on a wipe run.
        assert_eq!(
            parse_include_type(Some("VIDEO")).unwrap(),
            Some("VIDEO".to_string())
        );
        assert_eq!(
            parse_include_type(Some(" image ")).unwrap(),
            Some("IMAGE".to_string())
        );
        assert_eq!(parse_include_type(None).unwrap(), None);
        assert_eq!(parse_include_type(Some("  ")).unwrap(), None);

        let err = parse_include_type(Some("VIDO")).unwrap_err();
        assert!(
            err.contains("VIDO"),
            "the rejection must name the value, got: {err}"
        );
    }
}
