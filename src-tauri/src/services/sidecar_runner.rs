use std::{
    collections::VecDeque,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::{async_runtime::Receiver, AppHandle, Emitter};
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use uuid::Uuid;

use crate::models::job::Organization;
use crate::services::staging::acquire_dir_lock;

use crate::services::stdout_parser::{ProgressAccumulator, RunProgress};

/// Whether the sidecar's exit status was actually observed.
///
/// A lost termination event is NOT a clean exit. The plugin's `Terminated`
/// event is the only proof that the process was reaped, so its absence leaves
/// the run unproven. `Unknown` must never be read as success: doing so
/// published an interrupted run as a completed one and let the incremental
/// checkpoint advance its date floor past source media the run never
/// processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The process exited and the exit status was observed.
    Exited { success: bool },
    /// Termination was never confirmed: the event channel closed without a
    /// termination event, or a kill was requested and could not be confirmed.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct SidecarResult {
    pub error_lines: Vec<String>,
    pub outcome: RunOutcome,
    /// Whether the sidecar process was proven gone: an observed `Terminated`
    /// event, or a reap the plugin confirmed.
    ///
    /// `reaped == false` means the process may still be reading its source and
    /// uploading, so the caller must keep its source admission lease, leave the
    /// staged files alone, and refuse to admit a retry for that source.
    ///
    /// This is independent of `outcome`: a closed event channel whose reap was
    /// still confirmed leaves the exit status `Unknown` while the process is
    /// provably dead, so both facts have to travel separately.
    pub reaped: bool,
}

/// A sidecar result that did not establish process termination.
///
/// The caller keeps the source admission lease for this process session when
/// this variant occurs. A sidecar without a confirmed reap may still read staged
/// files or upload originals, so it cannot be treated as a normal cancellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunUploadError {
    Cancelled,
    UnconfirmedTermination(String),
    Other(String),
}

impl std::fmt::Display for RunUploadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Cancelled by user"),
            Self::UnconfirmedTermination(error) | Self::Other(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for RunUploadError {}

/// Keep enough stderr context to diagnose a failed run without retaining an
/// unbounded stream from a noisy sidecar.
const MAX_STDERR_LINES: usize = 16;
/// Bound each retained stderr line as well as the number of retained lines.
/// A sidecar can emit arbitrary diagnostics, so neither dimension may grow with
/// the length of an import.
const MAX_STDERR_LINE_CHARS: usize = 512;

#[derive(Debug, Default)]
struct StderrBuffer {
    lines: VecDeque<String>,
}

impl StderrBuffer {
    fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(MAX_STDERR_LINES),
        }
    }

    /// Retain only recent, non-empty diagnostics, with a fixed per-line bound.
    fn push(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        let line = line.chars().take(MAX_STDERR_LINE_CHARS).collect();
        if self.lines.len() == MAX_STDERR_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// The retained lines, newest last. The import command shows the last few
    /// of these in the job's failure message, so they must stay separate lines.
    fn into_vec(self) -> Vec<String> {
        self.lines.into_iter().collect()
    }
}

#[derive(Debug, Clone)]
pub struct UploadRequest {
    pub job_id: String,
    pub server_url: String,
    pub api_key: String,
    pub source_path: String,
    pub log_path: PathBuf,
    /// Paths immich-go is actually invoked against for this run — the temp
    /// staging directory for a hand-picked (staged) import, otherwise the
    /// user's source paths. Threaded into `ProgressAccumulator` so run-log
    /// `file=` values are only trusted (and tallied) when they resolve under
    /// one of these roots; see `fs_path_from_file_attr` in stdout_parser.rs.
    pub log_source_roots: Vec<String>,
    pub device_uuid: String,
    pub cancel_flag: Arc<AtomicBool>,
    pub stack_raw_jpeg: bool,
    pub stack_burst: bool,
    pub date_range: Option<String>,
    pub concurrent_tasks: Option<u32>,
    pub into_album: Option<String>,
    pub organization: Organization,
    pub on_errors: Option<String>,
    pub overwrite: bool,
    pub tags: Vec<String>,
    pub session_tag: bool,
    pub include_type: Option<String>,
    pub include_extensions: Vec<String>,
    pub exclude_extensions: Vec<String>,
}

/// Removes the private per-run config directory (with the api-key file inside)
/// when dropped, unless the run asked for it to be retained.
struct TempConfig {
    dir: PathBuf,
    path: PathBuf,
    lock: Option<fs::File>,
    /// Set when the directory must outlive the run. The config file carries the
    /// API key immich-go was started with and reads through `--config`, so
    /// removing it under a process whose termination was never proven can break
    /// an upload the app has already decided it cannot account for. Startup
    /// pruning reclaims the directory later: this process releases the advisory
    /// lock on drop, which is exactly the evidence `prune_stale_temp_artifacts`
    /// waits for.
    retain: bool,
}

