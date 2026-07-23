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

#[derive(Debug, Clone, Default)]
pub struct SidecarResult {
    /// stderr lines emitted by immich-go (diagnostics for a failed run).
    pub error_lines: Vec<String>,
    /// Whether immich-go exited with a non-zero status.
    pub exit_nonzero: bool,
}

/// Keep enough stderr context to diagnose a failed run without retaining an
/// unbounded stream from a noisy sidecar.
const MAX_STDERR_LINES: usize = 16;

#[derive(Debug, Clone)]
pub struct UploadRequest {
    pub job_id: String,
    pub server_url: String,
    pub api_key: String,
    pub source_path: String,
    pub log_path: PathBuf,
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
/// when dropped.
struct TempConfig {
    dir: PathBuf,
    path: PathBuf,
    lock: Option<fs::File>,
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        drop(self.lock.take());
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
    fn new(log_path: PathBuf) -> Self {
        Self {
            log_path,
            offset: 0,
            carry: Vec::new(),
            acc: ProgressAccumulator::new(),
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
            "parsed_progress": progress,
            "current_file": current_file,
        }),
    );
}

/// Stop a sidecar and wait for the plugin's background waiter to confirm that
/// it reaped the process. `CommandChild` exposes no `wait`; its `Terminated`
/// event is the lifecycle acknowledgement.
async fn kill_and_reap(
    child: &mut Option<CommandChild>,
    rx: &mut Receiver<CommandEvent>,
) -> Result<(), String> {
    let Some(running_child) = child.take() else {
        return Ok(());
    };
    running_child
        .kill()
        .map_err(|error| format!("could not kill sidecar: {error}"))?;

    tokio::time::timeout(Duration::from_secs(5), async {
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
            None => "sidecar event channel closed before termination was reported".to_string(),
        })
    })
    .await
    .map_err(|_| "timed out waiting for sidecar termination after kill".to_string())?
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

pub async fn run_upload(app: AppHandle, request: UploadRequest) -> Result<SidecarResult, String> {
    let config = write_api_key_config(&request.api_key)?;
    // Pre-create the run log 0600 so immich-go's --log-file output (which can
    // carry an x-api-key header) is not world-readable on shared machines.
    create_private_log(&request.log_path)?;
    let args = build_upload_args(&request, &config.path);

    let sidecar = app
        .shell()
        .sidecar("immich-go")
        .map_err(|e| format!("Could not prepare immich-go sidecar: {e}"))?
        .env("GODEBUG", "netdns=cgo")
        .args(args);

    let (mut rx, child) = sidecar
        .spawn()
        .map_err(|e| format!("Could not spawn immich-go sidecar: {e}"))?;
    let mut child = Some(child);

    let mut error_lines = VecDeque::with_capacity(MAX_STDERR_LINES);

    // immich-go's --no-ui stdout is a `\r`-refreshed aggregate that never
    // line-flushes through the pipe, so progress is polled from the run log
    // (append-only, written in real time) on a fixed cadence instead. The reader
    // parses only newly-appended bytes each tick.
    let mut progress = ProgressReader::new(request.log_path.clone());
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let exit_nonzero = loop {
        if request.cancel_flag.load(Ordering::Relaxed) {
            kill_and_reap(&mut child, &mut rx)
                .await
                .map_err(|error| format!("Could not cancel immich-go sidecar: {error}"))?;
            return Err("Cancelled by user".to_string());
        }

        tokio::select! {
            _ = ticker.tick() => {
                let (snapshot, current_path) = progress.poll();
                emit_progress(&app, &request.job_id, &snapshot, current_path);
            }
            maybe_event = rx.recv() => {
                match maybe_event {
                    None => {
                        let reap_error = kill_and_reap(&mut child, &mut rx).await.err();
                        let detail = reap_error
                            .unwrap_or_else(|| "sidecar stopped without a termination event".to_string());
                        return Err(format!("immich-go event channel closed unexpectedly: {detail}"));
                    }
                    Some(CommandEvent::Stderr(line_bytes)) => {
                        let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                        if !line.is_empty() {
                            if error_lines.len() == MAX_STDERR_LINES {
                                error_lines.pop_front();
                            }
                            error_lines.push_back(line);
                        }
                    }
                    Some(CommandEvent::Terminated(payload)) => {
                        let _ = child.take();
                        break payload.code.unwrap_or(1) != 0;
                    }
                    Some(CommandEvent::Error(error)) => {
                        let reap_error = kill_and_reap(&mut child, &mut rx).await.err();
                        let detail = reap_error
                            .map(|reap_error| format!("; {reap_error}"))
                            .unwrap_or_default();
                        return Err(format!("immich-go sidecar event failed: {error}{detail}"));
                    }
                    Some(_) => {}
                }
            }
        }
    };

    // Final authoritative snapshot so the UI lands on the run log's last counts.
    let snapshot = progress.finish();
    emit_progress(
        &app,
        &request.job_id,
        &snapshot.progress,
        snapshot.completed_paths.last().map(String::as_str),
    );

    Ok(SidecarResult {
        error_lines: error_lines.into_iter().collect(),
        exit_nonzero,
    })
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
}
