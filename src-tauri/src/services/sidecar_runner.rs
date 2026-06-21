use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::{process::CommandEvent, ShellExt};

use crate::{
    models::job::JobProgress,
    services::stdout_parser::{parse_line, ParsedLine},
};

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
}

pub async fn run_upload(app: AppHandle, request: UploadRequest) -> Result<SidecarResult, String> {
    let mut args = vec![
        "upload".to_string(),
        "from-folder".to_string(),
        "--server".to_string(),
        request.server_url.clone(),
        "--api-key".to_string(),
        request.api_key.clone(),
        "--manage-raw-jpeg=StackCoverRaw".to_string(),
        "--manage-burst=Stack".to_string(),
        "--folder-as-album=NONE".to_string(),
        "--device-uuid".to_string(),
        request.device_uuid.clone(),
        "--no-ui".to_string(),
        "--log-file".to_string(),
        request.log_path.to_string_lossy().to_string(),
        "--log-level".to_string(),
        "DEBUG".to_string(),
    ];
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
    let mut completed_asset_paths = Vec::new();
    let mut completed_asset_ids = Vec::new();

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
                let ParsedLine {
                    progress: new_progress,
                    has_error,
                    completed_asset_path,
                    completed_asset_id,
                } = parse_line(&line, progress);
                progress = new_progress;
                had_error_line |= has_error;
                if has_error {
                    error_lines.push(line.trim().to_string());
                }
                if let Some(path) = completed_asset_path {
                    completed_asset_paths.push(path);
                }
                if let Some(asset_id) = completed_asset_id {
                    completed_asset_ids.push(asset_id);
                }
                let _ = app.emit(
                    "import-progress",
                    serde_json::json!({
                        "job_id": request.job_id,
                        "line": line,
                        "progress": progress,
                        "parsed_progress": progress,
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