impl TempConfig {
    /// Disarm the cleanup and report the directory that is being left behind,
    /// so the caller can name it in the log line support needs to find it.
    fn persist(&mut self) -> &Path {
        self.retain = true;
        &self.dir
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        drop(self.lock.take());
        if self.retain {
            return;
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Write the immich-go config carrying the API key into a fresh private per-run
/// directory, so it is passed via `--config` instead of `--api-key` on the
/// command line (where it would be visible in the process table). The directory
/// name is random (never the logged job id), created 0700 on unix, and the
/// config file is created with exclusive semantics (`create_new`) at 0600 — a
/// local attacker can neither pre-create nor symlink-hijack the path. The
/// returned guard removes the whole directory when the run finishes.
fn write_api_key_config(api_key: &str) -> Result<TempConfig, String> {
    let dir = std::env::temp_dir().join(format!("immich-shuttle-{}", Uuid::new_v4()));
    #[cfg(unix)]
    let dir_builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut b = fs::DirBuilder::new();
        b.mode(0o700);
        b
    };
    #[cfg(not(unix))]
    let dir_builder = fs::DirBuilder::new();
    dir_builder
        .create(&dir)
        .map_err(|e| format!("Could not create immich-go config directory: {e}"))?;
    let lock = match acquire_dir_lock(&dir) {
        Ok(lock) => lock,
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!("Could not lock immich-go config directory: {e}"));
        }
    };
    // Construct the guard before writing so a write failure still cleans up the dir.
    let guard = TempConfig {
        dir: dir.clone(),
        path: dir.join("config.yaml"),
        lock: Some(lock),
        retain: false,
    };

    let escaped = api_key.replace('\\', "\\\\").replace('"', "\\\"");
    let contents = format!("upload:\n    api-key: \"{escaped}\"\n");

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(&guard.path)
        .map_err(|e| format!("Could not create immich-go config: {e}"))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| format!("Could not write immich-go config: {e}"))?;
    Ok(guard)
}

/// Create the run log file with 0600 permissions on unix before immich-go opens
/// it via `--log-file`, so the persisted log is not world-readable. The run log
/// name embeds a fresh UUID, so it never pre-exists; `create(true)` without
/// truncation leaves an existing file untouched.
fn create_private_log(path: &Path) -> Result<(), String> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .map(|_| ())
        .map_err(|e| format!("Could not create run log: {e}"))
}

/// Reads an append-only run log incrementally: each poll parses only bytes
/// appended since the last read and folds them into a running snapshot, so
/// per-tick work stays proportional to new output rather than total log size
/// (the log grows to many MB on large imports and was previously re-read whole
/// twice per second).
struct ProgressReader {
    log_path: PathBuf,
    offset: u64,
    /// Undecoded bytes trailing the last '\n'; held so a chunk that splits a
    /// multibyte char (non-ASCII filenames) is never decoded mid-sequence.
    carry: Vec<u8>,
    acc: ProgressAccumulator,
}

impl ProgressReader {
    fn new(log_path: PathBuf, log_source_roots: Vec<String>) -> Self {
        Self {
            log_path,
            offset: 0,
            carry: Vec::new(),
            acc: ProgressAccumulator::with_source_paths(&log_source_roots),
        }
    }

    /// Fold newly-appended bytes (through the last complete line) and return a
    /// lightweight view for the current UI update.
    fn poll(&mut self) -> (crate::models::job::JobProgress, Option<&str>) {
        if let Ok(mut file) = fs::File::open(&self.log_path) {
            if file.seek(SeekFrom::Start(self.offset)).is_ok() {
                let mut buf = Vec::new();
                if let Ok(n) = file.read_to_end(&mut buf) {
                    self.offset += n as u64;
                    self.carry.extend_from_slice(&buf);
                    if let Some(last_nl) = self.carry.iter().rposition(|&b| b == b'\n') {
                        let complete: Vec<u8> = self.carry.drain(..=last_nl).collect();
                        self.acc.push_chunk(&String::from_utf8_lossy(&complete));
                    }
                }
            }
        }
        self.acc.progress_view()
    }

    /// Authoritative final snapshot: drain any remaining bytes and flush a
    /// trailing line that never got a newline.
    fn finish(&mut self) -> RunProgress {
        let _ = self.poll();
        if !self.carry.is_empty() {
            let rest = std::mem::take(&mut self.carry);
            self.acc.push_chunk(&String::from_utf8_lossy(&rest));
        }
        self.acc.finish();
        self.acc.snapshot()
    }
}

/// Emit a progress snapshot to the frontend.
fn emit_progress(
    app: &AppHandle,
    job_id: &str,
    progress: &crate::models::job::JobProgress,
    current_path: Option<&str>,
) {
    let current_file = current_path.and_then(|path| {
        Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
    });
    let _ = app.emit(
        "import-progress",
        serde_json::json!({
            "job_id": job_id,
            "progress": progress,
            "current_file": current_file,
        }),
    );
}

/// The sidecar control handle the run loop needs: a one-shot kill.
///
/// `CommandChild::kill` CONSUMES the handle and the plugin installs no killing
/// `Drop`, so a failed kill leaves nothing to retry with — which is exactly
/// why a kill error can never be read as confirmed termination.
///
/// The loop is generic over this trait for one reason: `CommandChild` has no
/// public constructor, so the failed-kill and lost-termination paths are
/// otherwise unreachable from a test. Production always wires it to
/// `CommandChild`, unchanged.
trait KillHandle {
    fn kill(self) -> Result<(), String>;
}

impl KillHandle for CommandChild {
    fn kill(self) -> Result<(), String> {
        CommandChild::kill(self).map_err(|error| format!("could not kill sidecar: {error}"))
    }
}

/// What waiting on the sidecar's lifecycle events established.
#[derive(Debug)]
enum ReapOutcome {
    /// A `Terminated` event arrived, so the plugin's background waiter reaped
    /// the process. `note` carries a kill error that the later termination
    /// explained away — the process had already exited on its own.
    Confirmed { note: Option<String> },
    /// Termination was never observed, so the sidecar may still be uploading.
    /// The caller must not treat the run as cleanly finished.
    Unconfirmed { diagnostic: String },
}

