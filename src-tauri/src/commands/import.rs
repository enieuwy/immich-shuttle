use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
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
        job::{ImportInput, ImportJob, JobProgress, JobStatus, Organization},
        media::{ScanProgress, ScanSummary},
    },
    services::{
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

static IMPORT_START_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static ACTIVE_SCAN: LazyLock<Mutex<Option<Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(None));

struct PendingWipe {
    paths: Vec<String>,
    server_url: String,
    api_key: String,
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

fn get_job(job_id: &str) -> Result<ImportJob, String> {
    let jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    jobs.iter()
        .find(|j| j.id == job_id)
        .cloned()
        .ok_or_else(|| format!("Job not found: {job_id}"))
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
    // did survive containment (via `retain_paths_under_sources`, or via
    // `validate_selected_under_sources` at admission for a staged import — see
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

#[tauri::command]
pub async fn import_start(app: tauri::AppHandle, input: ImportInput) -> Result<String, String> {
    if input.source_paths.is_empty() {
        return Err("At least one source path is required".to_string());
    }

    let profile = profile_store::get_profile(&input.profile_id)?;
    let api_key = keychain::get_api_key(&input.profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {}", input.profile_id))?;

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
    // `on_errors` arrives over IPC; accept only immich-go's known modes or a
    // non-negative integer count, else drop it (leaving immich-go's default).
    let on_errors = input.on_errors.as_deref().and_then(|v| {
        let v = v.trim();
        if v == "stop" || v == "continue" || v.parse::<u32>().is_ok() {
            Some(v.to_string())
        } else {
            None
        }
    });
    let overwrite = input.overwrite;
    let tags: Vec<String> = input
        .tags
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let session_tag = input.session_tag;
    let include_type = sanitize_include_type(input.include_type.as_deref());
    let include_extensions = normalize_extensions(&input.include_extensions);
    let exclude_extensions = normalize_extensions(&input.exclude_extensions);
    // The "Open in Immich" deep-link points at a specific album only when the run
    // actually targets one: SingleAlbum mode AND a non-empty --into-album name
    // (folder/tag modes fan out; an unresolved selection sends no into_album, so
    // the card must not claim an album the upload never populated).
    let into_album_active = into_album
        .as_deref()
        .map(|a| !a.trim().is_empty())
        .unwrap_or(false);
    let target_album_id = if organization == Organization::SingleAlbum && into_album_active {
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
        album_id: target_album_id.clone(),
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
        let api_key_for_album_assignment = api_key_clone.clone();
        let staging_dir = if staging_requested {
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
        let log_contents = std::fs::read_to_string(&request.log_path).unwrap_or_default();
        let file_errors =
            crate::services::stdout_parser::parse_error_log(&log_contents, &source_paths);
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
        // parse_run_progress above resolves `file=` records against
        // invocation_roots (the staging dir for a staged import, matching what
        // immich-go actually saw), so uploaded/duplicate counts aren't silently
        // dropped. Wipe eligibility is a separate, stricter question: for a
        // staged import the log's paths still point at the temp symlink dir
        // cleaned up below, so wipe must target the user's selected originals
        // instead. SHA-1 verify_uploaded still gates deletion to files the
        // server actually holds, so unuploaded picks are kept safe.
        let completed_asset_paths = if staged_import {
            // Already validated under the source roots at admission, by
            // `validate_selected_under_sources` — which canonicalizes each
            // entry and so requires it to exist, the same containment
            // guarantee `retain_paths_under_sources` gives the non-staged
            // branch below, just established before the run instead of after.
            // A staged import therefore has no log-derived contained paths at
            // all (the log only ever saw the temp staging dir), but this
            // pre-run validation already supplies equivalent evidence, so
            // `completed_asset_paths.len()` can stand in for it below as
            // `classify_completed_run`'s contained-path count.
            select_files.clone()
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
                file_errors.len(),
                keep_files,
                completed_asset_paths.len(),
                run.scan_errors,
            );
            checkpoint_eligible = eligible;
            let failed = matches!(status, JobStatus::Failed);
            if wipe_eligible {
                if let Ok(mut pending) = PENDING_WIPE.lock() {
                    pending.insert(
                        job_id_clone.clone(),
                        PendingWipe {
                            paths: completed_asset_paths.clone(),
                            // Verify against the URL the upload actually used
                            // (post-failover), not the primary configured one,
                            // or the existence check can hit the wrong server.
                            server_url: request.server_url.clone(),
                            api_key: api_key_for_album_assignment.clone(),
                        },
                    );
                } else {
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

            let error = if failed {
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

            let summary = if failed {
                None
            } else {
                let head = format!(
                    "Upload completed. {} uploaded, {} duplicates, {} errors.",
                    progress.uploaded, progress.duplicates, progress.errors
                );
                Some(if keep_files {
                    format!("{head} Files kept on disk.")
                } else if wipe_eligible {
                    format!("{head} Awaiting wipe confirmation.")
                } else {
                    head
                })
            };

            ImportJob {
                id: job_id_clone.clone(),
                status,
                progress,
                error,
                summary,
                awaiting_wipe_confirmation: wipe_eligible,
                pending_wipe_count: if wipe_eligible {
                    completed_asset_paths.len() as u32
                } else {
                    0
                },
                file_errors: file_errors.clone(),
                profile_id: profile.id.clone(),
                album_id: target_album_id.clone(),
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
            JobStatus::Completed => "completed",
            JobStatus::Cancelled => "cancelled",
            _ => "failed",
        };
        if let Err(err) = crate::services::store::append_history(
            &app_clone,
            crate::models::history::ImportRecord {
                id: update.id.clone(),
                started_at,
                finished_at: now_ms(),
                profile_id: profile.id.clone(),
                source_paths: record_source_paths.clone(),
                album_ids: album_ids.clone(),
                status: status.to_string(),
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
    let api_key = keychain::get_api_key(&profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {profile_id}"))?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    if server_url.is_empty() {
        return Err("No reachable Immich server URL for this profile.".to_string());
    }

    // Apply the same type/extension filters immich-go would, so the forecast
    // counts the files that will actually upload (both filters are extension-based
    // on immich-go's side, so this matches). Date/only-new is intentionally NOT
    // applied here — it needs per-file EXIF — so the UI flags the estimate.
    let include_type = sanitize_include_type(include_type.as_deref());
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
            tauri::async_runtime::spawn_blocking(move || {
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
                        None,
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

    let mut result = wipe::forecast_upload(&server_url, &api_key, &candidates).await?;
    result.truncated = result.truncated || truncated;
    result.unreadable += skipped_unreadable;
    Ok(result)
}

/// immich-go accepts only VIDEO or IMAGE for --include-type; anything else drops.
fn sanitize_include_type(value: Option<&str>) -> Option<String> {
    value.and_then(|v| match v.trim().to_ascii_uppercase().as_str() {
        "VIDEO" => Some("VIDEO".to_string()),
        "IMAGE" => Some("IMAGE".to_string()),
        _ => None,
    })
}

/// Normalize extensions to immich-go's leading-dot, lowercase form, dropping blanks.
fn normalize_extensions(exts: &[String]) -> Vec<String> {
    exts.iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{e}"))
        .collect()
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
                        } else {
                            None
                        };
                        let _ = logs::append_log(
                            "app.log",
                            &format!(
                                "import_wipe_verified job_id={} confirmed={} unverified={} deleted={} changed={}",
                                job_id, confirmed_count, unverified_count, wipe_result.deleted, wipe_result.changed
                            ),
                        );
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
        // Put the payload back so a later import_confirm_wipe can retry.
        if let Ok(mut map) = PENDING_WIPE.lock() {
            map.insert(job_id.clone(), payload);
        }
        job.awaiting_wipe_confirmation = true;
        job.pending_wipe_count = pending_count as u32;
    } else {
        job.awaiting_wipe_confirmation = false;
        job.pending_wipe_count = 0;
    }
    set_job(job.clone())?;
    let _ = logs::append_log(
        "app.log",
        &format!(
            "import_wipe_confirmed job_id={} confirm={} pending_count={}",
            job_id, confirm, pending_count
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

    let cancellation = Arc::new(AtomicBool::new(false));
    let previous = {
        let mut active = ACTIVE_SCAN
            .lock()
            .map_err(|_| "Could not lock active scan state".to_string())?;
        active.replace(cancellation.clone())
    };
    if let Some(previous) = previous {
        previous.store(true, Ordering::Relaxed);
    }

    let deadline = Instant::now() + SCAN_DEADLINE;
    let scan_cancellation = cancellation.clone();
    let progress = Arc::new(Mutex::new(ScanSummary {
        status: "complete".to_string(),
        photo_count: 0,
        video_count: 0,
        total_size_bytes: 0,
        skipped_unreadable: 0,
    }));
    let scan_progress = progress.clone();
    let mut scan_task = tauri::async_runtime::spawn_blocking(move || {
        let paths = collapse_overlapping_roots(paths);
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
                    summary.status = "cancelled".to_string();
                    return Ok(summary);
                }
                Err(media_scanner::ScanError::TimedOut) => {
                    let mut summary = scan_progress
                        .lock()
                        .map_err(|_| "Could not lock scan progress state".to_string())?
                        .clone();
                    summary.status = "timed_out".to_string();
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
                summary.status = "timed_out".to_string();
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
            summary.status = "cancelled".to_string();
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
    let active = ACTIVE_SCAN
        .lock()
        .map_err(|_| "Could not lock active scan state".to_string())?;
    if let Some(cancellation) = active.as_ref() {
        cancellation.store(true, Ordering::Relaxed);
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
            return Err(format!("Cannot cancel a terminal import: {job_id}"));
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
}
