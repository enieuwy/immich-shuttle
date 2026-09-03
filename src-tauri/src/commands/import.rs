use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
        device_detector,
        immich_client::ImmichClient,
        keychain, logs, media_scanner, profile_store,
        sidecar_runner::{run_upload, RunOutcome, RunUploadError, SidecarResult, UploadRequest},
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
/// How many confirmed wipes are running per job right now.
///
/// The delete runs inside `import_confirm_wipe`, on a job that is already
/// terminal, so neither `RUNNING_IMPORTS` nor `FINALIZING_IMPORTS` covers it.
/// Without this the app answers "nothing is live" while it hashes a whole card
/// and moves originals to the Trash, and a quit abandons that delete halfway
/// with the payload already consumed.
///
/// Counted rather than a set of ids: a partly failed delete puts its retry
/// payload back before the first call returns, so a second confirmation can be
/// in flight for the SAME job while the first still runs. A plain set would let
/// whichever finishes first clear the only mark and wave a quit through the
/// other's delete.
static ACTIVE_WIPES: LazyLock<Mutex<HashMap<String, u32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static JOB_INPUTS: LazyLock<Mutex<HashMap<String, ImportInput>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Jobs whose sidecar process did not prove it terminated.
///
/// These leases last for this process session. They stop a retry from reusing a
/// source while the old sidecar can still read it or upload it.
static SESSION_SAFETY_LEASES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
/// One confirmation owns a job until it publishes either a retry payload or the
/// final non-awaiting state. This closes the remove/reoffer interleaving.
static WIPE_CONFIRMATIONS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

const MAX_RETAINED_TERMINAL_JOBS: usize = 500;
/// Cap on how many staging failures are turned into per-file error entries for
/// the job payload. Matches the run parser's own cap in spirit: a hand-picked
/// selection can be thousands of files, and the whole list crosses IPC. The
/// aggregate fault still names the true total, so nothing is hidden by it.
const MAX_STAGING_FILE_ERRORS: usize = 100;
const SCAN_DEADLINE: Duration = Duration::from_secs(60 * 60);
/// Ceiling for `import_await_terminal`'s IPC-supplied `timeout_ms`. Ample
/// above the 30_000ms production ever passes; exists only so an untrusted
/// huge value can't push `Instant::now() + Duration::from_millis(..)` past
/// `Instant`'s `Add` overflow panic.
const MAX_AWAIT_TERMINAL_TIMEOUT_MS: u64 = 600_000;
const IMPORT_WORKER_PANIC_ERROR: &str = "Import worker stopped unexpectedly.";
/// Summary published for every cancelled run, by `import_cancel` and by a worker
/// that observes the raised flag. Pinned here so the two cannot drift.
const CANCELLED_SUMMARY: &str = "Import cancelled by user.";
const PENDING_WIPE_STORE_ERROR: &str =
    "Could not prepare wipe confirmation; source files were kept on disk. Import the source again to retry the delete.";
/// Returned when a forecast's walk stops answering.
///
/// The claim on that source root is held by the abandoned walk until the
/// filesystem answers, so the next forecast of the same source will report it
/// as unresponsive too. Naming the source's state is therefore honest, where
/// "the scan failed" would invite a pointless retry.
const FORECAST_UNRESPONSIVE_ERROR: &str =
    "The source stopped responding, so the server check timed out. Check the drive or network share, then try again.";

/// How long the staging step may make no progress at all before the worker stops
/// waiting for it.
///
/// A stall, not a duration cap: staging copies whole files when neither a
/// symlink nor a hard link is possible (Windows without developer mode, or
/// across volumes), so a healthy large selection can legitimately run for hours.
/// One staged file, or one copied chunk, resets it. A minute of complete silence
/// from a card or share is not something a working source does.
const STAGING_STALL: Duration = Duration::from_secs(60);
/// How long a cancelled blocking walk may take to notice the flag before the
/// worker abandons it. Staging checks the flag between files, so a responsive
/// source releases well inside this; a dead mount never does.
const CANCEL_ABANDON_GRACE: Duration = Duration::from_secs(5);
/// Poll step for `join_bounded`. Blocking joins are not hot paths; this only
/// decides how promptly a cancel or a stall is noticed.
const JOIN_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What became of a bounded blocking task.
enum BoundedJoin<T> {
    /// The task returned on its own.
    Finished(T),
    /// The bound passed with the task neither returning nor making progress, so
    /// it was abandoned.
    TimedOut,
    /// The task was cancelled but did not return within the grace, so it was
    /// abandoned.
    Abandoned,
}

/// Await a blocking filesystem task under a bound, abandoning it when the bound
/// or a cancel outlives it.
///
/// A deadline checked inside a walk cannot bound a call already blocked in the
/// kernel — a dead SMB/NFS mount or a sleeping USB drive never returns, so
/// nothing checks that deadline again. Only abandoning the join bounds it. The
/// cost of abandoning is one blocking-pool thread and one temp directory held
/// until the filesystem answers (or until the next startup prunes it), which is
/// the same trade `scan_sources_stream` already takes. The alternative is worse:
/// the command never resolves, and a worker holding a liveness marker refuses
/// app quit forever.
///
/// `bound` is measured from the last observed progress when the task reports any
/// through `progress`, and from the start of the wait when it does not. That
/// distinction is what keeps a legitimately long job — staging that has to copy
/// gigabytes — from being called unresponsive.
///
/// `cancel` is only ever READ here. It is the user's cancellation, and callers
/// publish a cancelled run when it is set, so raising it on a timeout would
/// report a dead mount as something the user asked for. Stopping a merely slow
/// walk is the task's own business.
async fn join_bounded<T>(
    mut task: tauri::async_runtime::JoinHandle<T>,
    bound: Duration,
    cancel: &AtomicBool,
    cancel_grace: Duration,
    progress: Option<&AtomicU64>,
) -> Result<BoundedJoin<T>, String> {
    let mut bound_deadline = Instant::now() + bound;
    let mut seen_progress = progress.map(|counter| counter.load(Ordering::Relaxed));
    let mut abandon_after: Option<Instant> = None;
    loop {
        if let Ok(joined) = tokio::time::timeout(JOIN_POLL_INTERVAL, &mut task).await {
            return joined
                .map(BoundedJoin::Finished)
                .map_err(|error| error.to_string());
        }
        let now = Instant::now();
        if let (Some(counter), Some(seen)) = (progress, seen_progress.as_mut()) {
            let current = counter.load(Ordering::Relaxed);
            if current != *seen {
                *seen = current;
                bound_deadline = now + bound;
            }
        }
        if now >= bound_deadline {
            return Ok(BoundedJoin::TimedOut);
        }
        if cancel.load(Ordering::Relaxed) {
            match abandon_after {
                Some(deadline) if now >= deadline => return Ok(BoundedJoin::Abandoned),
                Some(_) => {}
                None => abandon_after = Some(now + cancel_grace),
            }
        }
    }
}

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

    // The prompt is gone, so any payload this run registered is unreachable.
    // Drop its candidate paths rather than retaining them until an unrelated
    // dismiss or eviction happens to clear them. Credentials are never in the
    // payload: confirmation reads the profile API key from the keychain. Taken
    // after the `JOBS` guard is released, matching `finalize_job`'s lock order.
    if marked {
        if let Ok(mut pending) = PENDING_WIPE.lock() {
            pending.remove(job_id);
        }
    }
}

static IMPORT_START_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static ACTIVE_SCAN: LazyLock<Mutex<Option<Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(None));
static ACTIVE_FORECAST: LazyLock<Mutex<Option<ActiveForecast>>> =
    LazyLock::new(|| Mutex::new(None));
struct ActiveForecast {
    generation: u64,
    cancellation: Arc<AtomicBool>,
}

/// Clears the active forecast only when this forecast still owns the slot.
///
/// Ownership is the cancellation flag this forecast installed, not its
/// generation: the generation arrives over IPC and two callers can legitimately
/// send the same one. With a generation test, the first forecast's guard would
/// clear a slot the second had already replaced, and the `forecast_cancel` that
/// followed would find nothing to cancel while the second forecast kept hashing
/// the card. `Arc::ptr_eq` names the exact slot, the same way `ACTIVE_SCAN`'s
/// owner test does.
struct ActiveForecastGuard {
    cancellation: Arc<AtomicBool>,
}

impl Drop for ActiveForecastGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE_FORECAST.lock() {
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.cancellation, &self.cancellation))
            {
                active.take();
            }
        }
    }
}

/// An outstanding offer to delete the originals of one verified run.
///
/// Holds no credential. The profile's API key used to be copied in here and
/// kept resident for as long as the user left the prompt unanswered, which is
/// the whole life of the process; `import_confirm_wipe` now reads it from the
/// keychain at the moment it is needed, from the same `profile_id` the run
/// recorded. The payload therefore carries only what the delete cannot be
/// reconstructed from: the candidate paths, and the server the upload actually
/// reached (post-failover), which the profile no longer names.
/// An outstanding offer to delete the originals of one verified run.
///
/// Holds no credential. The profile's API key is read only at confirmation.
struct PendingWipe {
    paths: Vec<String>,
    server_url: String,
    /// A fresh volume identity for every candidate, recorded before the server
    /// hash check. A missing identity makes the offer unsafe.
    volume_ids: HashMap<String, String>,
    /// Registration order, so the bound in `bound_pending_wipes` can drop the
    /// oldest unanswered offer.
    sequence: u64,
}

/// Scripted answers for `wipe_volume_identities`, in test builds only.
///
/// A unit test cannot swap a card at a mount point between two probes, and that
/// race is exactly what the recheck before the delete exists to catch, so the
/// identity source is scriptable here. Each entry answers ONE resolver — one
/// batch of paths — and is consumed when that batch starts, so a test can say
/// "this volume before the server check, a different one after it". An empty
/// script means every probe reads the real device.
#[cfg(test)]
static VOLUME_IDENTITY_SCRIPT: LazyLock<Mutex<Vec<HashMap<String, String>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// One batch's identity probe, boxed so the production resolver and the test
/// script share a single call shape.
type VolumeIdentityResolver = Box<dyn FnMut(&Path) -> Option<String> + Send>;

/// The identity source both wipe probes read: one fresh resolver per batch, so
/// each batch re-probes rather than trusting an answer from before the last
/// server round trip, while paths sharing a mount inside one batch cost a
/// single probe.
fn wipe_volume_identities() -> VolumeIdentityResolver {
    #[cfg(test)]
    {
        let scripted = VOLUME_IDENTITY_SCRIPT
            .lock()
            .ok()
            .filter(|script| !script.is_empty())
            .map(|mut script| script.remove(0));
        if let Some(identities) = scripted {
            return Box::new(move |path: &Path| {
                identities.get(path.to_string_lossy().as_ref()).cloned()
            });
        }
    }
    Box::new(device_detector::file_volume_identity_resolver())
}

fn snapshot_wipe_volumes(
    paths: &[String],
    mut identity: impl FnMut(&Path) -> Option<String>,
) -> Result<HashMap<String, String>, String> {
    let mut volumes = HashMap::with_capacity(paths.len());
    for path in paths {
        let Some(volume) = identity(Path::new(path)) else {
            return Err(format!(
                "Could not prove the source volume for {path}; source files were kept for safety."
            ));
        };
        volumes.insert(path.clone(), volume);
    }
    Ok(volumes)
}

/// Re-prove the volume identity recorded for every candidate path.
///
/// Takes the payload's two fields rather than the payload because the probe is
/// a per-mount `diskutil`/`wmic` call that must not run on the async runtime:
/// the caller clones these onto a blocking thread and keeps the payload itself,
/// so a task that fails to join can never take the only handle a retry has.
fn recheck_volume_identities(
    paths: &[String],
    recorded: &HashMap<String, String>,
    mut identity: impl FnMut(&Path) -> Option<String>,
) -> Result<(), String> {
    for path in paths {
        let Some(expected) = recorded.get(path) else {
            return Err(format!(
                "The source volume was not recorded for {path}; source files were kept for safety."
            ));
        };
        let Some(actual) = identity(Path::new(path)) else {
            return Err(format!(
                "Could not prove the source volume for {path}; source files were kept for safety."
            ));
        };
        if actual != *expected {
            return Err(format!(
                "The source volume changed for {path}; source files were kept for safety."
            ));
        }
    }
    Ok(())
}

struct WipeConfirmationGuard {
    job_id: String,
}

impl WipeConfirmationGuard {
    fn acquire(job_id: String) -> Result<Self, String> {
        let mut confirmations = WIPE_CONFIRMATIONS
            .lock()
            .map_err(|_| "Could not lock wipe confirmation state".to_string())?;
        if !confirmations.insert(job_id.clone()) {
            return Err(format!(
                "A wipe confirmation is already running for job: {job_id}"
            ));
        }
        Ok(Self { job_id })
    }
}

impl Drop for WipeConfirmationGuard {
    fn drop(&mut self) {
        if let Ok(mut confirmations) = WIPE_CONFIRMATIONS.lock() {
            confirmations.remove(&self.job_id);
        }
    }
}
/// Source of `PendingWipe::sequence`. Monotonic for the life of the process,
/// which is exactly the lifetime the bound has to reason about.
static PENDING_WIPE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// How many unanswered delete prompts may be outstanding at once.
///
/// Each one pins every candidate source path of its run and, by design, refuses
/// both eviction (`is_clearable`) and dismissal, so nothing else in the process
/// can ever reclaim it. Without a bound a user who keeps importing and never
/// answers grows the map for the life of the app.
///
/// A bound is safe here only because the payload is the sole thing that can
/// delete anything: dropping one withdraws the OFFER and leaves every source
/// file on disk. The cost of the bound is that the user loses the offer for
/// their oldest un-answered card and has to import it again to be offered the
/// delete; the cost of no bound is unbounded retention. Sized well above any
/// plausible number of cards left unanswered in one session, so it is a
/// backstop rather than a limit normal use meets.
const MAX_PENDING_WIPE_PROMPTS: usize = 16;

/// Summary appended to a job whose delete offer the bound withdrew. Names the
/// one fact the user needs: the files are still there.
const WIPE_PROMPT_WITHDRAWN_SUMMARY: &str =
    "The delete offer was withdrawn because too many were left unanswered; source files were kept on disk. Import the source again to retry the delete.";

/// Reduce the outstanding delete offers to `MAX_PENDING_WIPE_PROMPTS`,
/// returning the job ids whose payload was dropped.
///
/// Oldest first: the newest run is the one on screen, and a prompt ignored
/// across several later imports is the one the user is least likely to answer.
/// Dropping a payload can never delete a file — it is the only handle a delete
/// has — so the worst outcome is a lost offer.
///
/// Callers MUST pass the dropped ids to `withdraw_wipe_prompts`. A job left
/// advertising `awaiting_wipe_confirmation` with no payload behind it can be
/// neither answered, dismissed, nor evicted, so it would strand the card
/// permanently.
///
/// `in_flight` names the jobs a confirmation is running for right now.
/// `import_confirm_wipe` takes its payload out of the map, reads the keychain,
/// and calls the server before it puts a retry payload back, so its entry is
/// visible to this bound at exactly the moment it must not be dropped: the user
/// has already said "delete", and withdrawing the offer under them loses the
/// verified list they answered for. Those jobs are therefore never candidates,
/// which lets the map exceed the cap by however many confirmations are actually
/// running — a number bounded by the one-confirmation-per-job guard.
fn bound_pending_wipes(
    pending: &mut HashMap<String, PendingWipe>,
    in_flight: &HashSet<String>,
) -> Vec<String> {
    let excess = pending.len().saturating_sub(MAX_PENDING_WIPE_PROMPTS);
    if excess == 0 {
        return Vec::new();
    }
    let mut by_age: Vec<(u64, String)> = pending
        .iter()
        .filter(|(id, _)| !in_flight.contains(*id))
        .map(|(id, entry)| (entry.sequence, id.clone()))
        .collect();
    by_age.sort_unstable();
    let dropped: Vec<String> = by_age.into_iter().take(excess).map(|(_, id)| id).collect();
    for id in &dropped {
        pending.remove(id);
    }
    dropped
}

/// The jobs a wipe confirmation is running for right now, for
/// `bound_pending_wipes`. A poisoned lock reads as "none in flight" rather than
/// refusing the bound: the bound only ever drops offers, and the worst case is
/// the pre-existing behaviour.
fn in_flight_wipe_confirmations() -> HashSet<String> {
    WIPE_CONFIRMATIONS
        .lock()
        .map(|confirmations| confirmations.clone())
        .unwrap_or_default()
}

/// Take the prompt off every job whose payload the bound dropped, so the card
/// stops offering a delete that can no longer be performed and becomes
/// clearable again.
///
/// Taken with only the `JOBS` lock held, after the `PENDING_WIPE` guard is
/// released, matching the lock order `finalize_job` and `mark_worker_panic`
/// already use.
fn withdraw_wipe_prompts(job_ids: &[String]) {
    if job_ids.is_empty() {
        return;
    }
    let mut jobs = match JOBS.lock() {
        Ok(jobs) => jobs,
        Err(poisoned) => poisoned.into_inner(),
    };
    for job in jobs
        .iter_mut()
        .filter(|job| job_ids.iter().any(|id| id == &job.id))
    {
        job.awaiting_wipe_confirmation = false;
        job.pending_wipe_count = 0;
        job.summary = Some(match job.summary.take() {
            Some(summary) => format!("{summary} {WIPE_PROMPT_WITHDRAWN_SUMMARY}"),
            None => WIPE_PROMPT_WITHDRAWN_SUMMARY.to_string(),
        });
    }
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

/// Marks a confirmed wipe as live for as long as it runs.
///
/// Every exit from `import_confirm_wipe` releases the mark, including an early
/// `?` return, so a failed verification cannot leave the app permanently
/// unquittable. Unlike `ImportWorkerGuard` this never rewrites the job: the run
/// itself already finished, and the wipe reports its own outcome.
struct ActiveWipeGuard {
    job_id: String,
}

impl ActiveWipeGuard {
    fn new(job_id: String) -> Self {
        *ACTIVE_WIPES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(job_id.clone())
            .or_insert(0) += 1;
        Self { job_id }
    }
}

impl Drop for ActiveWipeGuard {
    fn drop(&mut self) {
        let mut wipes = ACTIVE_WIPES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = wipes.get_mut(&self.job_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                wipes.remove(&self.job_id);
            }
        }
    }
}

fn forecast_generation_matches(active_generation: u64, requested_generation: u64) -> bool {
    active_generation == requested_generation
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

/// Prefix of the error returned when a cancel arrives for a running job whose
/// cancellation flag is already gone.
///
/// The frontend matches this text to treat the cancel as a race it already won
/// rather than a failure.
pub const IMPORT_NOT_RUNNING_ERROR: &str = "Import is no longer running:";

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

/// Text `import_start` returns while a run is still uploading.
const IMPORT_RUNNING_ERROR: &str = "An import is already running";
/// Text `import_start` returns while the previous worker is still finalizing.
///
/// Kept distinct from `IMPORT_RUNNING_ERROR` because the two ask different
/// things of the user: the first means "stop that import first", the second
/// means "wait a moment and retry". Cancel publishes a terminal status while
/// the worker still writes history, so a fast Retry click lands here.
const IMPORT_FINISHING_ERROR: &str = "An import is still finishing; try again in a moment.";

/// Pure admission decision, so the ordering of the two refusals is testable
/// without the process-global job maps.
fn admission_block_reason(
    run_live: bool,
    finalizing_live: bool,
    safety_lease_live: bool,
) -> Option<&'static str> {
    if safety_lease_live {
        Some("A previous import did not prove its sidecar stopped; restart the app before importing this source again.")
    } else if run_live {
        Some(IMPORT_RUNNING_ERROR)
    } else if finalizing_live {
        Some(IMPORT_FINISHING_ERROR)
    } else {
        None
    }
}