/// The whole reap — the kill attempt included — is bounded by this deadline.
/// The plugin emits `Terminated` as soon as its background waiter's `wait()`
/// returns, so five seconds is far longer than a signalled process needs to be
/// reaped. A kill error does not earn a longer wait: such a process may never
/// exit at all, and an unbounded wait would hang cancellation and app
/// shutdown instead of reporting the run as unproven.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Stop a sidecar and wait for the plugin's background waiter to confirm that
/// it reaped the process. `CommandChild` exposes no `wait`; its `Terminated`
/// event is the lifecycle acknowledgement. If the event channel closes, the
/// plugin API provides no way to positively confirm termination.
///
/// A kill error does NOT end the wait. The kill consumed the handle, so the
/// error means either "already exited" or "still running and unsignalled", and
/// only a `Terminated` event tells the two apart. Returning on the error let
/// the caller finalize the run — wiping the staging tree and admitting a retry
/// — while immich-go could still be uploading.
///
/// The timeout and event-error paths remain distinct because they provide
/// different evidence about the sidecar lifecycle.
async fn kill_and_reap<K: KillHandle>(
    child: &mut Option<K>,
    rx: &mut Receiver<CommandEvent>,
) -> ReapOutcome {
    let Some(running_child) = child.take() else {
        // No handle left: the only path that takes it is a `Terminated` event
        // that was already observed, so termination is already confirmed.
        return ReapOutcome::Confirmed { note: None };
    };
    // Kept, not propagated: the wait below decides what the error meant.
    let kill_error = running_child.kill().err();

    let terminated = tokio::time::timeout(REAP_TIMEOUT, async {
        let mut event_error = None;
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Terminated(_) => return Ok(()),
                CommandEvent::Error(error) => event_error = Some(error),
                _ => {}
            }
        }

        Err(match event_error {
            Some(error) => format!("sidecar failed while waiting to terminate: {error}"),
            None => "sidecar termination could not be confirmed because the event channel closed"
                .to_string(),
        })
    })
    .await
    .unwrap_or_else(|_| Err("timed out waiting for sidecar termination after kill".to_string()));

    match terminated {
        Ok(()) => ReapOutcome::Confirmed { note: kill_error },
        Err(reason) => ReapOutcome::Unconfirmed {
            diagnostic: match kill_error {
                Some(kill_error) => format!("{kill_error}; {reason}"),
                None => reason,
            },
        },
    }
}

/// Collapse a reap into the single diagnostic line a caller reports, so the
/// kill error a confirmed termination explained away is still recorded.
fn reap_diagnostic(outcome: ReapOutcome) -> Option<String> {
    match outcome {
        ReapOutcome::Confirmed { note } => note,
        ReapOutcome::Unconfirmed { diagnostic } => Some(diagnostic),
    }
}

/// Build the immich-go `upload from-folder` argument vector for a run. Pure (no
/// I/O) so the flag mapping — especially the organization-mode -> folder/album/
/// tag flags — is unit-testable. The API key travels in `config_path`, never on
/// the command line.
fn build_upload_args(request: &UploadRequest, config_path: &Path) -> Vec<String> {
    let mut args = vec![
        "upload".to_string(),
        "from-folder".to_string(),
        "--server".to_string(),
        request.server_url.clone(),
        "--config".to_string(),
        config_path.to_string_lossy().to_string(),
        format!(
            "--manage-raw-jpeg={}",
            if request.stack_raw_jpeg {
                "StackCoverRaw"
            } else {
                "NoStack"
            }
        ),
        format!(
            "--manage-burst={}",
            if request.stack_burst {
                "Stack"
            } else {
                "NoStack"
            }
        ),
    ];

    // Organization mode -> immich-go folder/tag flags. Only single-album mode
    // honors --into-album; the folder modes derive albums/tags from the tree and
    // ignore any single-album selection.
    match request.organization {
        Organization::SingleAlbum => {
            args.push("--folder-as-album=NONE".to_string());
            if let Some(album) = request.into_album.as_deref() {
                let album = album.trim();
                if !album.is_empty() {
                    args.push(format!("--into-album={album}"));
                }
            }
        }
        Organization::FolderName => args.push("--folder-as-album=FOLDER".to_string()),
        Organization::FolderPath => {
            args.push("--folder-as-album=PATH".to_string());
            args.push("--album-path-joiner= / ".to_string());
        }
        Organization::FolderTags => {
            args.push("--folder-as-album=NONE".to_string());
            args.push("--folder-as-tags".to_string());
        }
    }

    args.push("--device-uuid".to_string());
    args.push(request.device_uuid.clone());
    args.push("--no-ui".to_string());
    args.push("--log-file".to_string());
    args.push(request.log_path.to_string_lossy().to_string());
    args.push("--log-level".to_string());
    // INFO by default: DEBUG can echo request headers (incl. x-api-key) into the
    // persisted run log. Raise only behind an explicit diagnostics opt-in.
    args.push("INFO".to_string());

    if let Some(range) = request.date_range.as_deref() {
        let range = range.trim();
        if !range.is_empty() {
            args.push(format!("--date-range={range}"));
        }
    }
    if let Some(tasks) = request.concurrent_tasks {
        if tasks >= 1 {
            args.push(format!("--concurrent-tasks={tasks}"));
        }
    }
    if let Some(on_errors) = request.on_errors.as_deref() {
        let on_errors = on_errors.trim();
        if !on_errors.is_empty() {
            args.push(format!("--on-errors={on_errors}"));
        }
    }
    if request.overwrite {
        args.push("--overwrite".to_string());
    }
    for tag in &request.tags {
        let tag = tag.trim();
        if !tag.is_empty() {
            args.push(format!("--tag={tag}"));
        }
    }
    if request.session_tag {
        args.push("--session-tag".to_string());
    }
    if let Some(include_type) = request.include_type.as_deref() {
        let include_type = include_type.trim();
        if !include_type.is_empty() {
            args.push(format!("--include-type={include_type}"));
        }
    }
    if !request.include_extensions.is_empty() {
        args.push(format!(
            "--include-extensions={}",
            request.include_extensions.join(",")
        ));
    }
    if !request.exclude_extensions.is_empty() {
        args.push(format!(
            "--exclude-extensions={}",
            request.exclude_extensions.join(",")
        ));
    }

    args.push(request.source_path.clone());
    args
}

