use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};

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

pub async fn run_upload(app: AppHandle, request: UploadRequest) -> Result<SidecarResult, String> {
    let mut args = vec![
        "upload".to_string(),
        "from-folder".to_string(),
        "--server".to_string(),
        request.server_url.clone(),
        "--api-key".to_string(),
        request.api_key.clone(),
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
        "--folder-as-album=NONE".to_string(),
        "--device-uuid".to_string(),
        request.device_uuid.clone(),
        "--no-ui".to_string(),
        "--log-file".to_string(),
        request.log_path.to_string_lossy().to_string(),
        "--log-level".to_string(),
        "DEBUG".to_string(),
    ];
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
    if let Some(album) = request.into_album.as_deref() {
        let album = album.trim();
        if !album.is_empty() {
            args.push(format!("--into-album={album}"));
        }
    }

    args.push(request.source_path.clone());

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
