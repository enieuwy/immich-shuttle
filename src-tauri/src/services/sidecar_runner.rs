use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};
use uuid::Uuid;

use crate::models::job::Organization;
use crate::services::stdout_parser::parse_run_progress;

#[derive(Debug, Clone, Default)]
pub struct SidecarResult {
    /// stderr lines emitted by immich-go (diagnostics for a failed run).
    pub error_lines: Vec<String>,
    /// Whether immich-go exited with a non-zero status.
    pub exit_nonzero: bool,
}

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
}

/// Removes the private per-run config directory (with the api-key file inside)
/// when dropped.
struct TempConfig {
    dir: PathBuf,
    path: PathBuf,
}

impl Drop for TempConfig {
    fn drop(&mut self) {
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
    let mut dir_builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        dir_builder.mode(0o700);
    }
    dir_builder
        .create(&dir)
        .map_err(|e| format!("Could not create immich-go config directory: {e}"))?;
    // Construct the guard before writing so a write failure still cleans up the dir.
    let guard = TempConfig {
        dir: dir.clone(),
        path: dir.join("config.yaml"),
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

/// Read the current run log and emit a progress snapshot to the frontend.
fn emit_progress(app: &AppHandle, job_id: &str, log_path: &Path) {
    let run = parse_run_progress(&std::fs::read_to_string(log_path).unwrap_or_default());
    let current_file = run.completed_paths.last().and_then(|p| {
        Path::new(p)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
    });
    let _ = app.emit(
        "import-progress",
        serde_json::json!({
            "job_id": job_id,
            "progress": run.progress,
            "parsed_progress": run.progress,
            "current_file": current_file,
        }),
    );
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
    let child = Arc::new(tokio::sync::Mutex::new(Some(child)));

    let mut error_lines: Vec<String> = Vec::new();
    let mut exit_nonzero = false;

    // immich-go's --no-ui stdout is a `\r`-refreshed aggregate that never
    // line-flushes through the pipe, so progress is polled from the run log
    // (append-only, written in real time) on a fixed cadence instead.
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if request.cancel_flag.load(Ordering::Relaxed) {
            if let Some(running_child) = child.lock().await.take() {
                let _ = running_child.kill();
            }
            return Err("Cancelled by user".to_string());
        }

        tokio::select! {
            _ = ticker.tick() => {
                emit_progress(&app, &request.job_id, &request.log_path);
            }
            maybe_event = rx.recv() => {
                match maybe_event {
                    None => break,
                    Some(CommandEvent::Stderr(line_bytes)) => {
                        let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                        if !line.is_empty() {
                            error_lines.push(line);
                        }
                    }
                    Some(CommandEvent::Terminated(payload)) => {
                        let _ = child.lock().await.take();
                        exit_nonzero = payload.code.unwrap_or(1) != 0;
                        break;
                    }
                    Some(_) => {}
                }
            }
        }
    }

    // Final authoritative snapshot so the UI lands on the run log's last counts.
    emit_progress(&app, &request.job_id, &request.log_path);

    Ok(SidecarResult {
        error_lines,
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
}