/// Drive one sidecar run to completion: fold lifecycle events, poll the run log
/// on a fixed cadence, and report progress through `emit`.
///
/// Split out of `run_upload` (which supplies the plugin's receiver and child,
/// plus an emitter bound to the `AppHandle`) purely as a test seam: neither
/// `CommandChild` nor `AppHandle` can be constructed here, so the
/// lost-termination and failed-kill paths were otherwise unreachable. The
/// production wiring and behaviour are unchanged.
async fn drive_run<K, E>(
    rx: &mut Receiver<CommandEvent>,
    child: &mut Option<K>,
    cancel_flag: &AtomicBool,
    progress: &mut ProgressReader,
    mut emit: E,
) -> Result<SidecarResult, RunUploadError>
where
    K: KillHandle,
    E: FnMut(&crate::models::job::JobProgress, Option<&str>),
{
    let mut error_lines = StderrBuffer::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // The loop yields both what the run established about the exit status and
    // whether the process itself was proven gone; the caller needs the second
    // fact to decide whether it may release the source admission lease.
    let (outcome, reaped) = loop {
        if cancel_flag.load(Ordering::Relaxed) {
            // An unconfirmed reap is not a cancelled run: the sidecar may still
            // be uploading, so it must surface as an error rather than as the
            // orderly cancellation the caller cleans up after.
            if let ReapOutcome::Unconfirmed { diagnostic } = kill_and_reap(child, rx).await {
                return Err(RunUploadError::UnconfirmedTermination(format!(
                    "Could not cancel immich-go sidecar: {diagnostic}"
                )));
            }
            return Err(RunUploadError::Cancelled);
        }

        tokio::select! {
            _ = ticker.tick() => {
                let (snapshot, current_path) = progress.poll();
                emit(&snapshot, current_path);
            }
            maybe_event = rx.recv() => {
                match maybe_event {
                    None => {
                        // A closed channel yields no exit status, and the plugin
                        // offers no other proof that the process was reaped, so
                        // the run is unproven rather than clean. Reporting
                        // `Unknown` (instead of the `false` a clean exit
                        // produces) keeps the caller from publishing an
                        // interrupted run as complete and from advancing the
                        // incremental checkpoint past media it never processed.
                        // Partial tallies still stand for diagnosis, and the
                        // reap diagnostic is recorded as a stderr line so it
                        // reaches the failure message and the run log through
                        // the same path as immich-go's own output.
                        // The reap decides `reaped` separately: only a
                        // confirmed reap proves the process is gone, and
                        // without that proof the caller has to keep treating it
                        // as a live process that may still be uploading.
                        let reap = kill_and_reap(child, rx).await;
                        let confirmed = matches!(reap, ReapOutcome::Confirmed { .. });
                        error_lines.push(&reap_diagnostic(reap).unwrap_or_else(|| {
                            "sidecar stopped without a termination event".to_string()
                        }));
                        break (RunOutcome::Unknown, confirmed);
                    }

                    Some(CommandEvent::Stderr(line_bytes)) => {
                        let line = String::from_utf8_lossy(&line_bytes);
                        for line in line.lines() {
                            error_lines.push(line);
                        }
                    }

                    Some(CommandEvent::Terminated(payload)) => {
                        let _ = child.take();
                        // A missing code means the process was signalled rather
                        // than exiting on its own; that is a failed run, not a
                        // clean one.
                        break (RunOutcome::Exited { success: payload.code == Some(0) }, true);
                    }
                    Some(CommandEvent::Error(error)) => {
                        // An unconfirmed reap here is the same live-process
                        // hazard the cancel path guards: `Other` lets the caller
                        // finalize the run — wiping the staging tree and
                        // admitting a retry — while immich-go may still be
                        // uploading. Only a confirmed reap earns that.
                        let reap = kill_and_reap(child, rx).await;
                        let confirmed = matches!(reap, ReapOutcome::Confirmed { .. });
                        let detail = reap_diagnostic(reap)
                            .map(|diagnostic| format!("; {diagnostic}"))
                            .unwrap_or_default();
                        let message =
                            format!("immich-go sidecar event failed: {error}{detail}");
                        return Err(if confirmed {
                            RunUploadError::Other(message)
                        } else {
                            RunUploadError::UnconfirmedTermination(message)
                        });
                    }
                    Some(_) => {}
                }
            }
        }
    };

    Ok(SidecarResult {
        error_lines: error_lines.into_vec(),
        outcome,
        reaped,
    })
}