/// Why a new import may not start right now, or `None` when one may.
///
/// `FINALIZING_IMPORTS` is part of the answer, not an afterthought: the worker
/// leaves `RUNNING_IMPORTS` before it reads the run log, writes the terminal
/// state, and appends history, and `import_cancel` has already published
/// `Cancelled` by then. Without the finalizing half, a cancel followed by a
/// fast Retry admits a second run while the first still owns those writes.
/// `import_await_terminal` and `has_live_import_worker` observe both maps for
/// the same reason, so admission must not answer the question differently.
///
/// `ACTIVE_WIPES` is deliberately NOT part of the answer: a confirmed delete
/// does not block a new import. The delete only touches files the server has
/// already confirmed, and it moves them to the Trash rather than erasing them,
/// so a run that starts during one cannot lose anything — a file trashed between
/// that run's scan and its upload is reported as a per-file access error in its
/// log, against a copy the server already holds. Blocking would instead stop the
/// user importing an unrelated card for as long as it takes to hash and delete
/// the previous one, which is minutes on a full card. `has_live_import_worker`
/// does count the wipe, so quitting waits for it even though admission does not.
fn import_admission_block() -> Result<Option<&'static str>, String> {
    let jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    let has_active_job = jobs.iter().any(|job| is_active(&job.status));
    drop(jobs);
    let run_live = {
        let running = RUNNING_IMPORTS
            .lock()
            .map_err(|_| "Could not lock running imports state".to_string())?;
        has_active_job || !running.is_empty()
    };
    let finalizing_live = !FINALIZING_IMPORTS
        .lock()
        .map_err(|_| "Could not lock finalizing imports state".to_string())?
        .is_empty();
    let safety_lease_live = !SESSION_SAFETY_LEASES
        .lock()
        .map_err(|_| "Could not lock import safety lease state".to_string())?
        .is_empty();
    Ok(admission_block_reason(
        run_live,
        finalizing_live,
        safety_lease_live,
    ))
}

#[cfg(any(target_os = "macos", test))]
/// Reports whether a confirmed wipe is running right now.
///
/// A poisoned lock fails safe: shutdown must not proceed while this cannot be
/// read with confidence.
fn has_active_wipe() -> bool {
    ACTIVE_WIPES
        .lock()
        .map(|wipes| !wipes.is_empty())
        .unwrap_or(true)
}

#[cfg(target_os = "macos")]
/// Reports whether an import worker, its post-run finalization, or a confirmed
/// wipe is still live.
///
/// A poisoned lock fails safe: shutdown must not proceed while the worker state
/// cannot be read with confidence. A wipe counts because it hashes the card and
/// moves originals to the Trash after the run is already terminal; quitting
/// through it leaves the card half deleted with no record of which files went.
pub fn has_live_import_worker() -> bool {
    import_admission_block()
        .map(|reason| reason.is_some())
        .unwrap_or(true)
        || has_active_wipe()
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
        // The same decision as `import_admission_block`, taken again with the
        // maps held. That check runs before the fallible setup this insert
        // follows, so it cannot also be the one that serializes two racing
        // starts. Both refusals come from `admission_block_reason` so their
        // wording cannot drift: a caller that got past the first check only to
        // lose the race here reads the same sentence it would have read there.
        let mut running = RUNNING_IMPORTS
            .lock()
            .map_err(|_| "Could not lock running imports state".to_string())?;
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "Could not lock import job state".to_string())?;
        let run_live = !running.is_empty() || jobs.iter().any(|job| is_active(&job.status));
        let finalizing_live = !FINALIZING_IMPORTS
            .lock()
            .map_err(|_| "Could not lock finalizing imports state".to_string())?
            .is_empty();
        let safety_lease_live = !SESSION_SAFETY_LEASES
            .lock()
            .map_err(|_| "Could not lock import safety lease state".to_string())?
            .is_empty();
        if let Some(reason) = admission_block_reason(run_live, finalizing_live, safety_lease_live) {
            return Err(reason.to_string());
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
    store_job(job, false).map(|_| ())
}

/// Publish `job` only while the stored status is still active, reporting whether
/// the write landed.
///
/// The check and the write share one `JOBS` hold, so a worker that publishes its
/// terminal state in the same instant cannot be overwritten from a stale
/// snapshot. `import_cancel` needs that: it reads the job, sets the cancel flag,
/// and only then writes `Cancelled`, and a run that finished in that gap must
/// keep the outcome it published — otherwise the card says `Cancelled` while the
/// history record, written by the worker, says something else.
fn set_job_if_active(job: ImportJob) -> Result<bool, String> {
    store_job(job, true)
}

fn store_job(job: ImportJob, require_active: bool) -> Result<bool, String> {
    let wrote;
    let evicted_ids = {
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "Could not lock import job state".to_string())?;
        let Some(index) = jobs.iter().position(|existing| existing.id == job.id) else {
            return Ok(false);
        };
        if require_active && !is_active(&jobs[index].status) {
            return Ok(false);
        }

        let terminal = is_terminal(&job.status);
        jobs[index] = job;
        wrote = true;
        // Terminal jobs are ordered by their last state transition so eviction
        // keeps the most recently completed/cancelled/failed jobs.
        if terminal {
            let job = jobs.remove(index);
            jobs.push(job);
        }
        evict_old_terminal_jobs(&mut jobs)
    };
    remove_job_state(&evicted_ids);
    Ok(wrote)
}

