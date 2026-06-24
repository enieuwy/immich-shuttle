use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};

use crate::{models::job::JobProgress, services::stdout_parser::parse_line};

#[derive(Debug, Clone)]
pub struct SidecarResult {
    pub progress: JobProgress,
    pub had_error_line: bool,
    pub error_lines: Vec<String>,
    pub completed_asset_paths: Vec<String>,
    pub completed_asset_ids: Vec<String>,
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

    let mut progress = JobProgress {
        total: 0,
        uploaded: 0,
        duplicates: 0,
        errors: 0,
    };
    let mut had_error_line = false;
    let mut error_lines: Vec<String> = Vec::new();
    // Per-file paths/ids come from the run log (parsed in import.rs), not
    // stdout, which in --no-ui carries only the aggregate progress line.
    let completed_asset_paths: Vec<String> = Vec::new();
    let completed_asset_ids: Vec<String> = Vec::new();
    let current_file: Option<String> = None;

    while let Some(event) = rx.recv().await {
        if request.cancel_flag.load(Ordering::Relaxed) {
            if let Some(running_child) = child.lock().await.take() {
                let _ = running_child.kill();
            }
            return Err("Cancelled by user".to_string());
        }

        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes).to_string();
                progress = parse_line(&line, progress);
                let _ = app.emit(
                    "import-progress",
                    serde_json::json!({
                        "job_id": request.job_id,
                        "line": line,
                        "progress": progress,
                        "parsed_progress": progress,
                        "current_file": current_file,
                    }),
                );
            }
            CommandEvent::Stderr(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes).to_string();
                had_error_line = true;
                error_lines.push(line.trim().to_string());
                let _ = app.emit(
                    "import-progress",
                    serde_json::json!({
                        "job_id": request.job_id,
                        "line": line,
                        "progress": progress,
                        "parsed_progress": progress,
                        "current_file": current_file,
                    }),
                );
            }
            CommandEvent::Terminated(payload) => {
                let _ = child.lock().await.take();
                if payload.code.unwrap_or(1) != 0 {
                    return Err(format!(
                        "immich-go sidecar exited with code {}",
                        payload.code.unwrap_or(-1)
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    Ok(SidecarResult {
        progress,
        had_error_line,
        error_lines,
        completed_asset_paths,
        completed_asset_ids,
    })
}