/// Whether the finished run proved that the sidecar process is gone.
///
/// Only a confirmed reap counts. `UnconfirmedTermination` and an `Ok` result
/// with `reaped == false` both leave a process that may still be reading the
/// source and talking to the server, and every other outcome — a clean exit, a
/// cancel whose reap was confirmed, a failure raised before the spawn — has
/// established that no process of ours is left.
fn termination_proven(result: &Result<SidecarResult, RunUploadError>) -> bool {
    match result {
        Ok(result) => result.reaped,
        Err(RunUploadError::UnconfirmedTermination(_)) => false,
        Err(_) => true,
    }
}

/// Decide the fate of the run's private config directory.
///
/// The config file carries the API key immich-go reads through `--config` for
/// the whole run, so it may only be removed once the process is proven gone.
/// Removing it under a possibly-live sidecar would break an upload the app has
/// already decided it cannot account for — precisely the run the safety lease
/// exists to protect. A retained directory is not a leak: this process drops
/// its advisory lock here, so the next startup's `prune_stale_temp_artifacts`
/// reclaims it.
///
/// `log` is injected because the log sink writes into the real user data
/// directory, which a unit test must not touch.
fn settle_temp_config<L>(
    mut config: TempConfig,
    result: &Result<SidecarResult, RunUploadError>,
    log: L,
) where
    L: FnOnce(&str),
{
    if termination_proven(result) {
        // The guard's drop removes the directory, as on every normal run.
        return;
    }

    let retained = config.persist();
    // Named in the log so support (and the user clearing space by hand) can
    // find the directory that startup pruning will collect later.
    log(&format!(
        "sidecar_config_retained reason=unconfirmed_termination path={}",
        retained.display()
    ));
}