/// The one shape a cancelled run is published in, wherever the cancel is
/// observed. `import_cancel` and a worker that reads the raised flag must agree,
/// or the card and the history record disagree about the same run.
///
/// The counts are cleared with the status, matching what `import_cancel` and the
/// worker's own cancel branch have always published: a cancelled run reports the
/// cancellation, not a partial tally the user cannot act on.
fn cancelled_state(job: ImportJob) -> ImportJob {
    ImportJob {
        status: JobStatus::Cancelled,
        error: None,
        summary: Some(CANCELLED_SUMMARY.to_string()),
        awaiting_wipe_confirmation: false,
        pending_wipe_count: 0,
        progress: JobProgress::default(),
        file_errors: Vec::new(),
        ..job
    }
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
///
/// `cancel` is the run's cancel flag, read inside that same hold. A cancel is
/// raised before it is published, so a worker finalizing in that gap would
/// otherwise publish its own outcome, persist a history record from it, and only
/// then be overwritten as `Cancelled` on the card. Reading the flag here decides
/// the stored state and the record together. Callers with no flag to offer pass
/// `None`.
fn finalize_job(update: ImportJob, cancel: Option<&AtomicBool>) -> ImportJob {
    let mut cancelled_id: Option<String> = None;
    let mut evicted_ids: Vec<String> = Vec::new();
    let unconfirmed_termination = SESSION_SAFETY_LEASES
        .lock()
        .map(|leases| leases.contains(&update.id))
        .unwrap_or(true);

    let stored = {
        let Ok(mut jobs) = JOBS.lock() else {
            return update;
        };
        let Some(index) = jobs.iter().position(|existing| existing.id == update.id) else {
            return update;
        };
        // Only while the stored job is still active: a run that already
        // published its own terminal state keeps it, exactly as the two guards
        // below insist, so a late cancel cannot relabel a finished run.
        let update = match cancel {
            Some(flag) if is_active(&jobs[index].status) && flag.load(Ordering::Relaxed) => {
                cancelled_state(update)
            }
            _ => update,
        };

        if matches!(jobs[index].status, JobStatus::Cancelled)
            && !matches!(update.status, JobStatus::Cancelled)
            && !unconfirmed_termination
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

/// Enumerate the exact source files that existed before the sidecar started.
///
/// A selected import manifests the user selection itself. A full import walks
/// the source with no extension filter, because the question this answers is
/// "which files existed under this source", not "which files does the preview
/// grid show": immich-go uploads formats the preview allowlist omits, and a
/// manifest narrower than the uploader turns every such clip into an
/// unmanifested upload — a permanent fault on the card and an original that is
/// never offered for deletion. Failure leaves no manifest.
///
/// `cancel` and `progress` exist so the caller can bound this walk the way it
/// bounds staging: a source that stops answering blocks inside the kernel,
/// where the deadline below can never reach it.
fn immutable_source_manifest(
    selected: Option<&[String]>,
    source_paths: &[String],
    cancel: &AtomicBool,
    progress: &AtomicU64,
) -> Result<HashSet<PathBuf>, String> {
    let mut manifest = HashSet::new();
    if let Some(selected) = selected {
        for path in selected {
            manifest.insert(
                std::fs::canonicalize(path).map_err(|error| {
                    format!("Could not manifest selected source {path}: {error}")
                })?,
            );
        }
        return Ok(manifest);
    }
    for source in source_paths {
        let skipped = media_scanner::manifest_directory_streaming(
            Path::new(source),
            Some(cancel),
            Some(Instant::now() + SCAN_DEADLINE),
            progress,
            &mut |batch| {
                for file in batch {
                    if let Ok(path) = std::fs::canonicalize(file) {
                        manifest.insert(path);
                    }
                }
            },
        )
        .map_err(|error| format!("Could not manifest source {source}: {error}"))?;
        if skipped > 0 {
            return Err(format!(
                "Could not manifest every source file under {source}"
            ));
        }
    }
    Ok(manifest)
}

/// Await a source-manifest walk under the same silence bound staging uses,
/// yielding the manifest or the reason there is none.
///
/// The walk runs BEFORE any upload and an unresponsive card or share blocks it
/// inside the kernel, where the deadline the walk checks between entries can
/// never reach it. An unbounded join therefore parks the worker in
/// `RUNNING_IMPORTS` with no upload started and no way for the user to quit.
/// Abandoning the join is the only bound that holds, and it costs one blocking
/// thread until the filesystem answers — the same trade staging already takes.
///
/// A bound that passes leaves NO manifest, deliberately: the run then reports a
/// fault, offers no delete prompt, and cannot advance the checkpoint, which is
/// the honest reading of "nothing is known about what was on this source".
async fn bounded_source_manifest(
    task: tauri::async_runtime::JoinHandle<Result<HashSet<PathBuf>, String>>,
    progress: &AtomicU64,
    cancel: &AtomicBool,
    stall: Duration,
) -> (Option<HashSet<PathBuf>>, Option<String>) {
    match join_bounded(task, stall, cancel, CANCEL_ABANDON_GRACE, Some(progress)).await {
        Ok(BoundedJoin::Finished(Ok(manifest))) => (Some(manifest), None),
        Ok(BoundedJoin::Finished(Err(error))) => (None, Some(error)),
        Ok(BoundedJoin::TimedOut) => (
            None,
            Some("The source stopped responding while it was being listed.".to_string()),
        ),
        Ok(BoundedJoin::Abandoned) => (
            None,
            Some("Listing the source did not stop when the import was cancelled.".to_string()),
        ),
        Err(error) => (None, Some(format!("Listing the source failed: {error}"))),
    }
}

fn retain_paths_in_manifest(
    paths: Vec<String>,
    manifest: Option<&HashSet<PathBuf>>,
) -> (Vec<String>, usize) {
    let Some(manifest) = manifest else {
        return (Vec::new(), paths.len());
    };
    let mut dropped = 0;
    let paths = paths
        .into_iter()
        .filter(|path| {
            let keep = std::fs::canonicalize(path)
                .ok()
                .is_some_and(|path| manifest.contains(&path));
            if !keep {
                dropped += 1;
            }
            keep
        })
        .collect();
    (paths, dropped)
}

/// One `import_error` app.log line.
///
/// Both values are quoted because neither is ours: the file name comes from the
/// user's card and the reason comes from the server, so either can contain a
/// literal ` reason=` and silently redraw this line's field boundaries for
/// anything reading the log back. `{:?}` also keeps a newline inside the value
/// from splitting one event into two.
fn import_error_log_line(job_id: &str, error: &FileError) -> String {
    format!(
        "import_error job_id={job_id} file={:?} reason={:?}",
        error.file, error.reason
    )
}

fn manifest_evidence_is_complete(
    manifest_present: bool,
    unmanifested_paths: usize,
    unresolved_file_events: u32,
) -> bool {
    manifest_present && unmanifested_paths == 0 && unresolved_file_events == 0
}

/// Error returned when the renderer asks to import an explicitly empty subset.
pub const EMPTY_SELECTION_ERROR: &str =
    "No files were selected. Choose at least one file, or import the whole folder.";

/// Reduce the renderer's `select_files` to the one discriminant the import
/// pipeline acts on: `None` imports the whole source, `Some(files)` imports
/// exactly those files after staging them.
///
/// An explicitly empty vector is neither, so it is refused here. The frontend
/// sends `null` when there is no subset, so an empty vector is a caller bug —
/// and reading it as "whole source" is the worst available guess: the forecast
/// counts zero files while the run would upload every media file under the
/// roots, and with keep-files off that unasked-for run then proposes wiping the
/// card. `import_start` and `import_forecast` both normalize here so they can
/// never disagree about what a given request means.
fn normalize_select_files(
    select_files: Option<Vec<String>>,
    source_paths: &[String],
) -> Result<Option<Vec<String>>, String> {
    match select_files {
        None => Ok(None),
        Some(files) if files.is_empty() => Err(EMPTY_SELECTION_ERROR.to_string()),
        Some(files) => {
            validate_selected_under_sources(&files, source_paths)?;
            Ok(Some(files))
        }
    }
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
struct RunClassification {
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
/// duplicate matches) AND it ended badly (bad or unproven exit, or per-file
/// errors); a partial run that uploaded or matched duplicates succeeds,
/// surfacing errors. Wipe is eligible only for a successful run with keep-files
/// off, at least one completed path, and an OBSERVED clean exit.
///
/// That last requirement is the same positive test the checkpoint already
/// makes, for the same reason. `failed` is cleared by a single landed asset, so
/// without it a run whose sidecar was never seen to stop — it may still be
/// reading and uploading the card right now — could offer the user's originals
/// for deletion. Refusing costs the user one re-import to be offered the delete
/// again; accepting can delete a file the run never finished with.
///
/// This `status` is deliberately NOT the whole account of how the run ended. A
/// single landed asset clears `failed` however badly the run finished, so the
/// aggregate faults — a bad or unproven exit, a source that could not be
/// enumerated, a selection that could not be staged — are reported separately by
/// `aggregate_fault_reasons` and reach the user's terminal error, summary, and
/// history receipt independently of this boolean. Folding them in here is what
/// let a run upload one photo, abort while enumerating the rest, and still
/// publish "Upload completed. 1 uploaded, 0 duplicates, 0 errors."
///
/// The checkpoint is separate and stricter because advancing it is a silent,
/// irreversible narrowing of every later import: it becomes a capture-date floor
/// passed to immich-go, and media added afterwards with an older capture date
/// falls below it and is never offered again. So it requires positive evidence
/// that this run actually processed the source — at least one landed asset, an
/// OBSERVED clean exit, and no aggregate scan error — and that evidence must
/// have survived containment (see `completed_paths_len` below): a forged log
/// entry naming a file that does not exist on disk cannot supply it. A
/// zero-asset run (empty card, or filters that excluded everything) is still
/// `Completed`, because nothing went wrong; it just has not earned the right to
/// raise the floor. Erring this way costs a re-scan that server-side dedupe
/// makes harmless; erring the other way loses photos.
fn classify_completed_run(
    uploaded: u32,
    duplicates: u32,
    outcome: RunOutcome,
    file_errors_len: usize,
    keep_files: bool,
    completed_paths_len: usize,
    scan_errors: u32,
) -> RunClassification {
    // `Unknown` means termination was never confirmed, so it counts as a bad
    // ending everywhere a non-zero exit does. It must never read as success:
    // there is strictly LESS evidence here than in an observed failure.
    let ended_badly = !matches!(outcome, RunOutcome::Exited { success: true });
    let landed = uploaded > 0 || duplicates > 0;
    let failed = !landed && (ended_badly || file_errors_len > 0);
    let status = if failed {
        JobStatus::Failed
    } else {
        JobStatus::Completed
    };
    let wipe_eligible = !failed
        && !keep_files
        && completed_paths_len > 0
        && matches!(outcome, RunOutcome::Exited { success: true });
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
    //
    // The exit test is positive — the process was seen to exit cleanly — rather
    // than "not known to have failed". `RunOutcome` has no third state that
    // could satisfy a positive match without that observation, so no
    // synthesized or defaulted value can open this gate.
    let checkpoint_eligible = !failed
        && landed
        && completed_paths_len > 0
        && file_errors_len == 0
        && matches!(outcome, RunOutcome::Exited { success: true })
        && scan_errors == 0;
    RunClassification {
        status,
        wipe_eligible,
        checkpoint_eligible,
    }
}

/// Fold one upload path's outcome into the run's, keeping the worse of the two.
///
/// A multi-path run is one job, so any path that ended badly decides the whole
/// run. `Unknown` outranks an observed failure because it is strictly less
/// evidence: a failure was at least seen to happen.
fn worse_outcome(current: RunOutcome, next: RunOutcome) -> RunOutcome {
    match (current, next) {
        (RunOutcome::Unknown, _) | (_, RunOutcome::Unknown) => RunOutcome::Unknown,
        (RunOutcome::Exited { success: left }, RunOutcome::Exited { success: right }) => {
            RunOutcome::Exited {
                success: left && right,
            }
        }
    }
}

/// What the upload loop knows so far, folded one source path at a time.
///
/// The loop itself is only iteration; every decision about what a path's result
/// means to the whole run lives here, so the rule that stops a run can be
/// stated and tested in one place.
struct RunTally {
    error_lines: Vec<String>,
    outcome: RunOutcome,
    cancelled: bool,
    /// A sidecar this run started was never proven to have stopped.
    unconfirmed_termination: bool,
    spawn_error: Option<String>,
}

impl RunTally {
    fn new() -> Self {
        Self {
            error_lines: Vec::new(),
            // Starts at the only value that claims nothing: an observed clean
            // exit is what every path must supply for the run to keep it. A
            // path that ended badly, or whose termination was never confirmed,
            // replaces this through `worse_outcome`.
            outcome: RunOutcome::Exited { success: true },
            cancelled: false,
            unconfirmed_termination: false,
            spawn_error: None,
        }
    }

    /// Fold one path's result in, answering whether the run must stop here.
    ///
    /// `reaped` is the only proof that the sidecar process is gone, and it is
    /// independent of the exit status: a closed event channel yields an `Ok`
    /// result with `reaped == false`, which means a process may still be
    /// reading this card and uploading from it. Continuing would put a SECOND
    /// sidecar on the same source, and the run would go on to publish a
    /// cancellation with no fault at all — erasing the only evidence that a
    /// process is still out there. So an unreaped `Ok` stops the run exactly
    /// like the unconfirmed-termination error does, keeping the merged outcome
    /// and the stderr the terminal error is built from.
    fn absorb(&mut self, result: Result<SidecarResult, RunUploadError>) -> bool {
        match result {
            Ok(run) => {
                self.error_lines.extend(run.error_lines);
                self.outcome = worse_outcome(self.outcome, run.outcome);
                if !run.reaped {
                    self.unconfirmed_termination = true;
                    return true;
                }
                false
            }
            Err(RunUploadError::Cancelled) => {
                self.cancelled = true;
                true
            }
            Err(RunUploadError::UnconfirmedTermination(error)) => {
                self.unconfirmed_termination = true;
                self.spawn_error = Some(error);
                true
            }
            Err(error) => {
                self.spawn_error = Some(error.to_string());
                true
            }
        }
    }
}

/// Everything a completed run's aggregate faults are computed from. Bundled
/// because they all answer one question — what did this run fail to prove it
/// did — and adding a new kind of missing evidence must not mean adding another
/// positional boolean nobody passes.
struct RunFaultInputs<'a> {
    outcome: RunOutcome,
    scan_errors: u32,
    /// Selections staging could not prepare, out of `requested`.
    unstaged: usize,
    requested: usize,
    /// The last few stderr lines, for the exit clauses.
    stderr_tail: Option<&'a str>,
    /// Why the run log could not be read, when it could not.
    log_read_error: Option<&'a str>,
    /// Why the pre-sidecar source manifest failed, when it failed with a reason.
    manifest_error: Option<&'a str>,
    /// Whether a pre-sidecar source manifest exists at all.
    manifest_present: bool,
    /// Uploaded paths the manifest did not contain.
    unmanifested_paths: usize,
    /// `file=` records in the run log that resolved to no invocation root.
    unresolved_file_events: u32,
}

/// The aggregate faults of a completed run, one user-facing clause each, or
/// empty when the run has no fault the per-file tallies do not already express.
///
/// "Aggregate" means not attributable to any single file, which is exactly why
/// these used to vanish: `file_errors` and `progress.errors` count per-FILE
/// failures, the summary was formatted from those counts alone, and the stderr
/// tail was only reached when the run was classified `Failed` — which one
/// landed asset prevents. So a run that uploaded one photo and then aborted
/// while enumerating the rest reported itself as clean, and the user could
/// reformat the card on the strength of that.
///
/// The stderr tail is carried in the exit clauses because for a landed-but-bad
/// run it is the ONLY evidence of what went wrong: nothing else in the job
/// names it. That includes the unproven-termination clause, where the tail is
/// the reap diagnostic — the one line that says WHY the process could not be
/// confirmed gone.
///
/// The evidence clauses below (no manifest, unmanifested uploads, unresolved
/// log records, an unreadable log) are faults for the same reason a bad exit
/// is: each one means the run cannot account for what it did. They already
/// withheld the delete prompt and the checkpoint silently; naming them here is
/// what puts them on the card and in the history receipt as well. The concrete
/// case: one resolved and one unresolved upload record in the same log
/// published "Upload completed. 1 uploaded" with no fault, while the second
/// file was kept with no explanation anywhere.
fn aggregate_fault_reasons(inputs: RunFaultInputs<'_>) -> Vec<String> {
    let RunFaultInputs {
        outcome,
        scan_errors,
        unstaged,
        requested,
        stderr_tail,
        log_read_error,
        manifest_error,
        manifest_present,
        unmanifested_paths,
        unresolved_file_events,
    } = inputs;
    let mut reasons = Vec::new();
    match outcome {
        RunOutcome::Exited { success: true } => {}
        RunOutcome::Exited { success: false } => reasons.push(match stderr_tail {
            Some(tail) => format!("immich-go exited with an error: {tail}"),
            None => "immich-go exited with an error.".to_string(),
        }),
        // Never presented as a success. The channel closed or a kill could not
        // be confirmed, so what the process did with the rest of the source is
        // unknown — which is a weaker claim than a failure, not a stronger one.
        RunOutcome::Unknown => reasons.push(match stderr_tail {
            Some(tail) => format!(
                "immich-go never reported that it stopped, so what happened to the rest of this source is unknown: {tail}"
            ),
            None => "immich-go never reported that it stopped, so what happened to the rest of this source is unknown."
                .to_string(),
        }),
    }
    if let Some(error) = log_read_error {
        reasons.push(error.to_string());
    }
    if scan_errors > 0 {
        reasons.push(format!(
            "{scan_errors} source error(s) were reported with no file named, so some files were never offered to the server."
        ));
    }
    if unstaged > 0 {
        reasons.push(format!(
            "{unstaged} of {requested} selected file(s) could not be prepared for upload, so they were never sent."
        ));
    }
    if !manifest_present {
        reasons.push(match manifest_error {
            Some(error) => format!(
                "The source could not be listed before the upload started, so no uploaded file can be matched back to it: {error}"
            ),
            None => "The source could not be listed before the upload started, so no uploaded file can be matched back to it."
                .to_string(),
        });
    }
    if unmanifested_paths > 0 {
        reasons.push(format!(
            "{unmanifested_paths} uploaded file(s) were not in the source listing taken before the run, so they were kept."
        ));
    }
    if unresolved_file_events > 0 {
        reasons.push(format!(
            "{unresolved_file_events} upload result(s) in the run log could not be matched to a source file, so those files were kept."
        ));
    }
    reasons
}

/// Everything the terminal error and summary of a completed run are computed
/// from. Bundled so the computation can be a pure function with one argument.
struct RunEvidenceInputs<'a> {
    /// `classify_completed_run`'s status, as a boolean.
    failed: bool,
    progress: JobProgress,
    /// How many per-file errors the job payload carries.
    file_error_count: usize,
    /// The run's aggregate faults, from `aggregate_fault_reasons`.
    faults: &'a [String],
    keep_files: bool,
    awaiting_wipe_confirmation: bool,
    pending_wipe_store_failed: bool,
}

/// What a completed run tells the user about how it ended.
struct TerminalEvidence {
    error: Option<String>,
    summary: Option<String>,
}

/// Build the terminal error and summary for a run that reached the sidecar.
///
/// Pure, because this is the surface a user decides whether to reformat a card
/// on. The invariant it exists to hold: a run with ANY aggregate fault cannot
/// publish a clean-looking result. Neither the error nor the summary consults
/// `failed` before reporting the faults, because `failed` is cleared by a
/// single landed asset — the old builder reached the stderr tail only when
/// `failed`, and formatted the summary from the per-file tallies alone, so a
/// run that uploaded one photo and then abandoned the rest of the card
/// published "Upload completed. 1 uploaded, 0 duplicates, 0 errors."
fn terminal_evidence(inputs: RunEvidenceInputs<'_>) -> TerminalEvidence {
    let RunEvidenceInputs {
        failed,
        progress,
        file_error_count,
        faults,
        keep_files,
        awaiting_wipe_confirmation,
        pending_wipe_store_failed,
    } = inputs;

    // The unreadable-log clause is NOT handled here: it is an aggregate fault
    // like any other, so `aggregate_fault_reasons` owns it. Carrying it
    // separately put it in the `error` but never in the `summary` a `Completed`
    // card actually shows, and left `incomplete` — the history receipt's own
    // account — believing the run was clean.

    // All terminal evidence travels together. A log-read or wipe-prompt warning
    // must not hide a bad/unproven exit or a source that could not enumerate:
    // for a `Failed` run there is no summary to carry the omitted clause, and
    // even a `Completed` card needs its `error` to be a complete receipt.
    let mut clauses = Vec::new();
    if pending_wipe_store_failed {
        // The user's originals are the thing at stake, so this comes first.
        clauses.push(PENDING_WIPE_STORE_ERROR.to_string());
    }
    clauses.extend_from_slice(faults);
    if file_error_count > 0 {
        clauses.push(format!(
            "{file_error_count} file(s) could not be uploaded; see the error list."
        ));
    }
    if clauses.is_empty() && failed {
        // Classified failed with nothing quotable: reachable only when the sole
        // evidence was per-file errors that all fell out of the job's list as
        // unmapped staged paths.
        clauses.push("immich-go reported errors during upload".to_string());
    }
    let error = (!clauses.is_empty()).then(|| clauses.join(" | "));

    let summary = if failed {
        // A failed run has no summary; its error carries the whole account.
        None
    } else {
        let head = format!(
            "Upload completed. {} uploaded, {} duplicates, {} errors.",
            progress.uploaded, progress.duplicates, progress.errors
        );
        let disposition = if keep_files {
            " Files kept on disk."
        } else if awaiting_wipe_confirmation {
            " Awaiting wipe confirmation."
        } else if pending_wipe_store_failed {
            " Source files were kept on disk because wipe confirmation could not be prepared."
        } else {
            ""
        };
        // The queue card shows the SUMMARY for a `Completed` run, so a fault has
        // to be named here as well as in `error`. The counts describe only the
        // files the sidecar reached, and read as a clean success for a run that
        // never reached the rest.
        let fault_note = if faults.is_empty() {
            String::new()
        } else {
            format!(" This run did not finish cleanly: {}", faults.join(" "))
        };
        Some(format!("{head}{disposition}{fault_note}"))
    };

    TerminalEvidence { error, summary }
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

/// The album name a run can be deep-linked to, or `None` when it fans out.
///
/// "Open in Immich" may name one album only when the run targets exactly one:
/// `SingleAlbum` mode with a non-empty `--into-album`. The folder and tag modes
/// spread assets across many albums, and the picker still sends its selected
/// album name in those modes, so gating on the name alone would offer a link to
/// one arbitrary album the run never exclusively populated.
fn album_link_target(organization: Organization, into_album: Option<&str>) -> Option<String> {
    if organization != Organization::SingleAlbum {
        return None;
    }
    let name = into_album?.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// The run-scoped inputs a history receipt needs that the job itself does not
/// carry. They are decided once at admission and always travel together.
struct RunRecord {
    started_at: i64,
    source_paths: Vec<String>,
    request: ImportInput,
}

/// The two decisions about a terminal run that only the finalizing worker knows
/// and the job itself does not carry.
///
/// They travel together because they answer the same question from opposite
/// ends: whether this run is trustworthy enough to narrow every later import,
/// and whether it is untrustworthy enough that the receipt must say so.
#[derive(Debug, Clone, Copy, Default)]
struct RunVerdict {
    /// Whether this run may advance the source's only-new-since checkpoint.
    checkpoint_eligible: bool,
    /// Whether the run ended with an aggregate fault — work it was asked to do
    /// that it never proved it did. Persisted so History cannot present the run
    /// as clean; see `ImportRecord::incomplete`.
    incomplete: bool,
}

/// The history receipt for one terminal run.
///
/// Pure, so the status mapping and the replayable request are testable without
/// an `AppHandle`: this is the only place a run's outcome becomes a record, and
/// a wrong mapping here misreports the run in History forever.
fn run_history_record(
    update: &ImportJob,
    record: RunRecord,
    incomplete: bool,
) -> crate::models::history::ImportRecord {
    crate::models::history::ImportRecord {
        id: update.id.clone(),
        started_at: record.started_at,
        finished_at: now_ms(),
        profile_id: update.profile_id.clone(),
        source_paths: record.source_paths,
        // The album this run actually landed in, resolved from the name
        // immich-go targeted, rather than whatever id the picker sent.
        album_ids: update.album_id.clone().into_iter().collect(),
        status: match &update.status {
            JobStatus::Completed => RecordStatus::Completed,
            JobStatus::Cancelled => RecordStatus::Cancelled,
            _ => RecordStatus::Failed,
        },
        total: update.progress.total,
        uploaded: update.progress.uploaded,
        duplicates: update.progress.duplicates,
        errors: update.progress.errors,
        // A cancelled run reports the cancellation, not a partial tally (see
        // `cancelled_state`), so it carries no fault either: the user stopped
        // it, nothing failed to happen that was still expected to.
        incomplete: incomplete && !matches!(update.status, JobStatus::Cancelled),
        // Persist the request (source/options) so History can replay it.
        request: Some(record.request),
    }
}

/// Persist one terminal run to the history store.
///
/// Every worker exit routes through here, so a run that ends during staging
/// leaves the same receipt as one that ends during upload: History can replay
/// it, and the per-source checkpoint decision is taken once, in one place.
fn persist_run_history(
    app: &tauri::AppHandle,
    update: &ImportJob,
    record: RunRecord,
    verdict: RunVerdict,
) {
    let checkpoint_eligible =
        verdict.checkpoint_eligible && matches!(update.status, JobStatus::Completed);
    if let Err(err) = crate::services::store::append_history(
        app,
        run_history_record(update, record, verdict.incomplete),
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
}

/// Dispose of a finished run's staging directory, returning the path when it
/// was RETAINED rather than removed.
///
/// Removing the tree is only safe once the sidecar is proven gone. When
/// termination was never confirmed the process may still be reading the staged
/// links, and immich-go follows them to the user's originals: deleting the tree
/// under a live uploader turns a recoverable unknown into a half-uploaded card
/// with no record of which files went.
///
/// `std::mem::forget` is the mechanism on purpose. `StagingDir`'s `Drop`
/// removes the tree, and the open `.lock` file it holds is exactly the lease
/// `prune_stale_temp_artifacts` probes: while this process lives the lock is
/// held and the directory is left alone, and the moment the process exits the
/// lock is released so the next startup reclaims it. Leaking one descriptor per
/// unproven run for the rest of the session is the price of not deleting files
/// a live process is using.
///
/// Runs on a blocking thread: the cleanup branch removes a directory tree.
fn release_staging_dir(dir: staging::StagingDir, termination_unproven: bool) -> Option<PathBuf> {
    if !termination_unproven {
        staging::cleanup_staging_dir(dir);
        return None;
    }
    let retained = dir.path().to_path_buf();
    std::mem::forget(dir);
    Some(retained)
}

/// Close out a worker that exits during staging, before the shared
/// finalization tail can run.
///
/// Staging failures and a cancel raised while staging are terminal outcomes
/// like any other, so they publish a terminal state AND a history record. The
/// `FINALIZING_IMPORTS` marker is registered before `RUNNING_IMPORTS` is
/// released so shutdown never sees a gap in which no map names this worker,
/// and it covers the history write the same way the normal tail does.
///
/// `cancel` is the run's own cancel flag, and `finalize_job` reads it inside the
/// `JOBS` hold that publishes the state, so the stored state and the history
/// record below always agree about whether the run was cancelled. A cancel is
/// raised before it is published: without that shared hold, a staging failure
/// landing in the gap would publish `Failed`, persist a `Failed` record, and
/// only then be overwritten as `Cancelled` on the card.
fn finish_staging_exit(
    app: &tauri::AppHandle,
    job_id: &str,
    profile_id: &str,
    error: String,
    cancel: &AtomicBool,
    record: RunRecord,
) {
    if let Ok(mut finalizing) = FINALIZING_IMPORTS.lock() {
        finalizing.insert(job_id.to_string());
    }
    if let Ok(mut running) = RUNNING_IMPORTS.lock() {
        running.remove(job_id);
    }
    let update = finalize_job(
        ImportJob {
            id: job_id.to_string(),
            status: JobStatus::Failed,
            progress: JobProgress {
                total: 0,
                uploaded: 0,
                duplicates: 0,
                errors: 1,
            },
            error: Some(error),
            summary: None,
            awaiting_wipe_confirmation: false,
            pending_wipe_count: 0,
            file_errors: Vec::new(),
            profile_id: profile_id.to_string(),
            album_id: None,
        },
        Some(cancel),
    );
    // A run that never reached the sidecar did none of what it was asked to do,
    // so the receipt records the fault. `run_history_record` drops it again for
    // a cancelled run, which is the user's own decision rather than a fault.
    persist_run_history(
        app,
        &update,
        record,
        RunVerdict {
            checkpoint_eligible: false,
            incomplete: true,
        },
    );
    if let Ok(mut finalizing) = FINALIZING_IMPORTS.lock() {
        finalizing.remove(job_id);
    }
}

/// Everything `import_start` decides about a request before it reads the
/// keychain, the profile store, or the process-global job maps.
///
/// Separate from the command body because the command takes an `AppHandle`,
/// which a unit test cannot build. With this inline, the two decisions that
/// matter most at this boundary — an explicitly empty selection must never
/// become a whole-source upload, and an unknown error mode must be refused
/// rather than silently inverted into "stop at the first error" — could only be
/// asserted against the helpers, so a command that stopped calling them would
/// still pass every test. Here they are the command's own answer.
struct StartPlan {
    source_paths: Vec<String>,
    /// `Some` is a hand-picked subset, which is staged; `None` is the whole
    /// source. The explicitly empty case never gets this far.
    select_files: Option<Vec<String>>,
    keep_files: bool,
    stack_raw_jpeg: bool,
    stack_burst: bool,
    date_range: Option<String>,
    concurrent_tasks: Option<u32>,
    into_album: Option<String>,
    organization: Organization,
    on_errors: Option<String>,
    overwrite: bool,
    tags: Vec<String>,
    session_tag: bool,
    include_type: Option<String>,
    include_extensions: Vec<String>,
    exclude_extensions: Vec<String>,
    /// The one album this run can be deep-linked to, or `None` when it fans out.
    album_link_name: Option<String>,
    /// The picker's id, shown while the run is in flight and replaced at
    /// finalization by the album the upload actually populated.
    provisional_album_id: Option<String>,
}

fn plan_import_start(input: &ImportInput) -> Result<StartPlan, String> {
    if input.source_paths.is_empty() {
        return Err("At least one source path is required".to_string());
    }

    let source_paths = collapse_overlapping_roots(input.source_paths.clone());
    // The UI limits this to 1..=20; re-clamp here since the value arrives over
    // IPC and must not be trusted to be in range (unbounded values would be
    // forwarded straight to immich-go's --concurrent-tasks).
    let concurrent_tasks = input.concurrent_tasks.map(|n| n.clamp(1, 20));
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
    let tags: Vec<String> = input
        .tags
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let include_type = parse_include_type(input.include_type.as_deref())?;
    let include_extensions = normalize_extensions(&input.include_extensions);
    let exclude_extensions = normalize_extensions(&input.exclude_extensions);
    // immich-go uploads into a single album per run (`--into-album`), so more
    // than one id is not a request this command can honour. Only element 0 was
    // ever read; refuse the rest rather than discard it silently.
    if input.album_ids.len() > 1 {
        return Err(format!(
            "An import targets one album, but {} were selected.",
            input.album_ids.len()
        ));
    }
    // Both the in-flight id and the resolution at finalization read this, so
    // they cannot disagree about whether a link is warranted.
    let album_link_name = album_link_target(input.organization, input.into_album.as_deref());
    let provisional_album_id = if album_link_name.is_some() {
        input.album_ids.first().cloned()
    } else {
        None
    };
    let select_files = normalize_select_files(input.select_files.clone(), &source_paths)?;
    Ok(StartPlan {
        source_paths,
        select_files,
        // Selected (staged) imports honor the same keep/delete toggle as
        // whole-folder imports; the post-wipe SHA-1 verification guards deletion
        // either way.
        keep_files: input.keep_files,
        stack_raw_jpeg: input.stack_raw_jpeg,
        stack_burst: input.stack_burst,
        date_range: input.date_range.clone(),
        concurrent_tasks,
        into_album: input.into_album.clone(),
        organization: input.organization,
        on_errors,
        overwrite: input.overwrite,
        tags,
        session_tag: input.session_tag,
        include_type,
        include_extensions,
        exclude_extensions,
        album_link_name,
        provisional_album_id,
    })
}

#[tauri::command]
pub async fn import_start(app: tauri::AppHandle, input: ImportInput) -> Result<String, String> {
    let StartPlan {
        source_paths,
        select_files,
        keep_files,
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
        album_link_name,
        provisional_album_id,
    } = plan_import_start(&input)?;
    let record_source_paths = source_paths.clone();
    // Credentials are read only after every pure check on the input has passed.
    // Reading the keychain first can raise an OS unlock prompt, and reports a
    // missing key, for a request this command was always going to refuse.
    let profile = profile_store::get_profile(&input.profile_id)?;
    let api_key = keychain::require_api_key(&input.profile_id)?;

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
        if let Some(reason) = import_admission_block()? {
            return Err(reason.to_string());
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
        // The album lookup is the only post-run action that needs this key. Wipe
        // confirmation reads from the keychain only after the user confirms; it
        // must never retain a copy in `PendingWipe`.
        let api_key_for_finalization = api_key_clone.clone();
        // Freeze the candidate set before the sidecar sees any source. A later
        // log entry can only propose a wipe when it names this immutable set.
        let manifest_selection = select_files.clone();
        let manifest_sources = source_paths.clone();
        let manifest_cancel = cancel_flag.clone();
        let manifest_progress = Arc::new(AtomicU64::new(0));
        let manifest_progress_for_walk = manifest_progress.clone();
        let manifest_task = tauri::async_runtime::spawn_blocking(move || {
            immutable_source_manifest(
                manifest_selection.as_deref(),
                &manifest_sources,
                manifest_cancel.as_ref(),
                manifest_progress_for_walk.as_ref(),
            )
        });
        let (pre_sidecar_manifest, manifest_error) = bounded_source_manifest(
            manifest_task,
            manifest_progress.as_ref(),
            cancel_flag.as_ref(),
            STAGING_STALL,
        )
        .await;

        let mut staging_dir = if let Some(selected_files) = select_files {
            let cancel_flag_for_staging = cancel_flag.clone();
            // Claimed BEFORE anything is queued, and released only when the walk
            // task ends, so a source that stops answering is refused by name on
            // the next attempt. Two orderings were wrong here and both allowed
            // retries to pile up against a dead mount: claiming inside the walk
            // means no claim exists until the closure runs, and claiming in a
            // second blocking task means the claim can queue behind the very
            // threads it is meant to cap. The registry is a plain mutex with a
            // `CLAIM_GRACE` ceiling, so it is taken here, off the blocking pool
            // entirely — the same order `import_forecast` uses.
            let claim = match media_scanner::acquire_scan_roots(
                media_scanner::ScanPurpose::Stage,
                &source_paths,
            ) {
                Ok(claim) => claim,
                Err(e) => {
                    finish_staging_exit(
                        &app_clone,
                        &job_id_clone,
                        &profile.id,
                        format!("Could not stage selected files: {e}"),
                        cancel_flag.as_ref(),
                        RunRecord {
                            started_at,
                            source_paths: record_source_paths,
                            request: history_request,
                        },
                    );
                    return;
                }
            };
            // One counter, written by the staging walk and read by the waiter.
            // Both apply the same test — has this source done anything lately —
            // to the two halves of the problem: the walk sees the gap between
            // files, and only the waiter can see a call stuck inside the kernel.
            let staging_progress = Arc::new(AtomicU64::new(0));
            let staging_progress_for_walk = staging_progress.clone();
            let staging_task = tauri::async_runtime::spawn_blocking(move || {
                // The claim must be dropped by the walking task: abandoning the
                // join cannot stop a blocked filesystem call, so releasing it
                // here would admit a duplicate walk of a root that is still held.
                let _staging_roots = claim;
                staging::create_staging_dir(
                    &selected_files,
                    Some(cancel_flag_for_staging.as_ref()),
                    Some(STAGING_STALL),
                    staging_progress_for_walk.as_ref(),
                )
            });
            // Staging is the one filesystem walk in this worker, and the worker
            // holds the liveness markers app quit waits on. An unbounded join
            // here means a dead mount refuses shutdown forever, so the join is
            // bounded by the same stall and a cancel may abandon it.
            let staged = join_bounded(
                staging_task,
                STAGING_STALL,
                cancel_flag.as_ref(),
                CANCEL_ABANDON_GRACE,
                Some(staging_progress.as_ref()),
            )
            .await;
            let staged_dir = match staged {
                Ok(BoundedJoin::Finished(Ok(dir))) => Ok(dir),
                Ok(BoundedJoin::Finished(Err(e))) => {
                    Err(format!("Could not stage selected files: {e}"))
                }
                Ok(BoundedJoin::TimedOut) => Err(format!("{}.", staging::STAGING_TIMED_OUT_ERROR)),
                // The walk is blocked inside a filesystem call and cannot be
                // interrupted. Report the run as terminal and release the
                // liveness markers so the user can quit; the abandoned task
                // drops its own staging directory if the source ever answers,
                // and startup pruning removes it if it never does.
                Ok(BoundedJoin::Abandoned) => Err(
                    "Staging did not stop when cancelled: the source stopped responding."
                        .to_string(),
                ),
                Err(e) => Err(format!("Staging task failed: {e}")),
            };
            match staged_dir {
                Ok(dir) => Some(dir),
                Err(error) => {
                    finish_staging_exit(
                        &app_clone,
                        &job_id_clone,
                        &profile.id,
                        error,
                        cancel_flag.as_ref(),
                        RunRecord {
                            started_at,
                            source_paths: record_source_paths,
                            request: history_request,
                        },
                    );
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
        let mut tally = RunTally::new();
        let mut request = request;
        for path in upload_paths {
            request.source_path = path;
            // The loop is iteration only; `absorb` owns what each result means
            // to the run, including which of them must stop it.
            if tally.absorb(run_upload(app_clone.clone(), request.clone()).await) {
                break;
            }
        }
        let RunTally {
            error_lines,
            outcome,
            cancelled,
            unconfirmed_termination,
            spawn_error,
        } = tally;

        // Staging's own account of the selection, taken before cleanup consumes
        // the guard. A selection that was partly staged ran against fewer files
        // than the user asked for, and nothing in the run log can say so: the
        // sidecar never saw the missing files.
        let (staging_requested, staging_failures) = match staging_dir.as_mut() {
            Some(dir) => (dir.requested, std::mem::take(&mut dir.failures)),
            None => (0, Vec::new()),
        };

        // Take the map before cleanup consumes the guard. The log is parsed after
        // the temporary directory is gone, so only this map can restore user paths.
        let staged_links = staging_dir
            .as_mut()
            .map(|dir| dir.take_links())
            .unwrap_or_default();
        if let Some(dir) = staging_dir {
            let retained = tauri::async_runtime::spawn_blocking(move || {
                release_staging_dir(dir, unconfirmed_termination)
            })
            .await
            .unwrap_or_default();
            if let Some(root) = retained {
                let _ = logs::append_log(
                    "app.log",
                    &format!(
                        "import_staging_retained job_id={job_id_clone} reason=unconfirmed_termination path={root:?}"
                    ),
                );
            }
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
        let (parsed_file_errors, dropped_staged_errors) = if staged_import {
            translate_staged_file_errors(parsed_file_errors, &staged_links)
        } else {
            (parsed_file_errors, 0)
        };
        // A selection that could not be staged never reached the sidecar, so it
        // appears in no run log. Report it in the same place as an upload
        // failure of the same file: it is a per-file failure, the user picked
        // that file by hand, and it is the file's own path they need to see.
        // Capped like the parser's own list so a large selection cannot grow the
        // IPC payload without bound; `staging_failures.len()` below still counts
        // every one for the tallies and the aggregate fault.
        let mut file_errors = parsed_file_errors;
        for failure in staging_failures.iter().take(MAX_STAGING_FILE_ERRORS) {
            file_errors.push(FileError {
                file: failure.source.clone(),
                reason: failure.message.clone(),
            });
        }
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
        let mut progress = run.progress;
        // Files that could not be staged are counted with the run's per-file
        // errors. They are the reason the run's own tally is not the whole
        // story: the sidecar could not fail on a file it was never given, so
        // without this the summary reports "0 errors" for a selection that was
        // only partly attempted, and the history receipt records the same.
        progress.errors = progress
            .errors
            .saturating_add(staging_failures.len().min(u32::MAX as usize) as u32);
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
        let (completed_asset_paths, unmanifested_paths) =
            retain_paths_in_manifest(completed_asset_paths, pre_sidecar_manifest.as_ref());

        // A process without a confirmed reap may still be reading this source
        // and uploading from it, so the source is leased for the rest of this
        // session however the run otherwise ended. The lease is taken even when
        // a cancellation raced the run: a cancel the user asked for says
        // nothing about whether the process actually stopped.
        if unconfirmed_termination {
            if let Ok(mut leases) = SESSION_SAFETY_LEASES.lock() {
                leases.insert(job_id_clone.clone());
            }
        }
        let cancelled =
            !unconfirmed_termination && (cancelled || cancel_flag.load(Ordering::Relaxed));

        // Only a completed run can earn the checkpoint; cancelled/failed stay false.
        let mut checkpoint_eligible = false;
        // A spawn failure is an aggregate fault by definition: the run was
        // asked to upload paths it never even started. `run_history_record`
        // drops the flag again for a cancelled run.
        let mut incomplete = spawn_error.is_some();
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
            let RunClassification {
                status,
                wipe_eligible: classified_wipe_eligible,
                checkpoint_eligible: eligible,
            } = classify_completed_run(
                progress.uploaded,
                progress.duplicates,
                outcome,
                file_errors.len() + dropped_staged_errors,
                keep_files,
                completed_asset_paths.len(),
                run.scan_errors,
            );
            let evidence_complete = manifest_evidence_is_complete(
                pre_sidecar_manifest.is_some(),
                unmanifested_paths,
                run.unresolved_file_events,
            );
            let wipe_eligible = classified_wipe_eligible && evidence_complete;
            checkpoint_eligible = eligible && evidence_complete;
            let failed = matches!(status, JobStatus::Failed);
            // The last few stderr lines. For a run that landed an asset and
            // then exited non-zero this is the ONLY account of what went wrong:
            // no per-file error was logged, and the tallies look clean.
            let stderr_tail = {
                let newest: Vec<&str> = error_lines
                    .iter()
                    .rev()
                    .take(3)
                    .map(|line| line.as_str())
                    .collect();
                let tail: Vec<&str> = newest.into_iter().rev().collect();
                (!tail.is_empty()).then(|| tail.join(" | "))
            };
            // Assembled WITHOUT consulting `failed`. One landed asset clears
            // that boolean, and these are exactly the faults it then hid. Every
            // kind of missing evidence is in here, so `incomplete` below is
            // simply "this list is not empty" and the card, the terminal error,
            // and the history receipt cannot disagree about it.
            let faults = aggregate_fault_reasons(RunFaultInputs {
                outcome,
                scan_errors: run.scan_errors,
                unstaged: staging_failures.len(),
                requested: staging_requested,
                stderr_tail: stderr_tail.as_deref(),
                log_read_error: log_read_error.as_deref(),
                manifest_error: manifest_error.as_deref(),
                manifest_present: pre_sidecar_manifest.is_some(),
                unmanifested_paths,
                unresolved_file_events: run.unresolved_file_events,
            });
            incomplete = !faults.is_empty();
            let mut pending_wipe_stored = false;
            let mut pending_wipe_store_failed = false;
            let mut withdrawn_prompts: Vec<String> = Vec::new();
            if wipe_eligible {
                // Every candidate is a FILE, and only its containing mount has
                // an identity, so the resolver maps each one to its mount root
                // and probes that root once. The probe shells out per mount and
                // is allowed up to three seconds, which is far too long to hold
                // the async worker: run the whole batch on a blocking thread.
                let candidates = completed_asset_paths.clone();
                let volumes = match tauri::async_runtime::spawn_blocking(move || {
                    snapshot_wipe_volumes(
                        &candidates,
                        wipe_volume_identities(),
                    )
                })
                .await
                {
                    Ok(volumes) => volumes,
                    Err(error) => Err(format!(
                        "Could not prove the source volumes; source files were kept for safety: {error}"
                    )),
                };
                match (volumes, PENDING_WIPE.lock()) {
                    (Ok(volume_ids), Ok(mut pending)) => {
                        pending.insert(
                            job_id_clone.clone(),
                            PendingWipe {
                                paths: completed_asset_paths.clone(),
                                server_url: request.server_url.clone(),
                                volume_ids,
                                sequence: PENDING_WIPE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
                            },
                        );
                        pending_wipe_stored = true;
                        // Bound the outstanding offers here, at terminalization,
                        // rather than at admission: refusing to START an import
                        // because an older prompt is unanswered would stop the
                        // user importing at all, and the run that has already
                        // uploaded is the one whose offer is worth keeping.
                        withdrawn_prompts =
                            bound_pending_wipes(&mut pending, &in_flight_wipe_confirmations());
                    }
                    (Err(_), _) | (_, Err(_)) => {
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
            // Taken after the `PENDING_WIPE` guard is released: the two locks are
            // never held together (see `finalize_job`). This job is the newest
            // offer, so the bound can never withdraw the one being published.
            if !withdrawn_prompts.is_empty() {
                withdraw_wipe_prompts(&withdrawn_prompts);
                let _ = logs::append_log(
                    "app.log",
                    &format!(
                        "import_wipe_prompts_withdrawn job_id={} withdrawn={}",
                        job_id_clone,
                        withdrawn_prompts.len()
                    ),
                );
            }

            let (awaiting_wipe_confirmation, pending_wipe_count) = wipe_prompt_state(
                wipe_eligible,
                pending_wipe_stored,
                completed_asset_paths.len(),
            );
            let TerminalEvidence { error, summary } = terminal_evidence(RunEvidenceInputs {
                failed,
                progress: progress.clone(),
                file_error_count: file_errors.len(),
                faults: &faults,
                keep_files,
                awaiting_wipe_confirmation,
                pending_wipe_store_failed,
            });

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
                    album_link_name.as_deref(),
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
                // No fallback to the picker's id. That id is what goes stale when
                // the album is deleted or recreated, so preferring it when the
                // name did not resolve reinstates the wrong link this resolution
                // exists to prevent: no link is honest, a stale link is not.
                album_id: resolved_album_id,
            }
        };

        for fe in &file_errors {
            let _ = logs::append_log("app.log", &import_error_log_line(&job_id_clone, fe));
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
        // No cancel flag here: `cancelled` above was already re-read from it
        // before the wipe payload was built, and the stored-`Cancelled` guard
        // inside `finalize_job` covers a cancel that lands later.
        let update = finalize_job(update, None);
        persist_run_history(
            &app_clone,
            &update,
            RunRecord {
                started_at,
                source_paths: record_source_paths,
                request: history_request,
            },
            RunVerdict {
                checkpoint_eligible,
                incomplete,
            },
        );
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
    generation: u64,
) -> Result<wipe::ForecastResult, String> {
    let cancellation = Arc::new(AtomicBool::new(false));
    let previous = {
        let mut active = ACTIVE_FORECAST
            .lock()
            .map_err(|_| "Could not lock active forecast state".to_string())?;
        active.replace(ActiveForecast {
            generation,
            cancellation: cancellation.clone(),
        })
    };
    if let Some(previous) = previous {
        previous.cancellation.store(true, Ordering::Relaxed);
    }
    // The flag this forecast just installed IS its claim on the slot. See
    // `ActiveForecastGuard`: the generation is caller-supplied and repeatable.
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
    let select_files = normalize_select_files(select_files, &source_paths)?;

    // Same ordering rule as `import_start`: refuse an unusable filter before
    // touching the keychain or probing the server, so "Check server" reports the
    // offending value rather than a missing key.
    let include_type = parse_include_type(include_type.as_deref())?;
    let include_extensions = normalize_extensions(&include_extensions);
    let exclude_extensions = normalize_extensions(&exclude_extensions);

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
            let scan_task = tauri::async_runtime::spawn_blocking(move || {
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
            });
            // The per-entry deadline inside the walk cannot bound a call already
            // blocked on a dead mount, so bound the join too — exactly as
            // `scan_sources_stream` does. Without this the IPC never resolves
            // and the forecast spinner never stops.
            match join_bounded(
                scan_task,
                SCAN_DEADLINE,
                cancellation.as_ref(),
                CANCEL_ABANDON_GRACE,
                None,
            )
            .await
            .map_err(|e| format!("Scan task failed: {e}"))?
            {
                BoundedJoin::Finished(result) => result?,
                // The claimed root stays claimed until the filesystem answers,
                // so say the source is unresponsive rather than pretending the
                // forecast merely failed.
                BoundedJoin::TimedOut | BoundedJoin::Abandoned => {
                    return Err(FORECAST_UNRESPONSIVE_ERROR.to_string())
                }
            }
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
    let volume_ids = failed_paths
        .iter()
        .filter_map(|path| {
            pending
                .volume_ids
                .get(path)
                .map(|volume| (path.clone(), volume.clone()))
        })
        .collect();
    Some(PendingWipe {
        paths: failed_paths,
        server_url: pending.server_url,
        volume_ids,
        // Keeps its original place in the queue: a retry is the same offer, so
        // a partial delete must not push it ahead of older unanswered prompts.
        sequence: pending.sequence,
    })
}

/// Re-prove every candidate's volume identity, off the async runtime.
///
/// The identity of a mount is read by shelling out to the platform's disk tool,
/// once per distinct mount and with a per-call timeout measured in seconds.
/// This command is what the confirmation dialog is waiting on, so that work
/// cannot run on the runtime thread.
///
/// Only the two fields the check reads are cloned onto the blocking thread. The
/// payload stays here on purpose: it is the sole handle a retry has, and a task
/// that failed to join would carry it away and strand the card.
async fn recheck_wipe_volumes_off_thread(pending: &PendingWipe) -> Result<(), String> {
    let paths = pending.paths.clone();
    let recorded = pending.volume_ids.clone();
    match tauri::async_runtime::spawn_blocking(move || {
        recheck_volume_identities(&paths, &recorded, wipe_volume_identities())
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(format!(
            "Could not prove the source volume; source files were kept for safety: {error}"
        )),
    }
}

#[tauri::command]
pub async fn import_confirm_wipe(job_id: String, confirm: bool) -> Result<ImportJob, String> {
    let mut job = get_job(&job_id)?;
    if !job.awaiting_wipe_confirmation {
        return Err(format!("Job does not need wipe confirmation: {job_id}"));
    }

    // A second confirmation must not remove a payload while the first request
    // verifies it and later reoffers it.
    let _confirmation_guard = WipeConfirmationGuard::acquire(job_id.clone())?;
    // Marked live BEFORE the payload is consumed. The quit path reads only
    // `ACTIVE_WIPES`, so holding the payload lock across both steps would not
    // order them against a quit.
    let _wipe_guard = ActiveWipeGuard::new(job_id.clone());
    // The guard stays live until the payload becomes either a saved retry or a
    // completed confirmation state.
    // Read at confirmation time instead of being retained in the payload. An
    // unanswered prompt used to hold a copy of the profile's API key resident
    // for the whole life of the process, one copy per prompt; this removes the
    // credential from the payload entirely. Read BEFORE the payload is
    // consumed, and only when the user confirmed, so a decline never raises a
    // keychain prompt and a missing key leaves the offer intact and retryable
    // rather than consuming it.
    let api_key = if confirm {
        Some(keychain::require_api_key(&job.profile_id)?)
    } else {
        None
    };
    let pending = PENDING_WIPE
        .lock()
        .map_err(|_| "Could not lock pending wipe state".to_string())?
        .remove(&job_id)
        .ok_or_else(|| format!("No pending wipe payload for job: {job_id}"))?;
    if confirm {
        if let Err(error) = recheck_wipe_volumes_off_thread(&pending).await {
            PENDING_WIPE
                .lock()
                .map_err(|_| "Could not lock pending wipe state".to_string())?
                .insert(job_id.clone(), pending);
            return Err(error);
        }
    }

    let pending_count = pending.paths.len();
    // When verification fails we keep every file AND leave the job actionable so
    // the user can retry once the server is reachable again (previously the
    // payload was dropped, making retry impossible).
    let mut retry_pending: Option<PendingWipe> = None;

    // `Some` exactly when the user confirmed: the key above is read in that
    // case and only that case.
    if let Some(api_key) = api_key {
        match wipe::verify_uploaded(&pending.server_url, &api_key, &pending.paths).await {
            Ok(verified) => {
                let confirmed_count = verified.confirmed.len();
                let unverified_count = verified.unverified.len();
                // The recorded identity was last proven before the keychain
                // read and a whole server round trip ago. A card swapped at the
                // same mount inside that window has its OWN files hashed and
                // moved to the Trash, so prove the volume again immediately
                // before the delete. A failure refuses the WHOLE wipe rather
                // than deleting the subset that still matches: if the volume
                // cannot be identified, nothing about the set can be trusted.
                let deleted = match recheck_wipe_volumes_off_thread(&pending).await {
                    Ok(()) => Ok(tauri::async_runtime::spawn_blocking(move || {
                        wipe::wipe_files(&verified.confirmed)
                    })
                    .await),
                    Err(error) => Err(error),
                };
                match deleted {
                    Err(error) => {
                        job.summary = Some(format!(
                            "All {pending_count} files were kept: the source could not be proven to be the same volume after the server check."
                        ));
                        job.error = Some(error);
                        let _ = logs::append_log(
                            "app.log",
                            &format!("import_wipe_volume_recheck_failed job_id={job_id}"),
                        );
                        retry_pending = Some(pending);
                    }
                    Ok(Ok(wipe_result)) => {
                        let kept = wipe_result.failed
                            + wipe_result.skipped
                            + wipe_result.changed
                            + wipe_result.unprovable
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
                        // Distinct from `changed`: nothing was proven different,
                        // the identity simply could no longer be established —
                        // typically the volume was remounted between the check
                        // and the delete. Folding it into `changed` would tell
                        // the user their files were edited, which is not known.
                        let unprovable_note = if wipe_result.unprovable > 0 {
                            format!(
                                " {} could no longer be proven to be the verified files (the volume may have been remounted) and were kept.",
                                wipe_result.unprovable
                            )
                        } else {
                            String::new()
                        };
                        job.summary = Some(format!(
                            "Verified {} of {} files on the server and deleted {}. Kept {} ({} not found on server).{}{}",
                            confirmed_count,
                            pending_count,
                            wipe_result.deleted,
                            kept,
                            unverified_count,
                            changed_note,
                            unprovable_note,
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
                        } else if wipe_result.unprovable > 0 {
                            Some(format!(
                                "{} file(s) could no longer be proven to be the verified files and were kept for safety. The volume may have been remounted; run the import again to retry the delete.",
                                wipe_result.unprovable
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
                                "import_wipe_verified job_id={} confirmed={} unverified={} deleted={} changed={} unprovable={} skipped={}",
                                job_id, confirmed_count, unverified_count, wipe_result.deleted, wipe_result.changed, wipe_result.unprovable, wipe_result.skipped
                            ),
                        );
                        if wipe_result.failed > 0 {
                            retry_pending = retry_pending_wipe(pending, wipe_result.failed_paths);
                        }
                    }
                    Ok(Err(err)) => {
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
        let (stored, withdrawn_prompts) = match PENDING_WIPE.lock() {
            Ok(mut map) => {
                map.insert(job_id.clone(), payload);
                // This job's own confirmation is still in flight, so the bound
                // cannot withdraw the retry payload it just put back.
                let withdrawn_prompts =
                    bound_pending_wipes(&mut map, &in_flight_wipe_confirmations());
                (map.contains_key(&job_id), withdrawn_prompts)
            }
            Err(_) => (false, Vec::new()),
        };
        let (awaiting, count) = wipe_prompt_state(true, stored, retry_count);
        job.awaiting_wipe_confirmation = awaiting;
        job.pending_wipe_count = count;
        if !stored {
            // A concurrent terminalization filled the bound while this retry
            // verified the server. The retry can be the oldest offer and so may
            // be the prompt the bound withdraws; do not tell the user a delete
            // list failed to save when it was deliberately dropped. In both
            // cases the source files were kept, and no delete can happen.
            job.summary = Some(if withdrawn_prompts.iter().any(|id| id == &job_id) {
                WIPE_PROMPT_WITHDRAWN_SUMMARY.to_string()
            } else {
                format!(
                    "All {retry_count} files were kept. The delete list could not be saved, so this run cannot re-offer them."
                )
            });
            job.error = if withdrawn_prompts.iter().any(|id| id == &job_id) {
                None
            } else {
                Some(PENDING_WIPE_STORE_ERROR.to_string())
            };
        }
        // Write the job's current retry state before changing other cards. The
        // lock was released above, and excluding this job avoids appending the
        // withdrawn sentence a second time when this retry itself was oldest.
        set_job(job.clone())?;
        let other_withdrawn: Vec<String> = withdrawn_prompts
            .into_iter()
            .filter(|id| id != &job_id)
            .collect();
        if !other_withdrawn.is_empty() {
            withdraw_wipe_prompts(&other_withdrawn);
        }
        // The stored job state is already published above. The final shared
        // `set_job` below is intentionally skipped for this branch.
        let _ = logs::append_log(
            "app.log",
            &format!(
                "import_wipe_confirmed job_id={} confirm={} pending_count={} retrying={}",
                job_id, confirm, pending_count, job.pending_wipe_count
            ),
        );
        return Ok(job);
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

/// Cancel ONLY the active forecast, leaving a running preview scan alone.
///
/// The forecast runs from the preflight sheet, which the user can leave while
/// the preview scan behind it is still walking the card. `scan_cancel` raises
/// both flags, so abandoning the forecast through it also killed that unrelated
/// scan and emptied the picker the user was still choosing from. Idempotent, so
/// an effect cleanup may call it unconditionally.
#[tauri::command]
pub async fn forecast_cancel(generation: u64) -> Result<(), String> {
    let active = ACTIVE_FORECAST
        .lock()
        .map_err(|_| "Could not lock active forecast state".to_string())?;
    if let Some(active) = active
        .as_ref()
        .filter(|active| forecast_generation_matches(active.generation, generation))
    {
        active.cancellation.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Stop everything walking the user's sources: the preview scan AND the
/// forecast.
///
/// Deliberately still cancels both. This is the "let go of the card" action —
/// it backs deselecting a source and app shutdown, where a forecast left
/// hashing keeps a claim on roots the next scan needs and keeps reading a drive
/// the user is about to unplug. `forecast_cancel` above is the narrower one.
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
        if let Some(active) = active.as_ref() {
            active.cancellation.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn import_cancel(job_id: String) -> Result<(), String> {
    let job = get_job(&job_id)?;
    match &job.status {
        JobStatus::Running => {
            let running = RUNNING_IMPORTS
                .lock()
                .map_err(|_| "Could not lock running imports state".to_string())?;
            let flag = running
                .get(&job_id)
                .ok_or_else(|| format!("{IMPORT_NOT_RUNNING_ERROR} {job_id}"))?;
            flag.store(true, Ordering::Relaxed);
        }
        JobStatus::Pending => {}
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled => {
            return Err(format!("{TERMINAL_CANCEL_ERROR}: {job_id}"));
        }
    }

    // Published only while the job is still active. The worker can finalize in
    // the gap between the status read above and this write, and its history
    // record is written from the state it published; overwriting that state from
    // this stale snapshot would leave the card and History disagreeing about the
    // same run. A run that finished first keeps its outcome, and the caller is
    // told the cancel was too late — the wording the frontend already treats as
    // a won race rather than a failure.
    if !set_job_if_active(cancelled_state(job))? {
        return Err(format!("{TERMINAL_CANCEL_ERROR}: {job_id}"));
    }
    // Dropped only once the cancellation is the stored state, so a run that
    // published a wipe prompt instead keeps the payload behind it.
    if let Ok(mut pending) = PENDING_WIPE.lock() {
        pending.remove(&job_id);
    }
    Ok(())
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

    fn is_failed(o: &RunClassification) -> bool {
        matches!(o.status, JobStatus::Failed)
    }

    /// The sidecar was seen to exit cleanly.
    const CLEAN_EXIT: RunOutcome = RunOutcome::Exited { success: true };
    /// The sidecar was seen to exit with a failure.
    const BAD_EXIT: RunOutcome = RunOutcome::Exited { success: false };

    /// A run with nothing wrong, so every fault test names only the one fault
    /// it is about.
    fn clean_fault_inputs() -> RunFaultInputs<'static> {
        RunFaultInputs {
            outcome: CLEAN_EXIT,
            scan_errors: 0,
            unstaged: 0,
            requested: 0,
            stderr_tail: None,
            log_read_error: None,
            manifest_error: None,
            manifest_present: true,
            unmanifested_paths: 0,
            unresolved_file_events: 0,
        }
    }

    /// Serializes the tests that script `VOLUME_IDENTITY_SCRIPT`. The script is
    /// process-wide, like the disks it stands in for, and each entry is
    /// consumed by whichever probe runs first, so two scripting tests running
    /// at once would answer each other's probes.
    fn volume_script_guard() -> std::sync::MutexGuard<'static, ()> {
        static VOLUME_SCRIPT_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
        VOLUME_SCRIPT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Arm the identity answers, one entry per probe batch, in order.
    fn script_volume_identities(batches: Vec<HashMap<String, String>>) {
        *VOLUME_IDENTITY_SCRIPT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = batches;
    }

    fn clear_volume_identity_script() {
        script_volume_identities(Vec::new());
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
        let o = classify_completed_run(0, 0, BAD_EXIT, 0, false, 3, 0);
        assert!(is_failed(&o));
        assert!(!o.wipe_eligible, "a failed run must never be wipe-eligible");
        assert!(!o.checkpoint_eligible);
    }

    #[test]
    fn nothing_landed_with_file_errors_is_failed() {
        let o = classify_completed_run(0, 0, CLEAN_EXIT, 2, false, 3, 0);
        assert!(is_failed(&o));
        assert!(!o.wipe_eligible);
        assert!(!o.checkpoint_eligible);
    }

    #[test]
    fn uploads_present_succeed_despite_errors_and_bad_exit() {
        // A partial run that uploaded something is still a success: the status
        // policy is unchanged.
        let o = classify_completed_run(5, 0, BAD_EXIT, 4, false, 5, 0);
        assert!(!is_failed(&o));
        assert!(
            !o.wipe_eligible,
            "the exit was not observed to be clean, so the originals are not offered for deletion"
        );
        assert!(
            !o.checkpoint_eligible,
            "a partial run must not raise the only-new date floor"
        );
        // The same run seen to exit cleanly does offer them, so the refusal
        // above comes from the ending and nothing else.
        assert!(classify_completed_run(5, 0, CLEAN_EXIT, 4, false, 5, 0).wipe_eligible);
    }

    #[test]
    fn duplicates_only_count_as_landed() {
        // Everything was already on the server (all duplicates): success, and the
        // originals are still eligible for deletion.
        let o = classify_completed_run(0, 7, CLEAN_EXIT, 0, false, 7, 0);
        assert!(!is_failed(&o));
        assert!(o.wipe_eligible);
        assert!(
            o.checkpoint_eligible,
            "the server holds every file, so the source is fully imported"
        );
    }

    #[test]
    fn keep_files_blocks_wipe_on_success() {
        let o = classify_completed_run(5, 0, CLEAN_EXIT, 0, true, 5, 0);
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
        let mut staged =
            staging::create_staging_dir(&selected, None, None, &AtomicU64::new(0)).unwrap();
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
            CLEAN_EXIT,
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
        let mut staged =
            staging::create_staging_dir(&selected, None, None, &AtomicU64::new(0)).unwrap();
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
            CLEAN_EXIT,
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
        let mut staged =
            staging::create_staging_dir(&selected, None, None, &AtomicU64::new(0)).unwrap();
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
        let o = classify_completed_run(0, 3, CLEAN_EXIT, 0, false, 0, 0);
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
        let o = classify_completed_run(5, 0, CLEAN_EXIT, 0, false, 0, 0);
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
        let o = classify_completed_run(0, 0, CLEAN_EXIT, 0, false, 0, 0);
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
        let o = classify_completed_run(9, 0, CLEAN_EXIT, 0, false, 9, 1);
        assert!(!is_failed(&o), "the files that did upload still count");
        assert!(o.wipe_eligible, "verified uploads remain deletable");
        assert!(
            !o.checkpoint_eligible,
            "an unreadable source must not be marked fully imported"
        );
    }

    #[test]
    fn a_clean_full_run_earns_the_checkpoint() {
        let o = classify_completed_run(12, 3, CLEAN_EXIT, 0, false, 15, 0);
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
                    sequence: 0,
                    volume_ids: HashMap::new(),
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
        let preserved = finalize_job(late_completion, None);
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
            volume_ids: HashMap::new(),
            sequence: 7,
        };
        let result = wipe::WipeResult {
            deleted: 1,
            failed: 1,
            skipped: 1,
            changed: 1,
            unprovable: 0,
            failed_paths: vec!["/tmp/failed.jpg".to_string()],
            errors: vec!["failed".to_string()],
        };

        let retry = retry_pending_wipe(pending, result.failed_paths).expect("failed file retries");
        assert_eq!(retry.paths, vec!["/tmp/failed.jpg"]);
        assert_eq!(
            retry.sequence, 7,
            "a retry is the same offer and keeps its place in the bound's queue"
        );
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
            1,
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
            2,
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
        let returned = finalize_job(failed, None);
        assert!(matches!(returned.status, JobStatus::Cancelled));
        let stored = get_job(&job_id).expect("the cancelled job must remain stored");
        assert!(matches!(stored.status, JobStatus::Cancelled));

        lock_jobs().retain(|job| job.id != job_id);
        if let Ok(mut pending) = PENDING_WIPE.lock() {
            pending.remove(&job_id);
        }
    }

    fn replayable_input(profile_id: &str) -> ImportInput {
        ImportInput {
            profile_id: profile_id.to_string(),
            source_paths: vec!["/Volumes/CARD".to_string()],
            album_ids: Vec::new(),
            keep_files: true,
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
        }
    }

    /// A blocking task that cannot be interrupted must not hold the caller.
    /// This is the shape of a dead SMB/NFS mount: the walk never returns, so
    /// only abandoning the join bounds the wait.
    #[test]
    fn a_blocked_task_is_abandoned_when_its_bound_passes() {
        let release = Arc::new(AtomicBool::new(false));
        let release_in_task = release.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tauri::async_runtime::spawn_blocking(move || {
            while !release_in_task.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let outcome = tauri::async_runtime::block_on(join_bounded(
            task,
            Duration::from_millis(150),
            cancel.as_ref(),
            CANCEL_ABANDON_GRACE,
            None,
        ))
        .expect("an abandoned join is not a join failure");

        assert!(matches!(outcome, BoundedJoin::TimedOut));
        // The bound must NOT raise the user's cancel flag: callers publish a
        // cancelled run when it is set, so a dead mount would be reported as
        // something the user asked for. The caller's own walk deadline is what
        // stops a merely slow walk.
        assert!(
            !cancel.load(Ordering::Relaxed),
            "a timeout is not a user cancellation"
        );
        // Let the leaked thread finish so it does not outlive the test binary.
        release.store(true, Ordering::Relaxed);
    }

    /// A cancel must free the caller even when the task cannot notice it. The
    /// import worker holds the liveness markers app quit waits on, so "cancel
    /// then quit" has to work in seconds, not at the staging deadline.
    #[test]
    fn a_cancelled_task_that_never_returns_is_abandoned_after_the_grace() {
        let release = Arc::new(AtomicBool::new(false));
        let release_in_task = release.clone();
        let cancel = Arc::new(AtomicBool::new(true));
        let task = tauri::async_runtime::spawn_blocking(move || {
            while !release_in_task.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let started = Instant::now();
        let outcome = tauri::async_runtime::block_on(join_bounded(
            task,
            Duration::from_secs(60),
            cancel.as_ref(),
            Duration::from_millis(120),
            None,
        ))
        .expect("an abandoned join is not a join failure");

        assert!(matches!(outcome, BoundedJoin::Abandoned));
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the cancel, not the bound, must release the caller"
        );
        release.store(true, Ordering::Relaxed);
    }

    /// `import_cancel` raises the cancel flag before it writes `Cancelled`, so a
    /// staging failure can land in that gap. The worker must publish the cancel
    /// it observed rather than a `Failed` that the cancel then overwrites: the
    /// history record is written from this state and cannot be corrected later.
    #[test]
    fn a_staging_exit_that_saw_the_cancel_flag_records_a_cancelled_run() {
        // Still `Running` in JOBS, exactly as it is before `import_cancel`'s
        // `set_job` lands, so `finalize_job` cannot preserve anything.
        let job_id = format!("staging-cancel-race-{}", Uuid::new_v4());
        let mut running = terminal_job(&job_id, false);
        running.status = JobStatus::Running;
        lock_jobs().push(running);

        // The worker's own candidate is `Failed`; the raised cancel flag, read
        // under the `JOBS` lock, is what turns it into the published cancel.
        let cancel = AtomicBool::new(true);
        let published = finalize_job(
            ImportJob {
                status: JobStatus::Failed,
                error: Some("Could not stage selected files".to_string()),
                progress: JobProgress {
                    total: 0,
                    uploaded: 0,
                    duplicates: 0,
                    errors: 1,
                },
                ..terminal_job(&job_id, false)
            },
            Some(&cancel),
        );
        let record = run_history_record(
            &published,
            RunRecord {
                started_at: 0,
                source_paths: Vec::new(),
                request: replayable_input("p1"),
            },
            // The staging exit reports a fault; a cancelled run must drop it.
            true,
        );

        assert!(matches!(published.status, JobStatus::Cancelled));
        assert_eq!(record.status.as_wire(), "cancelled");
        assert_eq!(record.errors, 0, "a cancelled run reports no error count");
        assert!(
            !record.incomplete,
            "the user stopped this run, so nothing failed to happen that was still expected"
        );

        lock_jobs().retain(|job| job.id != job_id);
    }

    /// A staging walk claims its source roots on the walking task, so a walk
    /// abandoned on a dead mount keeps the claim. Without it, every retry spawns
    /// another permanently blocked thread.
    #[test]
    fn a_second_staging_walk_of_a_held_root_is_refused_by_name() {
        let root = format!("/Volumes/CARD-{}", Uuid::new_v4());
        let roots = vec![root.clone()];
        let held = media_scanner::acquire_scan_roots(media_scanner::ScanPurpose::Stage, &roots)
            .expect("the first staging walk claims the root");

        let refused = media_scanner::acquire_scan_roots(media_scanner::ScanPurpose::Stage, &roots)
            .expect_err("a held staging root must refuse the next walk");
        assert!(
            refused.contains(&root),
            "the refusal names the source: {refused}"
        );

        // A scan of the same root claims a different namespace and is unaffected.
        let scanning = media_scanner::acquire_scan_roots(media_scanner::ScanPurpose::Scan, &roots)
            .expect("a scan must not be blocked by a staging claim");

        drop(scanning);
        drop(held);
    }

    /// The delete runs on an already terminal job, so neither worker map covers
    /// it. Without this mark the app reports "nothing is live" while it hashes a
    /// card and moves originals to the Trash, and a quit abandons the delete
    /// with the payload already consumed.
    #[test]
    fn a_running_wipe_counts_as_live_work() {
        let job_id = format!("wipe-live-{}", Uuid::new_v4());
        {
            let _guard = ActiveWipeGuard::new(job_id.clone());
            assert!(
                has_active_wipe(),
                "a wipe in flight must keep shutdown waiting"
            );
            assert!(ACTIVE_WIPES
                .lock()
                .expect("wipe state is readable")
                .contains_key(&job_id));
        }
        assert!(
            !ACTIVE_WIPES
                .lock()
                .expect("wipe state is readable")
                .contains_key(&job_id),
            "every exit from the wipe, including an error return, must release the mark"
        );
    }

    /// A partly failed delete puts its retry payload back before the first call
    /// returns, so a second confirmation for the SAME job can be in flight while
    /// the first still runs. Whichever finishes first must not clear the mark the
    /// other one is relying on.
    #[test]
    fn overlapping_wipes_for_one_job_keep_the_mark_until_the_last_finishes() {
        let job_id = format!("wipe-overlap-{}", Uuid::new_v4());
        let first = ActiveWipeGuard::new(job_id.clone());
        let second = ActiveWipeGuard::new(job_id.clone());

        drop(first);
        assert!(
            ACTIVE_WIPES
                .lock()
                .expect("wipe state is readable")
                .contains_key(&job_id),
            "the second delete is still running, so the job stays live work"
        );

        drop(second);
        assert!(!ACTIVE_WIPES
            .lock()
            .expect("wipe state is readable")
            .contains_key(&job_id));
    }

    /// A cancel that loses the race to the worker's own terminal write must not
    /// relabel the run. The worker has already written a history record from the
    /// state it published, and that record cannot be corrected afterwards, so the
    /// card has to keep agreeing with it.
    #[test]
    fn a_cancel_cannot_relabel_a_run_that_already_finished() {
        let job_id = format!("cancel-too-late-{}", Uuid::new_v4());
        let mut finished = terminal_job(&job_id, false);
        finished.status = JobStatus::Completed;
        finished.summary = Some("Upload completed.".to_string());
        lock_jobs().push(finished);

        let published = set_job_if_active(cancelled_state(terminal_job(&job_id, false)))
            .expect("job state is readable");

        assert!(!published, "a finished run must refuse the late cancel");
        let stored = get_job(&job_id).expect("the finished job must remain stored");
        assert!(matches!(stored.status, JobStatus::Completed));
        assert_eq!(stored.summary.as_deref(), Some("Upload completed."));

        lock_jobs().retain(|job| job.id != job_id);
    }

    /// The same publish over a live run lands, so a cancel of an import that is
    /// still working keeps its authority.
    #[test]
    fn a_cancel_publishes_over_a_running_job() {
        let job_id = format!("cancel-running-{}", Uuid::new_v4());
        let mut running = terminal_job(&job_id, true);
        running.status = JobStatus::Running;
        lock_jobs().push(running);

        let published = set_job_if_active(cancelled_state(terminal_job(&job_id, true)))
            .expect("job state is readable");

        assert!(published);
        let stored = get_job(&job_id).expect("the cancelled job must remain stored");
        assert!(matches!(stored.status, JobStatus::Cancelled));
        assert_eq!(stored.summary.as_deref(), Some(CANCELLED_SUMMARY));
        assert!(
            !stored.awaiting_wipe_confirmation,
            "a cancelled run must never offer a delete prompt"
        );

        lock_jobs().retain(|job| job.id != job_id);
    }

    /// A responsive task still wins: a cancel it notices returns its own error,
    /// which is what names the outcome on the queue card.
    #[test]
    fn a_task_that_returns_within_the_grace_reports_its_own_result() {
        let cancel = Arc::new(AtomicBool::new(true));
        let cancel_in_task = cancel.clone();
        let task = tauri::async_runtime::spawn_blocking(move || {
            if cancel_in_task.load(Ordering::Relaxed) {
                return Err("Staging cancelled".to_string());
            }
            Ok(())
        });

        let outcome = tauri::async_runtime::block_on(join_bounded(
            task,
            Duration::from_secs(60),
            cancel.as_ref(),
            CANCEL_ABANDON_GRACE,
            None,
        ))
        .expect("a returning task is not a join failure");

        match outcome {
            BoundedJoin::Finished(result) => {
                assert_eq!(result.unwrap_err(), "Staging cancelled");
            }
            _ => panic!("a task that returns must not be reported as abandoned"),
        }
    }

    /// A run cancelled or failed during staging must still leave a receipt, and
    /// that receipt must report the status the user was shown. Before this,
    /// staging exits returned without appending history at all, so the run was
    /// invisible in History and had no request to replay.
    #[test]
    fn a_staging_exit_records_the_published_status_and_keeps_the_request() {
        let job_id = format!("staging-history-{}", Uuid::new_v4());
        let mut cancelled = terminal_job(&job_id, false);
        cancelled.status = JobStatus::Cancelled;
        lock_jobs().push(cancelled);

        // What `finish_staging_exit` publishes for a staging failure raised on a
        // run the user already cancelled.
        let mut failed = terminal_job(&job_id, false);
        failed.status = JobStatus::Failed;
        let published = finalize_job(failed, None);
        let record = run_history_record(
            &published,
            RunRecord {
                started_at: 1_000,
                source_paths: vec!["/Volumes/CARD".to_string()],
                request: replayable_input("p1"),
            },
            true,
        );

        assert_eq!(record.status.as_wire(), "cancelled");
        assert_eq!(record.id, job_id);
        assert_eq!(record.started_at, 1_000);
        assert!(
            record.request.is_some(),
            "History replays a run from its persisted request"
        );

        lock_jobs().retain(|job| job.id != job_id);
    }

    /// A staging failure on a run nobody cancelled is recorded as failed.
    #[test]
    fn a_failed_staging_exit_records_a_failed_run() {
        let mut failed = terminal_job("staging-failed-record", false);
        failed.status = JobStatus::Failed;
        let record = run_history_record(
            &failed,
            RunRecord {
                started_at: 0,
                source_paths: Vec::new(),
                request: replayable_input("p1"),
            },
            true,
        );

        assert_eq!(record.status.as_wire(), "failed");
        assert!(
            record.incomplete,
            "a run that never reached the sidecar records the fault"
        );
    }

    /// Admission and shutdown must answer "is an import live?" the same way.
    /// A cancel publishes a terminal status while the worker still writes
    /// history, so only the finalizing half of the answer keeps a second run
    /// out during that window.
    #[test]
    fn admission_refuses_while_a_worker_is_only_finalizing() {
        assert_eq!(admission_block_reason(false, false, false), None);
        assert_eq!(
            admission_block_reason(false, true, false),
            Some(IMPORT_FINISHING_ERROR),
            "a finalizing worker must still block a new import"
        );
        // A live run outranks finalization: the user is told to stop that run,
        // not to wait a moment.
        assert_eq!(
            admission_block_reason(true, true, false),
            Some(IMPORT_RUNNING_ERROR)
        );
    }

    /// The process-global check must observe `FINALIZING_IMPORTS` too. Only the
    /// blocking direction is asserted: sibling tests publish their own jobs in
    /// parallel, so "not blocked" is never this test's answer to give.
    #[test]
    fn a_finalizing_worker_blocks_admission_process_wide() {
        let job_id = format!("admission-finalizing-{}", Uuid::new_v4());
        lock_finalizing().insert(job_id.clone());

        let blocked = import_admission_block().expect("admission state is readable");
        lock_finalizing().remove(&job_id);

        assert!(
            blocked.is_some(),
            "a registered finalizing worker must block a new import"
        );
    }

    /// The last line of defence for the single-active-import invariant. The
    /// check in `import_admission_block` runs before the keychain read and the
    /// forecast, so two starts can both pass it; only this insert, holding the
    /// maps, can refuse the second. Asserts the refusal AND that nothing was
    /// published: a partially admitted job would leave a card in the queue with
    /// no worker behind it. Sibling tests publish their own jobs in parallel, so
    /// which of the two sentences comes back is not this test's business.
    #[test]
    fn a_racing_insert_is_refused_without_publishing_the_job() {
        let job_id = format!("insert-race-{}", Uuid::new_v4());
        let blocker = format!("insert-race-blocker-{}", Uuid::new_v4());
        lock_finalizing().insert(blocker.clone());

        let refused = insert_initial_job(
            terminal_job(&job_id, false),
            replayable_input("p1"),
            Arc::new(AtomicBool::new(false)),
        );
        lock_finalizing().remove(&blocker);

        let err = refused.expect_err("a live finalizing worker must refuse the insert");
        assert!(
            err == IMPORT_RUNNING_ERROR || err == IMPORT_FINISHING_ERROR,
            "the insert must refuse with an admission sentence, got: {err}"
        );
        assert!(
            !lock_jobs().iter().any(|job| job.id == job_id),
            "a refused insert must not publish the job"
        );
        assert!(
            !JOB_INPUTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&job_id),
            "a refused insert must not retain the request"
        );
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
        // `shutdown.ts` and `backendErrors.ts` substring-match these to decide
        // whether app quit may proceed, and whether a late cancel is a won race
        // rather than a failure. A reword here silently changes quit behaviour,
        // and shutdown.test.ts cannot catch it because it builds its own strings.
        assert_eq!(JOB_NOT_FOUND_ERROR, "Job not found:");
        assert_eq!(TERMINAL_CANCEL_ERROR, "Cannot cancel a terminal import");
        assert_eq!(IMPORT_NOT_RUNNING_ERROR, "Import is no longer running:");

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

    #[test]
    fn only_a_single_album_run_can_be_deep_linked() {
        // The picker sends its selected album name even in folder and tag modes,
        // where immich-go ignores it and spreads assets across many albums. A
        // link there would name one album the run never exclusively populated.
        for organization in [
            Organization::FolderName,
            Organization::FolderPath,
            Organization::FolderTags,
        ] {
            assert_eq!(
                album_link_target(organization, Some("Holiday")),
                None,
                "{organization:?} fans out and must not claim one album"
            );
        }

        assert_eq!(
            album_link_target(Organization::SingleAlbum, Some(" Holiday ")),
            Some("Holiday".to_string())
        );
        assert_eq!(album_link_target(Organization::SingleAlbum, None), None);
        assert_eq!(
            album_link_target(Organization::SingleAlbum, Some("  ")),
            None
        );
    }

    /// PARTIAL-RUN-LOOKS-CLEAN: one photo uploads, immich-go then reports an
    /// aggregate ERR with no `file=` (the source could not be enumerated) and
    /// exits non-zero. The landed asset clears `failed`, so the run stays
    /// `Completed` — that status policy is kept — but the omitted work has to be
    /// visible. Before this, the terminal error was only built from the stderr
    /// tail when the run was `Failed`, and the summary was formatted from the
    /// per-file tallies alone, so the card read exactly "Upload completed. 1
    /// uploaded, 0 duplicates, 0 errors." with NO error, and History recorded
    /// the same. The user could reformat the card on the strength of that.
    #[test]
    fn a_landed_run_with_a_scan_error_and_a_bad_exit_cannot_look_clean() {
        let root = "/Volumes/CARD";
        let roots = vec![root.to_string()];
        let log = format!(
            "2026-06-24 16:10:00 INF uploaded successfully file={root}:DCIM/IMG_0001.JPG\n\
             2026-06-24 16:10:01 ERR DCIM/SUB: file does not exist\n"
        );

        let run = crate::services::stdout_parser::parse_run_progress(&log, &roots);
        let file_errors = crate::services::stdout_parser::parse_error_log(&log, &roots);
        assert_eq!(run.progress.uploaded, 1, "one asset landed");
        assert_eq!(run.scan_errors, 1, "and one source could not be read");
        assert!(
            file_errors.is_empty(),
            "an aggregate ERR names no file, so it is not a per-file error"
        );

        let classification = classify_completed_run(
            run.progress.uploaded,
            run.progress.duplicates,
            BAD_EXIT,
            file_errors.len(),
            true,
            1,
            run.scan_errors,
        );
        assert!(
            matches!(classification.status, JobStatus::Completed),
            "the landed asset keeps the existing partial-success status policy"
        );
        assert!(
            !classification.checkpoint_eligible,
            "an unreadable source must not raise the only-new date floor"
        );

        let faults = aggregate_fault_reasons(RunFaultInputs {
            outcome: BAD_EXIT,
            scan_errors: run.scan_errors,
            stderr_tail: Some("immich-go: cannot read DCIM/SUB"),
            ..clean_fault_inputs()
        });
        let evidence = terminal_evidence(RunEvidenceInputs {
            failed: false,
            progress: run.progress.clone(),
            file_error_count: file_errors.len(),
            faults: &faults,
            keep_files: true,
            awaiting_wipe_confirmation: false,
            pending_wipe_store_failed: false,
        });

        let error = evidence
            .error
            .expect("an aggregate fault must set a non-empty terminal error");
        assert!(
            error.contains("immich-go: cannot read DCIM/SUB"),
            "the stderr tail is the only account of the bad exit: {error}"
        );
        assert!(
            error.contains("never offered to the server"),
            "the unreadable source must be named: {error}"
        );
        let summary = evidence
            .summary
            .expect("a completed run still publishes a summary");
        assert!(
            summary.contains("This run did not finish cleanly"),
            "the summary is what the card shows for a Completed run: {summary}"
        );

        let mut job = terminal_job("partial-looks-clean", false);
        job.progress = run.progress.clone();
        job.error = Some(error);
        job.summary = Some(summary);
        let record = run_history_record(
            &job,
            RunRecord {
                started_at: 0,
                source_paths: roots,
                request: replayable_input("p1"),
            },
            !faults.is_empty(),
        );
        assert!(
            record.incomplete,
            "the receipt must not present this run as clean"
        );
    }

    /// The same failure with NO aggregate scan error and no per-file ERR line:
    /// every tally is clean and the non-zero exit is the only evidence there is.
    /// It still has to reach the user, and the run's disposition sentence must
    /// survive alongside it.
    #[test]
    fn a_landed_run_that_exits_non_zero_with_no_file_error_still_reports_it() {
        let faults = aggregate_fault_reasons(RunFaultInputs {
            outcome: BAD_EXIT,
            stderr_tail: Some("panic: runtime error"),
            ..clean_fault_inputs()
        });
        assert_eq!(faults.len(), 1, "the bad exit is the whole fault");

        let evidence = terminal_evidence(RunEvidenceInputs {
            failed: false,
            progress: JobProgress {
                total: 3,
                uploaded: 3,
                duplicates: 0,
                errors: 0,
            },
            file_error_count: 0,
            faults: &faults,
            keep_files: false,
            awaiting_wipe_confirmation: true,
            pending_wipe_store_failed: false,
        });

        let error = evidence
            .error
            .expect("a non-zero exit must set an error with no per-file error to lean on");
        assert!(error.contains("panic: runtime error"), "got: {error}");
        let summary = evidence.summary.expect("a completed run has a summary");
        assert!(
            summary.contains("Awaiting wipe confirmation."),
            "the delete prompt sentence must survive the fault note: {summary}"
        );
        assert!(
            summary.contains("This run did not finish cleanly"),
            "got: {summary}"
        );
    }

    /// `RunOutcome::Unknown` means termination was never confirmed. There is
    /// strictly LESS evidence here than in an observed failure, so it must never
    /// be read as success: it sets terminal evidence like a failure does, and it
    /// makes the run ineligible for the incremental checkpoint even when every
    /// tally in the log is spotless.
    #[test]
    fn an_unproven_termination_is_never_read_as_success() {
        let classification = classify_completed_run(9, 0, RunOutcome::Unknown, 0, true, 9, 0);
        assert!(
            matches!(classification.status, JobStatus::Completed),
            "nine assets landed, so the partial-success policy still applies"
        );
        assert!(
            !classification.checkpoint_eligible,
            "an unproven run must never raise the date floor"
        );
        // The same tallies with an OBSERVED clean exit do earn it, so the
        // refusal above comes from the outcome and nothing else.
        assert!(classify_completed_run(9, 0, CLEAN_EXIT, 0, true, 9, 0).checkpoint_eligible);

        let faults = aggregate_fault_reasons(RunFaultInputs {
            outcome: RunOutcome::Unknown,
            stderr_tail: Some("dial tcp: connection reset"),
            ..clean_fault_inputs()
        });
        assert_eq!(
            faults.len(),
            1,
            "the unproven termination is itself the fault"
        );
        let evidence = terminal_evidence(RunEvidenceInputs {
            failed: false,
            progress: JobProgress {
                total: 9,
                uploaded: 9,
                duplicates: 0,
                errors: 0,
            },
            file_error_count: 0,
            faults: &faults,
            keep_files: true,
            awaiting_wipe_confirmation: false,
            pending_wipe_store_failed: false,
        });
        let error = evidence
            .error
            .expect("an unproven run must carry terminal evidence");
        assert!(error.contains("never reported that it stopped"), "{error}");
        assert!(
            error.contains("dial tcp: connection reset"),
            "the reap diagnostic is the only account of WHY it could not be confirmed: {error}"
        );
        assert!(evidence
            .summary
            .is_some_and(|summary| summary.contains("This run did not finish cleanly")));
        assert!(
            !classify_completed_run(9, 0, RunOutcome::Unknown, 0, false, 9, 0).wipe_eligible,
            "a process that may still be uploading must not offer its originals for deletion"
        );
    }

    /// A multi-path run is one job, so the worst path decides it. `Unknown`
    /// outranks an observed failure, and one clean path can never launder
    /// another path's bad ending.
    #[test]
    fn the_worst_upload_path_decides_the_run_outcome() {
        assert_eq!(worse_outcome(CLEAN_EXIT, CLEAN_EXIT), CLEAN_EXIT);
        assert_eq!(worse_outcome(CLEAN_EXIT, BAD_EXIT), BAD_EXIT);
        assert_eq!(worse_outcome(BAD_EXIT, CLEAN_EXIT), BAD_EXIT);
        assert_eq!(
            worse_outcome(CLEAN_EXIT, RunOutcome::Unknown),
            RunOutcome::Unknown
        );
        assert_eq!(
            worse_outcome(RunOutcome::Unknown, CLEAN_EXIT),
            RunOutcome::Unknown
        );
        assert_eq!(
            worse_outcome(BAD_EXIT, RunOutcome::Unknown),
            RunOutcome::Unknown
        );
    }

    /// A hand-picked selection that only partly staged ran against fewer files
    /// than the user asked for, and the run log cannot say so: the sidecar was
    /// never given the missing files. Those failures must reach the job's error
    /// list, its error tally, and the history receipt, so a partly attempted
    /// selection can never publish an error-free summary.
    #[test]
    fn a_partly_staged_selection_reports_the_files_it_never_sent() {
        let tmp = std::env::temp_dir().join(format!("import-staged-partial-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let staged_source = tmp.join("kept.jpg");
        std::fs::write(&staged_source, b"kept").unwrap();
        let never_existed = tmp.join("gone.jpg");
        let selected = vec![
            staged_source.to_string_lossy().into_owned(),
            never_existed.to_string_lossy().into_owned(),
        ];

        let mut staged = staging::create_staging_dir(&selected, None, None, &AtomicU64::new(0))
            .expect("one file staged, so staging succeeds");
        let requested = staged.requested;
        assert_eq!(requested, 2, "both selections were asked for");
        let failures = std::mem::take(&mut staged.failures);
        staging::cleanup_staging_dir(staged);
        assert_eq!(failures.len(), 1, "exactly the missing file failed");
        assert_eq!(failures[0].source, selected[1]);

        // What the worker does with them.
        let file_errors: Vec<FileError> = failures
            .iter()
            .map(|failure| FileError {
                file: failure.source.clone(),
                reason: failure.message.clone(),
            })
            .collect();
        let progress = JobProgress {
            total: 1,
            uploaded: 1,
            duplicates: 0,
            errors: failures.len() as u32,
        };
        let classification =
            classify_completed_run(1, 0, CLEAN_EXIT, file_errors.len(), true, 1, 0);
        assert!(
            !classification.checkpoint_eligible,
            "files that were never sent must not be treated as imported"
        );

        let faults = aggregate_fault_reasons(RunFaultInputs {
            unstaged: failures.len(),
            requested,
            ..clean_fault_inputs()
        });
        let evidence = terminal_evidence(RunEvidenceInputs {
            failed: false,
            progress: progress.clone(),
            file_error_count: file_errors.len(),
            faults: &faults,
            keep_files: true,
            awaiting_wipe_confirmation: false,
            pending_wipe_store_failed: false,
        });
        let summary = evidence.summary.expect("a completed run has a summary");
        assert!(
            summary.contains("1 uploaded, 0 duplicates, 1 errors"),
            "the unstaged file is counted: {summary}"
        );
        assert!(
            summary.contains("1 of 2 selected file(s)"),
            "requested vs staged must be named: {summary}"
        );
        assert!(evidence.error.is_some_and(
            |error| error.contains("gone.jpg") || error.contains("could not be prepared")
        ));

        let mut job = terminal_job("staged-partial", false);
        job.progress = progress;
        job.file_errors = file_errors;
        let record = run_history_record(
            &job,
            RunRecord {
                started_at: 0,
                source_paths: vec![tmp.to_string_lossy().into_owned()],
                request: replayable_input("p1"),
            },
            !faults.is_empty(),
        );
        assert_eq!(
            record.errors, 1,
            "the receipt counts the file that never went"
        );
        assert!(record.incomplete);

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// PROMPT-RETENTION: an unanswered delete prompt pins every candidate source
    /// path of its run and, by design, refuses both eviction and dismissal, so
    /// without a bound they accumulate for the life of the process. The bound is
    /// only safe because it withdraws the OFFER: the payload is the sole handle
    /// a delete has, so dropping one leaves every source file on disk.
    #[test]
    fn outstanding_wipe_prompts_are_bounded_and_dropping_one_deletes_nothing() {
        let source = std::env::temp_dir().join(format!("import-bound-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&source).unwrap();
        let candidate = source.join("photo.jpg");
        std::fs::write(&candidate, b"photo").unwrap();

        // A local map, not the process-global one: siblings publish their own
        // prompts in parallel, and the policy is what is under test.
        let mut pending: HashMap<String, PendingWipe> = HashMap::new();
        let oldest = "bound-oldest".to_string();
        pending.insert(
            oldest.clone(),
            PendingWipe {
                paths: vec![candidate.to_string_lossy().into_owned()],
                server_url: "https://example.invalid".to_string(),
                volume_ids: HashMap::new(),
                sequence: 0,
            },
        );
        for index in 1..=MAX_PENDING_WIPE_PROMPTS {
            pending.insert(
                format!("bound-{index}"),
                PendingWipe {
                    paths: Vec::new(),
                    server_url: "https://example.invalid".to_string(),
                    volume_ids: HashMap::new(),
                    sequence: index as u64,
                },
            );
        }

        let dropped = bound_pending_wipes(&mut pending, &HashSet::new());
        assert_eq!(
            dropped,
            vec![oldest],
            "the oldest unanswered offer is the one withdrawn"
        );
        assert_eq!(pending.len(), MAX_PENDING_WIPE_PROMPTS, "the bound holds");
        assert!(
            candidate.exists(),
            "withdrawing an offer must never delete a source file"
        );
        assert!(
            bound_pending_wipes(&mut pending, &HashSet::new()).is_empty(),
            "exactly at the bound, nothing is withdrawn"
        );

        std::fs::remove_dir_all(&source).unwrap();
    }

    /// The other half of the bound: a job left advertising a prompt with no
    /// payload behind it can be neither answered, dismissed, nor evicted, so
    /// withdrawing the payload MUST also take the prompt off the card.
    #[test]
    fn a_withdrawn_prompt_leaves_a_clearable_job_that_offers_no_delete() {
        let job_id = format!("bound-withdraw-{}", Uuid::new_v4());
        let mut job = terminal_job(&job_id, true);
        job.summary = Some("Upload completed. Awaiting wipe confirmation.".to_string());
        lock_jobs().push(job);
        assert!(
            !is_clearable(&get_job(&job_id).expect("the job is stored")),
            "an awaiting job starts unclearable"
        );

        withdraw_wipe_prompts(std::slice::from_ref(&job_id));

        let stored = get_job(&job_id).expect("the job stays in the queue");
        assert!(!stored.awaiting_wipe_confirmation);
        assert_eq!(stored.pending_wipe_count, 0);
        assert!(
            stored
                .summary
                .as_deref()
                .is_some_and(|summary| summary.contains("kept on disk")),
            "the card must say the files are still there: {:?}",
            stored.summary
        );
        assert!(
            is_clearable(&stored),
            "a payload-less prompt must not strand the card forever"
        );
        // And the job is dismissible again, which an awaiting job refuses.
        tauri::async_runtime::block_on(import_dismiss(job_id.clone()))
            .expect("a job with no prompt is dismissible");
        assert!(get_job(&job_id).is_err());
    }

    /// The preflight sheet can be left while the preview scan behind it is still
    /// walking the card, so the sheet's cleanup cannot use `scan_cancel`: that
    /// raises both flags and would empty the picker the user is still choosing
    /// from. `forecast_cancel` touches only the forecast.
    #[test]
    fn forecast_cancel_raises_only_the_forecast_flag() {
        let _slot = forecast_slot_guard();
        let scan = Arc::new(AtomicBool::new(false));
        let forecast = Arc::new(AtomicBool::new(false));
        *ACTIVE_SCAN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(scan.clone());
        *ACTIVE_FORECAST
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ActiveForecast {
            generation: 41,
            cancellation: forecast.clone(),
        });

        tauri::async_runtime::block_on(forecast_cancel(41))
            .expect("cancelling a forecast never fails");
        assert!(
            forecast.load(Ordering::Relaxed),
            "the forecast is cancelled"
        );
        assert!(
            !scan.load(Ordering::Relaxed),
            "the unrelated preview scan must keep running"
        );

        // `scan_cancel` deliberately keeps stopping both: it is the "let go of
        // the card" action, not the sheet's cleanup.
        tauri::async_runtime::block_on(scan_cancel()).expect("cancelling a scan never fails");
        assert!(scan.load(Ordering::Relaxed));

        ACTIVE_SCAN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        ACTIVE_FORECAST
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        tauri::async_runtime::block_on(forecast_cancel(41))
            .expect("no active forecast is not an error");
    }
    /// Serializes every test that installs or reads the single active-forecast
    /// slot, which is process-global.
    fn forecast_slot_guard() -> std::sync::MutexGuard<'static, ()> {
        static FORECAST_SLOT_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
        FORECAST_SLOT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn install_forecast(generation: u64, cancellation: Arc<AtomicBool>) {
        ACTIVE_FORECAST
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(ActiveForecast {
                generation,
                cancellation,
            });
    }

    fn clear_forecast_slot() {
        ACTIVE_FORECAST
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    /// Driven through the command against a real installed slot: the pure
    /// generation comparison keeps passing even if `forecast_cancel` stops
    /// consulting it, so the comparison alone defends nothing.
    #[test]
    fn stale_forecast_generation_cannot_cancel_the_active_forecast() {
        let _slot = forecast_slot_guard();
        let live = Arc::new(AtomicBool::new(false));
        install_forecast(9, live.clone());

        tauri::async_runtime::block_on(forecast_cancel(8)).expect("a stale cancel is not an error");
        assert!(
            !live.load(Ordering::Relaxed),
            "a stale generation must never cancel the forecast that is running"
        );

        tauri::async_runtime::block_on(forecast_cancel(9)).expect("the live generation cancels");
        assert!(live.load(Ordering::Relaxed));
        clear_forecast_slot();
    }

    /// SLOT-OWNERSHIP: the generation arrives over IPC and two callers can send
    /// the same one. A guard that cleared the slot on a generation match would
    /// let the FIRST forecast's guard remove the SECOND forecast's live entry,
    /// after which `forecast_cancel` finds nothing to cancel and the second
    /// keeps hashing the card with no way to stop it.
    #[test]
    fn a_forecast_guard_cannot_clear_a_slot_it_did_not_install() {
        let _slot = forecast_slot_guard();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));

        install_forecast(7, first.clone());
        let first_guard = ActiveForecastGuard {
            cancellation: first.clone(),
        };
        // The second forecast replaces the slot, with the SAME generation.
        install_forecast(7, second.clone());
        drop(first_guard);

        tauri::async_runtime::block_on(forecast_cancel(7)).expect("cancelling never errors");
        assert!(
            second.load(Ordering::Relaxed),
            "the live forecast must still be reachable by a cancel"
        );
        assert!(
            !first.load(Ordering::Relaxed),
            "the finished forecast must not be re-cancelled"
        );
        clear_forecast_slot();
    }

    /// Answers every request with one body and counts how many it was asked.
    /// A wipe that refuses before the server must leave the count at zero.
    struct ServerStub {
        url: String,
        hits: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl ServerStub {
        fn start(body: &'static str) -> Self {
            use std::io::{Read, Write};

            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
            let url = format!(
                "http://127.0.0.1:{}",
                listener.local_addr().expect("stub address").port()
            );
            listener
                .set_nonblocking(true)
                .expect("stub accepts without blocking");
            let hits = Arc::new(AtomicU64::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = std::thread::spawn({
                let hits = hits.clone();
                let stop = stop.clone();
                move || {
                    while !stop.load(Ordering::Relaxed) {
                        let Ok((mut socket, _)) = listener.accept() else {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        };
                        hits.fetch_add(1, Ordering::Relaxed);
                        let _ = socket.set_nonblocking(false);
                        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                        // Read the head and the body it declares. Answering
                        // while a POST body is still unread reaches the client
                        // as a connection reset instead of a response.
                        let mut request = Vec::new();
                        let mut chunk = [0_u8; 1024];
                        while let Ok(read) = socket.read(&mut chunk) {
                            if read == 0 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..read]);
                            let text = String::from_utf8_lossy(&request).into_owned();
                            let Some(head_len) = text.find("\r\n\r\n").map(|at| at + 4) else {
                                continue;
                            };
                            let declared = text
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())?
                                })
                                .unwrap_or(0);
                            if request.len() >= head_len + declared {
                                break;
                            }
                        }
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes());
                        let _ = socket.flush();
                    }
                }
            });
            Self {
                url,
                hits,
                stop,
                thread: Some(thread),
            }
        }

        fn hits(&self) -> u64 {
            self.hits.load(Ordering::Relaxed)
        }
    }

    impl Drop for ServerStub {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The server's answer for a file it already holds, live (not trashed) —
    /// the only answer that authorizes deleting the local original.
    const DUPLICATE_LIVE_BODY: &str =
        r#"{"results":[{"id":"0","action":"reject","reason":"duplicate","isTrashed":false}]}"#;

    /// Publishes a job awaiting confirmation with `payload` behind it, and the
    /// profile key the confirmation will read.
    fn offer_wipe(job_id: &str, profile_id: &str, payload: PendingWipe) {
        keychain::store_api_key(profile_id, "test-key").expect("the test keychain accepts a key");
        let mut job = terminal_job(job_id, true);
        job.profile_id = profile_id.to_string();
        job.pending_wipe_count = payload.paths.len() as u32;
        lock_jobs().push(job);
        PENDING_WIPE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(job_id.to_string(), payload);
    }

    fn forget_wipe(job_id: &str) {
        lock_jobs().retain(|job| job.id != job_id);
        PENDING_WIPE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(job_id);
    }

    /// A candidate with no recorded identity is refused as hard as a changed
    /// one: there is nothing to compare the card against, so nothing about it
    /// is proven. Asserted through the injected closure, which is the only way
    /// to reach this branch without a payload built by a real run.
    #[test]
    fn a_candidate_with_no_recorded_volume_is_refused() {
        let error =
            recheck_volume_identities(&["/card/photo.jpg".to_string()], &HashMap::new(), |_| {
                Some("disk-a".to_string())
            })
            .expect_err("an unrecorded candidate cannot be proven");
        assert!(error.contains("was not recorded"), "{error}");

        assert!(recheck_volume_identities(
            &["/card/photo.jpg".to_string()],
            &HashMap::from([("/card/photo.jpg".to_string(), "disk-a".to_string())]),
            |_| Some("disk-a".to_string()),
        )
        .is_ok());
    }

    /// Driven through `import_confirm_wipe`: the rejection has to happen inside
    /// the command, before it hashes anything or asks the server, and it has to
    /// leave the offer answerable. Asserting the recheck helper alone would
    /// keep passing if the command stopped calling it.
    #[test]
    fn volume_mismatch_rejects_before_the_bulk_check() {
        let _script = volume_script_guard();
        let _keychain = keychain::test_store::exclusive();
        let source = std::env::temp_dir().join(format!("wipe-mismatch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&source).unwrap();
        let photo = source.join("photo.jpg");
        std::fs::write(&photo, b"photo").unwrap();
        let path = photo.to_string_lossy().into_owned();
        let stub = ServerStub::start(DUPLICATE_LIVE_BODY);

        // The card at this mount now answers with a different identity than the
        // one recorded when the offer was made.
        script_volume_identities(vec![HashMap::from([(path.clone(), "disk-b".to_string())])]);
        let job_id = format!("wipe-mismatch-{}", Uuid::new_v4());
        offer_wipe(
            &job_id,
            &format!("profile-{job_id}"),
            PendingWipe {
                paths: vec![path.clone()],
                server_url: stub.url.clone(),
                volume_ids: HashMap::from([(path.clone(), "disk-a".to_string())]),
                sequence: PENDING_WIPE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            },
        );

        let error = tauri::async_runtime::block_on(import_confirm_wipe(job_id.clone(), true))
            .expect_err("a changed volume must refuse the wipe");

        assert!(
            error.contains("The source volume changed"),
            "the mismatch itself must be named: {error}"
        );
        assert_eq!(
            stub.hits(),
            0,
            "nothing may be hashed or sent to the server once the volume is wrong"
        );
        assert!(photo.exists(), "the original must still be on the card");
        assert!(
            PENDING_WIPE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&job_id),
            "the offer must stay answerable after a refusal"
        );

        clear_volume_identity_script();
        forget_wipe(&job_id);
        std::fs::remove_dir_all(&source).unwrap();
    }

    /// SWAPPED-CARD: the identity recorded at terminalization was last proven
    /// before the keychain read and a whole server round trip. A card swapped
    /// at the same mount inside that window has its OWN files hashed and moved
    /// to the Trash, so the identity must be proven again immediately before
    /// the delete. With only the pre-verification check, this test's file is
    /// trashed.
    #[test]
    fn a_volume_change_after_verification_refuses_the_whole_wipe() {
        let _script = volume_script_guard();
        let _keychain = keychain::test_store::exclusive();
        let source = std::env::temp_dir().join(format!("wipe-swap-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&source).unwrap();
        let photo = source.join("photo.jpg");
        std::fs::write(&photo, b"photo").unwrap();
        let path = photo.to_string_lossy().into_owned();
        let stub = ServerStub::start(DUPLICATE_LIVE_BODY);

        // First batch: the volume the offer recorded, so the wipe proceeds to
        // the server. Second batch: the card was swapped while it waited.
        script_volume_identities(vec![
            HashMap::from([(path.clone(), "disk-a".to_string())]),
            HashMap::from([(path.clone(), "disk-b".to_string())]),
        ]);
        let job_id = format!("wipe-swap-{}", Uuid::new_v4());
        offer_wipe(
            &job_id,
            &format!("profile-{job_id}"),
            PendingWipe {
                paths: vec![path.clone()],
                server_url: stub.url.clone(),
                volume_ids: HashMap::from([(path.clone(), "disk-a".to_string())]),
                sequence: PENDING_WIPE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            },
        );

        let job = tauri::async_runtime::block_on(import_confirm_wipe(job_id.clone(), true))
            .expect("the command reports the refusal on the job, not as an error");

        assert!(
            photo.exists(),
            "a file on a volume that can no longer be proven must never be deleted"
        );
        assert!(stub.hits() > 0, "the server check did run before the swap");
        let error = job.error.expect("the refusal must be visible on the card");
        assert!(
            error.contains("kept for safety"),
            "the user must be told the files were kept: {error}"
        );
        assert!(
            job.awaiting_wipe_confirmation,
            "the offer must be retryable once the right card is back"
        );

        clear_volume_identity_script();
        forget_wipe(&job_id);
        std::fs::remove_dir_all(&source).unwrap();
    }

    /// A confirmation the user already answered holds no payload in the map
    /// while it reads the keychain and calls the server — it took it out — but
    /// a retry it puts back is visible to the next terminalization's bound.
    /// Withdrawing THAT offer drops the verified list under a delete the user
    /// already agreed to.
    #[test]
    fn a_confirmation_in_flight_keeps_its_payload_when_the_bound_runs() {
        let mut pending: HashMap<String, PendingWipe> = HashMap::new();
        let answering = "bound-in-flight".to_string();
        pending.insert(
            answering.clone(),
            PendingWipe {
                paths: vec!["/card/photo.jpg".to_string()],
                server_url: "https://example.invalid".to_string(),
                volume_ids: HashMap::new(),
                sequence: 0,
            },
        );
        for index in 1..=MAX_PENDING_WIPE_PROMPTS {
            pending.insert(
                format!("bound-other-{index}"),
                PendingWipe {
                    paths: Vec::new(),
                    server_url: "https://example.invalid".to_string(),
                    volume_ids: HashMap::new(),
                    sequence: index as u64,
                },
            );
        }

        let in_flight = HashSet::from([answering.clone()]);
        let dropped = bound_pending_wipes(&mut pending, &in_flight);

        assert_eq!(
            dropped,
            vec!["bound-other-1".to_string()],
            "the oldest offer NOT being answered is the one withdrawn"
        );
        assert!(
            pending.contains_key(&answering),
            "a confirmation in flight must keep the payload it is about to retry"
        );
    }

    #[test]
    fn concurrent_wipe_confirmation_keeps_one_owner() {
        let id = format!("wipe-interleaving-{}", Uuid::new_v4());
        let first = WipeConfirmationGuard::acquire(id.clone()).unwrap();
        assert!(WipeConfirmationGuard::acquire(id).is_err());
        drop(first);
    }

    #[test]
    fn unmanifested_log_path_is_excluded_while_manifested_path_remains() {
        let root = std::env::temp_dir().join(format!("manifest-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let accepted = root.join("accepted.jpg");
        let forged = root.join("forged.jpg");
        std::fs::write(&accepted, b"accepted").unwrap();
        std::fs::write(&forged, b"forged").unwrap();
        let manifest = HashSet::from([accepted.canonicalize().unwrap()]);
        let (kept, dropped) = retain_paths_in_manifest(
            vec![
                accepted.to_string_lossy().into_owned(),
                forged.to_string_lossy().into_owned(),
            ],
            Some(&manifest),
        );
        assert_eq!(kept, vec![accepted.to_string_lossy().into_owned()]);
        assert_eq!(dropped, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// SILENT-KEEP: one upload record that resolves and one that does not is
    /// the whole trigger. The unresolved file is kept with no explanation, and
    /// the run used to publish "Upload completed. 1 uploaded" with no fault at
    /// all: the evidence gate withheld the delete prompt and the checkpoint
    /// silently, and nothing reached the card, the error, or the receipt.
    #[test]
    fn unresolved_event_blocks_checkpoint() {
        let root = std::env::temp_dir().join(format!("unresolved-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let landed = root.join("landed.jpg");
        std::fs::write(&landed, b"landed").unwrap();
        let roots = vec![root.to_string_lossy().into_owned()];
        let log = format!(
            "2026-06-24 16:10:00 INF uploaded successfully file={}:landed.jpg\n\
             2026-06-24 16:10:01 INF uploaded successfully file=/elsewhere/unknown.jpg\n",
            root.display()
        );
        let run = crate::services::stdout_parser::parse_run_progress(&log, &roots);
        assert_eq!(run.progress.uploaded, 1);
        assert_eq!(
            run.unresolved_file_events, 1,
            "the second record resolves against no invocation root"
        );

        let (completed, _) = retain_paths_under_sources(run.completed_paths.clone(), &roots);
        let classification = classify_completed_run(
            run.progress.uploaded,
            0,
            CLEAN_EXIT,
            0,
            false,
            completed.len(),
            0,
        );
        let evidence_complete = manifest_evidence_is_complete(true, 0, run.unresolved_file_events);
        assert!(!evidence_complete);
        assert!(
            !(classification.wipe_eligible && evidence_complete),
            "an unaccounted-for upload record must withhold the delete prompt"
        );
        assert!(!(classification.checkpoint_eligible && evidence_complete));

        let faults = aggregate_fault_reasons(RunFaultInputs {
            unresolved_file_events: run.unresolved_file_events,
            ..clean_fault_inputs()
        });
        assert!(
            !faults.is_empty(),
            "the run cannot account for one of its own upload records"
        );
        let evidence = terminal_evidence(RunEvidenceInputs {
            failed: false,
            progress: run.progress.clone(),
            file_error_count: 0,
            faults: &faults,
            keep_files: false,
            awaiting_wipe_confirmation: false,
            pending_wipe_store_failed: false,
        });
        let summary = evidence.summary.expect("a completed run has a summary");
        assert!(
            summary.contains("could not be matched to a source file"),
            "the card must say why a file was kept: {summary}"
        );
        assert!(evidence
            .error
            .is_some_and(|error| error.contains("could not be matched to a source file")));

        let mut job = terminal_job("unresolved-record", false);
        job.progress = run.progress.clone();
        let record = run_history_record(
            &job,
            RunRecord {
                started_at: 0,
                source_paths: roots,
                request: replayable_input("p1"),
            },
            !faults.is_empty(),
        );
        assert!(
            record.incomplete,
            "the receipt must not present this run as clean"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The other three kinds of missing evidence reach the user the same way:
    /// they are faults, not silent gates.
    #[test]
    fn missing_evidence_is_reported_as_a_fault() {
        let unreadable_log = aggregate_fault_reasons(RunFaultInputs {
            log_read_error: Some("Could not read import run log: permission denied"),
            ..clean_fault_inputs()
        });
        assert_eq!(unreadable_log.len(), 1, "{unreadable_log:?}");
        assert!(unreadable_log[0].contains("permission denied"));

        let no_manifest = aggregate_fault_reasons(RunFaultInputs {
            manifest_present: false,
            manifest_error: Some("The source stopped responding while it was being listed."),
            ..clean_fault_inputs()
        });
        assert_eq!(no_manifest.len(), 1, "{no_manifest:?}");
        assert!(
            no_manifest[0].contains("stopped responding"),
            "the reason the listing failed must survive, not be collapsed away: {no_manifest:?}"
        );

        let unmanifested = aggregate_fault_reasons(RunFaultInputs {
            unmanifested_paths: 2,
            ..clean_fault_inputs()
        });
        assert_eq!(unmanifested.len(), 1, "{unmanifested:?}");
        assert!(unmanifested[0].contains("2 uploaded file(s)"));

        assert!(
            aggregate_fault_reasons(clean_fault_inputs()).is_empty(),
            "a run with nothing missing has no fault to report"
        );
    }

    /// Driven through the process-global admission state, not the pure ordering
    /// helper: the lease only defends a source if `import_start`'s own check
    /// reads it.
    #[test]
    fn unconfirmed_sidecar_lease_blocks_admission() {
        assert!(admission_block_reason(false, false, true).is_some());

        let job_id = format!("lease-{}", Uuid::new_v4());
        SESSION_SAFETY_LEASES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(job_id.clone());
        let blocked = import_admission_block().expect("admission state is readable");
        SESSION_SAFETY_LEASES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&job_id);

        assert!(
            blocked.is_some_and(|reason| reason.contains("did not prove its sidecar stopped")),
            "a leased session must refuse a new import by name"
        );
    }

    /// LIVE-PROCESS: an `Ok` result whose process was never reaped is the same
    /// hazard as the unconfirmed-termination error. Continuing the loop starts
    /// a second sidecar on the same card, and the run then publishes a
    /// cancellation with no fault, erasing the evidence entirely.
    #[test]
    fn an_unreaped_run_stops_the_loop_and_is_never_a_clean_cancel() {
        let mut tally = RunTally::new();
        let stop = tally.absorb(Ok(SidecarResult {
            error_lines: vec!["immich-go: broken pipe".to_string()],
            outcome: RunOutcome::Unknown,
            reaped: false,
        }));

        assert!(stop, "an unreaped process must stop the run");
        assert!(
            tally.unconfirmed_termination,
            "the session lease and the cancelled-suppression both read this"
        );
        assert_eq!(tally.outcome, RunOutcome::Unknown);
        assert_eq!(
            tally.error_lines,
            vec!["immich-go: broken pipe".to_string()]
        );
        assert!(
            tally.spawn_error.is_none(),
            "the run reached the sidecar, so it is classified, not a spawn failure"
        );
        // What the worker derives from it: a cancel racing this run can no
        // longer publish `Cancelled` and hide the live process.
        assert!(!(!tally.unconfirmed_termination && (tally.cancelled || true)));

        let mut proven = RunTally::new();
        assert!(
            !proven.absorb(Ok(SidecarResult {
                error_lines: Vec::new(),
                outcome: CLEAN_EXIT,
                reaped: true,
            })),
            "a proven exit lets the next source path run"
        );
        assert!(!proven.unconfirmed_termination);

        let mut errored = RunTally::new();
        assert!(errored.absorb(Err(RunUploadError::UnconfirmedTermination(
            "Could not cancel immich-go sidecar".to_string()
        ))));
        assert!(errored.unconfirmed_termination);
    }

    /// A staging tree is a set of links to the user's originals, and immich-go
    /// follows them. Removing it under a process that was never proven gone
    /// turns a recoverable unknown into a half-uploaded card. The retained
    /// directory keeps its `.lock` held for this process's life, which is
    /// exactly the lease startup pruning reclaims once we exit.
    #[test]
    fn an_unproven_termination_keeps_the_staging_tree() {
        let tmp = std::env::temp_dir().join(format!("import-retain-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("photo.jpg");
        std::fs::write(&source, b"photo").unwrap();
        let selected = vec![source.to_string_lossy().into_owned()];

        let staged =
            staging::create_staging_dir(&selected, None, None, &AtomicU64::new(0)).unwrap();
        let retained_root = staged.path().to_path_buf();
        let retained = release_staging_dir(staged, true);

        assert_eq!(retained.as_ref(), Some(&retained_root));
        assert!(
            retained_root.exists(),
            "the staged links must survive a process that may still be reading them"
        );
        // The same probe `prune_stale_temp_artifacts` makes at startup.
        use fs4::fs_std::FileExt;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(retained_root.join(".lock"))
            .expect("the lease file is still there");
        assert!(
            !lock.try_lock_exclusive().expect("the lease is probeable"),
            "the retained directory must still hold its lease, so pruning leaves it alone"
        );
        drop(lock);

        let proven =
            staging::create_staging_dir(&selected, None, None, &AtomicU64::new(0)).unwrap();
        let proven_root = proven.path().to_path_buf();
        assert_eq!(release_staging_dir(proven, false), None);
        assert!(
            !proven_root.exists(),
            "a proven run still cleans up after itself"
        );

        std::fs::remove_dir_all(&retained_root).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// MOUNT-QUESTION: `volume_identity_for_path` answers only for a mount
    /// POINT, and every wipe candidate is a FILE, so asking it directly could
    /// never prove anything and delete-after-import could never store a prompt.
    ///
    /// The probe is recorded rather than real: whether this host's temp
    /// filesystem HAS a volume id is not the invariant — a CI container's root
    /// filesystem has none — and the mount-root step under test is production
    /// code either way.
    #[test]
    fn wipe_candidates_are_asked_about_their_mount_not_their_own_path() {
        let _script = volume_script_guard();
        clear_volume_identity_script();
        let tmp = std::env::temp_dir().join(format!("wipe-volume-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let photo = tmp.join("photo.jpg");
        std::fs::write(&photo, b"photo").unwrap();
        let paths = vec![photo.to_string_lossy().into_owned()];

        assert!(
            snapshot_wipe_volumes(&paths, device_detector::volume_identity_for_path).is_err(),
            "a file path is not a mount point, so the old question can never be answered"
        );

        let asked: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&asked);
        let volumes = snapshot_wipe_volumes(
            &paths,
            device_detector::file_volume_identity_resolver_probing(move |mount: &Path| {
                recorder
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(mount.to_path_buf());
                Some("volume-id".to_string())
            }),
        )
        .expect("the mount question can be answered");
        assert_eq!(volumes.len(), 1);

        let asked = asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(asked.len(), 1, "one probe per mount, not one per file");
        assert_ne!(asked[0], photo, "the file itself is never the question");
        assert!(
            photo.starts_with(&asked[0]),
            "the question must name a mount the file lives under, not {:?}",
            asked[0]
        );

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Both values are quoted because neither is ours: a server reason carrying
    /// its own ` reason=` used to redraw this line's field boundaries.
    #[test]
    fn the_error_log_line_quotes_values_it_does_not_control() {
        let line = import_error_log_line(
            "job-1",
            &FileError {
                file: "/card/DCIM/a b.jpg".to_string(),
                reason: "upload failed reason=injected file=/etc/passwd".to_string(),
            },
        );
        // Every field boundary the reader can see is now a quote, so the
        // server's own ` reason=` sits inside a value instead of opening a
        // field of its own.
        assert!(
            line.starts_with(r#"import_error job_id=job-1 file="/card/DCIM/a b.jpg" reason=""#),
            "{line}"
        );
        assert!(
            line.contains(r#"reason="upload failed reason=injected file=/etc/passwd""#),
            "the injected text must be delimited, not merged into the line: {line}"
        );
        assert!(line.ends_with('"'), "{line}");

        // A newline in either value would otherwise split one event into two.
        let split = import_error_log_line(
            "job-2",
            &FileError {
                file: "/card/a.jpg".to_string(),
                reason: "failed\nimport_error job_id=job-3".to_string(),
            },
        );
        assert!(!split.contains('\n'), "{split}");
    }

    /// MANIFEST-SCOPE: the manifest answers "what was under this source before
    /// the run", not "what does the preview grid show". immich-go uploads
    /// formats the preview allowlist omits, and a manifest narrower than the
    /// uploader marks every such clip unaccounted for — a permanent fault on
    /// the card and an original that is never offered for deletion.
    #[test]
    fn the_source_manifest_covers_formats_the_preview_grid_hides() {
        let root = std::env::temp_dir().join(format!("manifest-scope-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let clip = root.join("clip.mts");
        let photo = root.join("photo.jpg");
        std::fs::write(&clip, b"clip").unwrap();
        std::fs::write(&photo, b"photo").unwrap();

        let manifest = immutable_source_manifest(
            None,
            &[root.to_string_lossy().into_owned()],
            &AtomicBool::new(false),
            &AtomicU64::new(0),
        )
        .expect("a readable source manifests");

        assert!(
            manifest.contains(&clip.canonicalize().unwrap()),
            "a format immich-go uploads must be in the manifest: {manifest:?}"
        );
        assert!(manifest.contains(&photo.canonicalize().unwrap()));
        let (kept, dropped) =
            retain_paths_in_manifest(vec![clip.to_string_lossy().into_owned()], Some(&manifest));
        assert_eq!(kept.len(), 1, "the uploaded clip stays a wipe candidate");
        assert_eq!(dropped, 0);

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A source that stops answering blocks the manifest walk inside the
    /// kernel, where its own deadline can never reach it. Unbounded, the worker
    /// sits in `RUNNING_IMPORTS` with no upload started and the app cannot
    /// quit. The bound leaves no manifest, which the finalization reports as a
    /// fault and treats as no evidence.
    #[test]
    fn a_source_that_never_answers_cannot_park_the_manifest_walk() {
        let release = Arc::new(AtomicBool::new(false));
        let release_for_walk = release.clone();
        let stuck = tauri::async_runtime::spawn_blocking(move || {
            // Stands in for a `readdir` blocked in the kernel: it reports no
            // progress and cannot be interrupted from outside.
            while !release_for_walk.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(HashSet::new())
        });
        let progress = AtomicU64::new(0);
        let cancel = AtomicBool::new(false);

        let started = Instant::now();
        let (manifest, error) = tauri::async_runtime::block_on(bounded_source_manifest(
            stuck,
            &progress,
            &cancel,
            Duration::from_millis(120),
        ));
        let waited = started.elapsed();
        release.store(true, Ordering::Relaxed);

        assert!(manifest.is_none(), "an abandoned walk proves nothing");
        assert!(
            error.is_some_and(|error| error.contains("stopped responding")),
            "the reason must survive into the run's faults"
        );
        assert!(
            waited < Duration::from_secs(5),
            "the join must be bounded by silence, not by the walk returning: {waited:?}"
        );
    }

    /// Driven through `plan_import_start`, which is everything the command
    /// decides before it touches the keychain or the job maps. Asserting
    /// `normalize_select_files` alone would keep passing if `import_start`
    /// stopped calling it — which is the exact regression this defends.
    #[test]
    fn both_import_commands_read_a_selection_the_same_way() {
        let root = std::env::temp_dir().join(format!("immich-shuttle-select-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("photo.jpg");
        std::fs::write(&file, b"x").unwrap();
        let roots = vec![root.to_string_lossy().into_owned()];
        let selected = file.to_string_lossy().into_owned();

        // No subset: import the whole source.
        assert_eq!(normalize_select_files(None, &roots).unwrap(), None);
        // A subset: exactly these files, once they pass the scope check.
        assert_eq!(
            normalize_select_files(Some(vec![selected.clone()]), &roots).unwrap(),
            Some(vec![selected.clone()])
        );
        // Neither: refused rather than guessed.
        assert_eq!(
            normalize_select_files(Some(Vec::new()), &roots).unwrap_err(),
            EMPTY_SELECTION_ERROR
        );

        // The start boundary reads all three the same way.
        let mut input = replayable_input("p1");
        input.source_paths = roots.clone();
        input.select_files = None;
        assert_eq!(plan_import_start(&input).unwrap().select_files, None);
        input.select_files = Some(vec![selected.clone()]);
        assert_eq!(
            plan_import_start(&input).unwrap().select_files,
            Some(vec![selected])
        );
        input.select_files = Some(Vec::new());
        assert_eq!(
            plan_import_start(&input)
                .err()
                .expect("an explicitly empty selection is refused"),
            EMPTY_SELECTION_ERROR
        );

        // And so does the forecast boundary. Zero `source_paths` keeps this off
        // the process-global approved-root state; the bogus profile id proves
        // the refusal lands before any profile or keychain work.
        let err = tauri::async_runtime::block_on(import_forecast(
            "no-such-profile".to_string(),
            Vec::new(),
            Some(Vec::new()),
            None,
            Vec::new(),
            Vec::new(),
            3,
        ))
        .unwrap_err();
        assert_eq!(err, EMPTY_SELECTION_ERROR);

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `None` is the only value that makes the worker upload the source roots:
    /// staging runs only for `Some`, and `upload_paths` falls back to
    /// `source_paths` only when there is no staging dir. Refusing to turn an
    /// explicitly empty selection into `None` is therefore the whole guard
    /// against a zero-file request importing the entire card — and, with
    /// keep-files off, proposing to wipe it afterwards.
    #[test]
    fn an_empty_selection_never_becomes_a_whole_source_upload() {
        let root =
            std::env::temp_dir().join(format!("immich-shuttle-empty-sel-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.jpg"), b"x").unwrap();
        std::fs::write(root.join("b.mp4"), b"x").unwrap();

        let mut input = replayable_input("p1");
        input.source_paths = vec![root.to_string_lossy().into_owned()];
        input.keep_files = false;
        input.select_files = Some(Vec::new());

        match plan_import_start(&input) {
            Ok(plan) if plan.select_files.is_none() => {
                panic!("an empty selection collapsed into a whole-source import")
            }
            Ok(plan) => panic!(
                "an empty selection became a subset: {:?}",
                plan.select_files
            ),
            Err(e) => assert_eq!(e, EMPTY_SELECTION_ERROR),
        }

        std::fs::remove_dir_all(&root).unwrap();
    }
}