pub async fn run_upload(
    app: AppHandle,
    request: UploadRequest,
) -> Result<SidecarResult, RunUploadError> {
    let config = write_api_key_config(&request.api_key).map_err(RunUploadError::Other)?;
    // Pre-create the run log 0600 so immich-go's --log-file output (which can
    // carry an x-api-key header) is not world-readable on shared machines.
    create_private_log(&request.log_path).map_err(RunUploadError::Other)?;
    let args = build_upload_args(&request, &config.path);

    let sidecar = app
        .shell()
        .sidecar("immich-go")
        .map_err(|e| RunUploadError::Other(format!("Could not prepare immich-go sidecar: {e}")))?
        .env("GODEBUG", "netdns=cgo")
        .args(args);

    let (mut rx, child) = sidecar
        .spawn()
        .map_err(|e| RunUploadError::Other(format!("Could not spawn immich-go sidecar: {e}")))?;
    let mut child = Some(child);

    // immich-go's --no-ui stdout is a `\r`-refreshed aggregate that never
    // line-flushes through the pipe, so progress is polled from the run log
    // (append-only, written in real time) on a fixed cadence instead. The reader
    // parses only newly-appended bytes each tick.
    let mut progress =
        ProgressReader::new(request.log_path.clone(), request.log_source_roots.clone());

    let result = drive_run(
        &mut rx,
        &mut child,
        &request.cancel_flag,
        &mut progress,
        |snapshot, current_path| emit_progress(&app, &request.job_id, snapshot, current_path),
    )
    .await;

    // Taken before the error is propagated, because an unproven termination
    // arrives on both the error and the success path and the config must
    // outlive the sidecar in either case.
    settle_temp_config(config, &result, |line| {
        let _ = crate::services::logs::append_log("app.log", line);
    });
    let result = result?;

    // Final authoritative snapshot so the UI lands on the run log's last counts.
    let snapshot = progress.finish();
    emit_progress(
        &app,
        &request.job_id,
        &snapshot.progress,
        snapshot.completed_paths.last().map(String::as_str),
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(organization: Organization, into_album: Option<&str>) -> UploadRequest {
        UploadRequest {
            job_id: "job".to_string(),
            server_url: "https://immich.example.com".to_string(),
            api_key: "secret".to_string(),
            source_path: "/src".to_string(),
            log_path: PathBuf::from("/logs/run.log"),
            log_source_roots: vec!["/src".to_string()],
            device_uuid: "dev".to_string(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            stack_raw_jpeg: false,
            stack_burst: false,
            date_range: None,
            concurrent_tasks: None,
            into_album: into_album.map(str::to_string),
            organization,
            on_errors: None,
            overwrite: false,
            tags: Vec::new(),
            session_tag: false,
            include_type: None,
            include_extensions: Vec::new(),
            exclude_extensions: Vec::new(),
        }
    }

    fn args_for(organization: Organization, into_album: Option<&str>) -> Vec<String> {
        build_upload_args(&request(organization, into_album), Path::new("/cfg.yaml"))
    }

    #[test]
    fn single_album_uses_into_album_and_no_folder_organization() {
        let args = args_for(Organization::SingleAlbum, Some("2026 Weddings"));
        assert!(args.contains(&"--folder-as-album=NONE".to_string()));
        assert!(args.contains(&"--into-album=2026 Weddings".to_string()));
        assert!(!args.iter().any(|a| a == "--folder-as-tags"));
    }

    #[test]
    fn single_album_without_selection_emits_no_into_album() {
        let args = args_for(Organization::SingleAlbum, None);
        assert!(args.contains(&"--folder-as-album=NONE".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--into-album")));
    }

    #[test]
    fn single_album_ignores_blank_into_album() {
        let args = args_for(Organization::SingleAlbum, Some("   "));
        assert!(!args.iter().any(|a| a.starts_with("--into-album")));
    }

    #[test]
    fn folder_name_maps_to_folder_as_album_and_ignores_into_album() {
        let args = args_for(Organization::FolderName, Some("ignored"));
        assert!(args.contains(&"--folder-as-album=FOLDER".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--into-album")));
    }

    #[test]
    fn folder_path_maps_to_path_with_joiner() {
        let args = args_for(Organization::FolderPath, None);
        assert!(args.contains(&"--folder-as-album=PATH".to_string()));
        assert!(args.contains(&"--album-path-joiner= / ".to_string()));
    }

    #[test]
    fn folder_tags_maps_to_tags_flag_without_album() {
        let args = args_for(Organization::FolderTags, Some("ignored"));
        assert!(args.contains(&"--folder-as-tags".to_string()));
        assert!(args.contains(&"--folder-as-album=NONE".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--into-album")));
    }

    #[test]
    fn always_logs_at_info_never_debug_and_ends_with_source() {
        let args = args_for(Organization::SingleAlbum, None);
        // The x-api-key can appear in DEBUG output; the run log must stay INFO.
        let level = args
            .iter()
            .position(|a| a == "--log-level")
            .map(|i| &args[i + 1]);
        assert_eq!(level, Some(&"INFO".to_string()));
        assert!(!args.iter().any(|a| a == "DEBUG"));
        assert_eq!(args.last(), Some(&"/src".to_string()));
    }

    #[test]
    fn resilience_and_tag_flags_absent_by_default() {
        let args = args_for(Organization::SingleAlbum, None);
        assert!(!args.iter().any(|a| a.starts_with("--on-errors")));
        assert!(!args.iter().any(|a| a == "--overwrite"));
        assert!(!args.iter().any(|a| a.starts_with("--tag=")));
        assert!(!args.iter().any(|a| a == "--session-tag"));
    }

    #[test]
    fn emits_on_errors_overwrite_and_tags_when_set() {
        let mut req = request(Organization::SingleAlbum, None);
        req.on_errors = Some("continue".to_string());
        req.overwrite = true;
        req.tags = vec![
            "Trip/Iceland".to_string(),
            "  ".to_string(),
            "client-a".to_string(),
        ];
        req.session_tag = true;
        let args = build_upload_args(&req, Path::new("/cfg.yaml"));
        assert!(args.contains(&"--on-errors=continue".to_string()));
        assert!(args.contains(&"--overwrite".to_string()));
        assert!(args.contains(&"--tag=Trip/Iceland".to_string()));
        assert!(args.contains(&"--tag=client-a".to_string()));
        // Blank tags are dropped, not emitted as empty --tag= args.
        assert_eq!(args.iter().filter(|a| a.starts_with("--tag=")).count(), 2);
        assert!(args.contains(&"--session-tag".to_string()));
    }

    #[test]
    fn filter_flags_absent_by_default() {
        let args = args_for(Organization::SingleAlbum, None);
        assert!(!args.iter().any(|a| a.starts_with("--include-type")));
        assert!(!args.iter().any(|a| a.starts_with("--include-extensions")));
        assert!(!args.iter().any(|a| a.starts_with("--exclude-extensions")));
    }

    #[test]
    fn emits_type_and_extension_filters_when_set() {
        let mut req = request(Organization::SingleAlbum, None);
        req.include_type = Some("VIDEO".to_string());
        req.include_extensions = vec![".mp4".to_string(), ".mov".to_string()];
        req.exclude_extensions = vec![".gif".to_string()];
        let args = build_upload_args(&req, Path::new("/cfg.yaml"));
        assert!(args.contains(&"--include-type=VIDEO".to_string()));
        assert!(args.contains(&"--include-extensions=.mp4,.mov".to_string()));
        assert!(args.contains(&"--exclude-extensions=.gif".to_string()));
    }

    #[test]
    fn stderr_buffer_keeps_recent_bounded_lines_as_separate_entries() {
        let mut buffer = StderrBuffer::new();
        for index in 0..=MAX_STDERR_LINES {
            buffer.push(&format!("line-{index}"));
        }

        let long_line = "x".repeat(MAX_STDERR_LINE_CHARS + 1);
        buffer.push(&long_line);

        let lines = buffer.into_vec();
        // Separate entries, because the import command renders only the last few
        // of them into the job's failure message. Collapsing them into one entry
        // would put the whole bounded buffer in front of the user.
        let expected = (2..=MAX_STDERR_LINES)
            .map(|index| format!("line-{index}"))
            .chain(std::iter::once("x".repeat(MAX_STDERR_LINE_CHARS)))
            .collect::<Vec<_>>();
        assert_eq!(lines, expected);
        assert_eq!(lines.len(), MAX_STDERR_LINES);
        assert!(!lines.iter().any(|line| line == "line-0"));
        assert!(!lines.iter().any(|line| line == &long_line));
    }

    // ---- sidecar lifecycle (`drive_run` / `kill_and_reap`) ----

    /// A kill handle whose result is scripted. `CommandChild` has no public
    /// constructor, so this is the only way to reach the failed-kill and
    /// lost-termination paths; like `CommandChild::kill` it consumes the
    /// handle, so a failure leaves nothing to retry with.
    struct FakeKill {
        result: Result<(), String>,
    }

    impl KillHandle for FakeKill {
        fn kill(self) -> Result<(), String> {
            self.result
        }
    }

    fn kills_ok() -> Option<FakeKill> {
        Some(FakeKill { result: Ok(()) })
    }

    fn kill_fails() -> Option<FakeKill> {
        Some(FakeKill {
            result: Err("could not kill sidecar: os error 3".to_string()),
        })
    }

    /// A fresh run-log directory, used as the invocation root.
    fn log_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sidecar-run-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A reader over a run log holding `contents`, rooted at `dir` exactly as
    /// production roots it at the path immich-go was invoked on.
    fn reader_for(dir: &Path, contents: &str) -> ProgressReader {
        let log_path = dir.join("run.log");
        fs::write(&log_path, contents).unwrap();
        ProgressReader::new(log_path, vec![dir.to_string_lossy().to_string()])
    }

    fn terminated(code: Option<i32>) -> CommandEvent {
        CommandEvent::Terminated(tauri_plugin_shell::process::TerminatedPayload {
            code,
            signal: None,
        })
    }

    /// The regression this enum exists for: a lost `Terminated` event used to
    /// synthesize the same `exit_nonzero: false` a clean exit produces, so an
    /// interrupted run was published as a successful one and its advanced date
    /// floor let the next "only new" import skip unprocessed source media.
    #[tokio::test]
    async fn a_closed_event_channel_is_unknown_not_a_clean_exit() {
        let dir = log_dir();
        let mut progress = reader_for(
            &dir,
            &format!(
                "2026-06-24 16:10:21 INF uploaded successfully file={}:IMG_0001.JPG\n",
                dir.display()
            ),
        );

        let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
        // Closing the channel without a termination event is the whole scenario.
        drop(tx);
        let mut child = kills_ok();

        let result = drive_run(
            &mut rx,
            &mut child,
            &AtomicBool::new(false),
            &mut progress,
            |_, _| {},
        )
        .await
        .expect("a lost termination event is reported, not an error");

        assert_eq!(result.outcome, RunOutcome::Unknown);
        // Nothing proved the process is gone, so the caller must still treat it
        // as live: this is the flag its safety lease hangs on.
        assert!(
            !result.reaped,
            "an unconfirmed reap must not read as a reaped process"
        );
        // The diagnostic must survive: it is the only evidence the user gets.
        assert!(
            result
                .error_lines
                .iter()
                .any(|line| line.contains("event channel closed")),
            "reap diagnostic missing: {:?}",
            result.error_lines
        );
        // Partial tallies still stand for diagnosis.
        assert_eq!(progress.finish().progress.uploaded, 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_observed_exit_status_decides_success() {
        for (code, expected) in [
            (Some(0), RunOutcome::Exited { success: true }),
            (Some(2), RunOutcome::Exited { success: false }),
            // No code means the process was signalled, which is not a clean run.
            (None, RunOutcome::Exited { success: false }),
        ] {
            let dir = log_dir();
            let mut progress = reader_for(&dir, "");
            let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
            tx.send(terminated(code)).await.unwrap();
            let mut child = kills_ok();

            let result = drive_run(
                &mut rx,
                &mut child,
                &AtomicBool::new(false),
                &mut progress,
                |_, _| {},
            )
            .await
            .unwrap();

            assert_eq!(result.outcome, expected, "exit code {code:?}");
            // An observed termination event IS the proof of reaping.
            assert!(result.reaped, "exit code {code:?}");
            // The termination event consumed the handle, so nothing is left to kill.
            assert!(child.is_none());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    /// `CommandChild::kill` consumes the handle and the plugin installs no
    /// killing `Drop`, so returning on the kill error released the run while
    /// immich-go could still be uploading. The wait must continue instead.
    #[tokio::test]
    async fn a_failed_kill_waits_and_a_later_termination_confirms_it() {
        let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
        // The receiver is still pending when the kill fails; the real
        // already-exited race delivers `Terminated` shortly afterwards.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = tx.send(terminated(Some(0))).await;
        });
        let mut child = kill_fails();

        let outcome = kill_and_reap(&mut child, &mut rx).await;

        match outcome {
            ReapOutcome::Confirmed { note } => assert!(
                note.unwrap_or_default().contains("could not kill sidecar"),
                "the kill error must stay on the record"
            ),
            other => panic!("a delivered termination event must confirm the reap: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_kill_without_a_termination_event_stays_unconfirmed() {
        let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
        drop(tx);
        let mut child = kill_fails();

        match kill_and_reap(&mut child, &mut rx).await {
            ReapOutcome::Unconfirmed { diagnostic } => {
                assert!(diagnostic.contains("could not kill sidecar"));
                assert!(diagnostic.contains("event channel closed"));
            }
            other => panic!("an unreaped sidecar must not read as confirmed: {other:?}"),
        }
    }

    /// Caller-visible consequence of the above: an unconfirmed reap must not
    /// pass as the orderly cancellation whose cleanup wipes the staging tree
    /// and admits a retry alongside a sidecar that may still be running.
    #[tokio::test]
    async fn cancelling_with_an_unconfirmed_reap_is_an_error_not_a_clean_cancel() {
        let dir = log_dir();
        let mut progress = reader_for(&dir, "");
        let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
        drop(tx);
        let mut child = kill_fails();

        let error = drive_run(
            &mut rx,
            &mut child,
            &AtomicBool::new(true),
            &mut progress,
            |_, _| {},
        )
        .await
        .expect_err("an unreaped sidecar cannot be reported as a finished run");

        assert!(
            matches!(&error, RunUploadError::UnconfirmedTermination(message) if message.starts_with("Could not cancel immich-go sidecar")),
            "unexpected error: {error}"
        );
        assert_ne!(error, RunUploadError::Cancelled);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A closed channel is not automatically a live process. When the reap was
    /// still confirmed, the exit status stays unknown while the process is
    /// provably gone, so the two facts travel as separate fields and the caller
    /// only keeps the safety lease for the case that needs it.
    #[tokio::test]
    async fn a_closed_channel_with_a_confirmed_reap_reports_a_dead_process() {
        let dir = log_dir();
        let mut progress = reader_for(&dir, "");
        let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
        drop(tx);
        // The only shape a closed channel can still confirm: no handle left,
        // which in production means a `Terminated` event was already observed.
        let mut child: Option<FakeKill> = None;

        let result = drive_run(
            &mut rx,
            &mut child,
            &AtomicBool::new(false),
            &mut progress,
            |_, _| {},
        )
        .await
        .expect("a lost exit status is reported, not an error");

        assert_eq!(result.outcome, RunOutcome::Unknown);
        assert!(
            result.reaped,
            "a confirmed reap is proof of termination even without an exit status"
        );
        assert_eq!(
            result.error_lines,
            vec!["sidecar stopped without a termination event".to_string()]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// `Other` lets the worker finalize the run — wipe the staging tree and
    /// admit a retry — so an event failure whose reap was never confirmed must
    /// be reported as an unproven termination instead, exactly like a cancel.
    #[tokio::test]
    async fn an_event_error_with_an_unconfirmed_reap_is_an_unproven_termination() {
        let dir = log_dir();
        let mut progress = reader_for(&dir, "");
        let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
        tx.send(CommandEvent::Error("pipe broken".to_string()))
            .await
            .unwrap();
        // No termination event follows the failure, so the reap stays unproven.
        drop(tx);
        let mut child = kills_ok();

        let error = drive_run(
            &mut rx,
            &mut child,
            &AtomicBool::new(false),
            &mut progress,
            |_, _| {},
        )
        .await
        .expect_err("an event failure is never a finished run");

        match &error {
            RunUploadError::UnconfirmedTermination(message) => {
                // Both diagnostics stay on the record: what failed, and why the
                // process could not be accounted for afterwards.
                assert!(message.contains("pipe broken"), "{message}");
                assert!(message.contains("event channel closed"), "{message}");
            }
            other => panic!("an unreaped sidecar must not read as a plain failure: {other}"),
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_event_error_with_a_confirmed_reap_stays_a_plain_failure() {
        let dir = log_dir();
        let mut progress = reader_for(&dir, "");
        let (tx, mut rx) = tauri::async_runtime::channel::<CommandEvent>(4);
        tx.send(CommandEvent::Error("pipe broken".to_string()))
            .await
            .unwrap();
        // The reap that follows the failure observes the termination event.
        tx.send(terminated(Some(1))).await.unwrap();
        drop(tx);
        let mut child = kills_ok();

        let error = drive_run(
            &mut rx,
            &mut child,
            &AtomicBool::new(false),
            &mut progress,
            |_, _| {},
        )
        .await
        .expect_err("an event failure is never a finished run");

        assert!(
            matches!(&error, RunUploadError::Other(message) if message.contains("pipe broken")),
            "a proven-dead sidecar is an ordinary failure: {error}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    // ---- the run's private config directory ----

    /// Create a real per-run config directory, settle it against `result`, and
    /// report the directory it used plus everything that was logged.
    fn settle(result: Result<SidecarResult, RunUploadError>) -> (PathBuf, Vec<String>) {
        let config = write_api_key_config("secret").unwrap();
        let dir = config.dir.clone();
        assert!(
            dir.join("config.yaml").exists(),
            "the fixture must start with the api-key config in place"
        );

        let mut logged = Vec::new();
        settle_temp_config(config, &result, |line| logged.push(line.to_string()));
        (dir, logged)
    }

    fn finished(outcome: RunOutcome, reaped: bool) -> Result<SidecarResult, RunUploadError> {
        Ok(SidecarResult {
            error_lines: Vec::new(),
            outcome,
            reaped,
        })
    }

    #[test]
    fn a_clean_run_removes_the_private_config_directory() {
        let (dir, logged) = settle(finished(RunOutcome::Exited { success: true }, true));

        assert!(
            !dir.exists(),
            "a proven-dead sidecar leaves no config to protect"
        );
        assert!(logged.is_empty(), "nothing was retained: {logged:?}");
    }

    /// The config file carries the API key immich-go reads through `--config`
    /// for the whole run. Deleting it under a process the app could not account
    /// for can break the very upload the safety lease exists to protect, so the
    /// directory outlives the run and startup pruning reclaims it later.
    #[test]
    fn an_unproven_termination_keeps_the_private_config_and_logs_its_path() {
        for result in [
            finished(RunOutcome::Unknown, false),
            Err(RunUploadError::UnconfirmedTermination(
                "Could not cancel immich-go sidecar: timed out".to_string(),
            )),
        ] {
            let (dir, logged) = settle(result);

            assert!(
                dir.join("config.yaml").exists(),
                "immich-go may still be reading its --config file"
            );
            assert_eq!(
                logged.len(),
                1,
                "one retention line is expected: {logged:?}"
            );
            assert!(
                logged[0].contains("sidecar_config_retained")
                    && logged[0].contains(&dir.display().to_string()),
                "the retained path must be findable from the log: {}",
                logged[0]
            );

            fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Every other ending established that no process of ours is left: a cancel
    /// whose reap was confirmed, and a failure raised around the run.
    #[test]
    fn a_proven_termination_removes_the_private_config_on_the_error_paths() {
        for error in [
            RunUploadError::Cancelled,
            RunUploadError::Other("Could not spawn immich-go sidecar".to_string()),
        ] {
            let (dir, logged) = settle(Err(error));

            assert!(!dir.exists(), "{} must be cleaned up", dir.display());
            assert!(logged.is_empty(), "nothing was retained: {logged:?}");
        }
    }
}
