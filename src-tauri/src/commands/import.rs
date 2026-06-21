use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use once_cell::sync::Lazy;
use uuid::Uuid;

use crate::{
    models::{
        job::{ImportInput, ImportJob, JobProgress, JobStatus},
        media::ScanResult,
    },
    services::{
        immich_client::ImmichClient,
        keychain, logs, media_scanner, profile_store,
        sidecar_runner::{run_upload, SidecarResult, UploadRequest},
        url_resolver, wipe,
    },
};

static JOBS: Lazy<Mutex<Vec<ImportJob>>> = Lazy::new(|| Mutex::new(Vec::new()));
static PENDING_WIPE: Lazy<Mutex<HashMap<String, Vec<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static RUNNING_IMPORTS: Lazy<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn get_job(job_id: &str) -> Result<ImportJob, String> {
    let jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    jobs.iter()
        .find(|j| j.id == job_id)
        .cloned()
        .ok_or_else(|| format!("Job not found: {job_id}"))
}

fn set_job(job: ImportJob) -> Result<(), String> {
    let mut jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    if let Some(existing) = jobs.iter_mut().find(|j| j.id == job.id) {
        *existing = job;
    } else {
        jobs.push(job);
    }
    Ok(())
}

#[tauri::command]
pub async fn import_start(app: tauri::AppHandle, input: ImportInput) -> Result<String, String> {
    if input.source_paths.is_empty() {
        return Err("At least one source path is required".to_string());
    }

    let profile = profile_store::get_profile(&input.profile_id)?;
    let api_key = keychain::get_api_key(&input.profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {}", input.profile_id))?;

    let job_id = Uuid::new_v4().to_string();
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
    };
    set_job(initial)?;
    logs::append_log(
        "app.log",
        &format!(
            "import_start job_id={job_id} profile_id={}",
            input.profile_id
        ),
    )?;

    let source_paths = input.source_paths.clone();
    let keep_files = input.keep_files;
    let stack_raw_jpeg = input.stack_raw_jpeg;
    let stack_burst = input.stack_burst;
    let album_ids = input.album_ids.clone();
    let job_id_clone = job_id.clone();
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let api_key_clone = api_key;
    let app_clone = app.clone();
    let log_path = logs::logs_dir()?.join(format!("run-{job_id}.log"));
    let device_uuid = format!("immich-shuttle-{}", Uuid::new_v4());
    let cancel_flag = Arc::new(AtomicBool::new(false));
    RUNNING_IMPORTS
        .lock()
        .map_err(|_| "Could not lock running imports state".to_string())?
        .insert(job_id.clone(), cancel_flag.clone());

    tauri::async_runtime::spawn(async move {
        let api_key_for_album_assignment = api_key_clone.clone();
        let request = UploadRequest {
            job_id: job_id_clone.clone(),
            server_url,
            api_key: api_key_clone,
            source_path: source_paths[0].clone(),
            log_path,
            device_uuid,
            cancel_flag: cancel_flag.clone(),
            stack_raw_jpeg,
            stack_burst,
        };
        let mut merged_progress = JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        };
        let mut merged_had_error_line = false;
        let mut merged_error_lines: Vec<String> = Vec::new();
        let mut merged_asset_paths = Vec::new();
        let mut merged_asset_ids = Vec::new();

        let mut request = request;
        let mut upload_error: Option<String> = None;

        for path in source_paths {
            request.source_path = path;
            match run_upload(app_clone.clone(), request.clone()).await {
                Ok(run) => {
                    merged_progress.total =
                        merged_progress.total.saturating_add(run.progress.total);
                    merged_progress.uploaded = merged_progress
                        .uploaded
                        .saturating_add(run.progress.uploaded);
                    merged_progress.duplicates = merged_progress
                        .duplicates
                        .saturating_add(run.progress.duplicates);
                    merged_progress.errors =
                        merged_progress.errors.saturating_add(run.progress.errors);
                    merged_had_error_line |= run.had_error_line;
                    merged_error_lines.extend(run.error_lines);
                    merged_asset_paths.extend(run.completed_asset_paths);
                    merged_asset_ids.extend(run.completed_asset_ids);
                }
                Err(err) => {
                    upload_error = Some(err);
                    break;
                }
            }
        }

        let result = if let Some(err) = upload_error {
            Err(err)
        } else {
            Ok(SidecarResult {
                progress: merged_progress,
                had_error_line: merged_had_error_line,
                error_lines: merged_error_lines,
                completed_asset_paths: merged_asset_paths,
                completed_asset_ids: merged_asset_ids,
            })
        };

        if let Ok(mut running) = RUNNING_IMPORTS.lock() {
            running.remove(&job_id_clone);
        }

        let update = match result {
            Ok(SidecarResult {
                progress,
                had_error_line,
                error_lines,
                completed_asset_paths,
                completed_asset_ids,
            }) => ImportJob {
                id: job_id_clone.clone(),
                status: if had_error_line {
                    JobStatus::Failed
                } else {
                    JobStatus::Completed
                },
                progress,
                error: {
                    let mut error: Option<String> = None;
                    if had_error_line {
                        let last_errors: Vec<&str> = error_lines.iter().rev().take(3).map(|s| s.as_str()).collect();
                        let last_errors: Vec<&str> = last_errors.into_iter().rev().collect();
                        if last_errors.is_empty() {
                            error = Some("immich-go reported error output during upload".to_string());
                        } else {
                            error = Some(last_errors.join(" | "));
                        }
                    }
                    if !had_error_line && !album_ids.is_empty() && !completed_asset_ids.is_empty() {
                        let client =
                            ImmichClient::new(&profile.server_url, &api_key_for_album_assignment);
                        let mut failures = Vec::new();
                        for album_id in &album_ids {
                            if let Err(err) = client
                                .add_assets_to_album(album_id, &completed_asset_ids)
                                .await
                            {
                                failures.push(format!("{album_id}: {err}"));
                            }
                        }
                        if !failures.is_empty() {
                            error = Some(format!(
                                "Album assignment failed for {} album(s): {}",
                                failures.len(),
                                failures.join(" | ")
                            ));
                        }
                    }
                    error
                },
                summary: if had_error_line {
                    None
                } else if !keep_files {
                    if let Ok(mut pending) = PENDING_WIPE.lock() {
                        pending.insert(job_id_clone.clone(), completed_asset_paths.clone());
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
                    Some(if album_ids.is_empty() {
                        "Upload completed. Awaiting wipe confirmation.".to_string()
                    } else {
                        format!(
                            "Upload completed. Assigned {} assets to {} album(s). Awaiting wipe confirmation.",
                            completed_asset_ids.len(),
                            album_ids.len()
                        )
                    })
                } else {
                    Some(if album_ids.is_empty() {
                        "Upload completed. Files were kept on disk.".to_string()
                    } else {
                        format!(
                            "Upload completed. Assigned {} assets to {} album(s). Files were kept on disk.",
                            completed_asset_ids.len(),
                            album_ids.len()
                        )
                    })
                },
                awaiting_wipe_confirmation: !had_error_line && !keep_files,
                pending_wipe_count: if had_error_line || keep_files {
                    0
                } else {
                    completed_asset_paths.len() as u32
                },
            },
            Err(err) => {
                let cancelled = err == "Cancelled by user";
                ImportJob {
                    id: job_id_clone.clone(),
                    status: if cancelled {
                        JobStatus::Cancelled
                    } else {
                        JobStatus::Failed
                    },
                    progress: JobProgress {
                        total: 0,
                        uploaded: 0,
                        duplicates: 0,
                        errors: if cancelled { 0 } else { 1 },
                    },
                    error: if cancelled { None } else { Some(err) },
                    summary: if cancelled {
                        Some("Import cancelled by user.".to_string())
                    } else {
                        None
                    },
                    awaiting_wipe_confirmation: false,
                    pending_wipe_count: 0,
                }
            }
        };

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
        let _ = set_job(update);
    });

    Ok(job_id)
}

#[tauri::command]
pub async fn import_confirm_wipe(job_id: String, confirm: bool) -> Result<ImportJob, String> {
    let mut job = get_job(&job_id)?;

    if !job.awaiting_wipe_confirmation {
        return Err(format!("Job does not need wipe confirmation: {job_id}"));
    }

    let files = PENDING_WIPE
        .lock()
        .map_err(|_| "Could not lock pending wipe state".to_string())?
        .remove(&job_id)
        .ok_or_else(|| format!("No pending wipe payload for job: {job_id}"))?;

    if confirm {
        let wipe_result = wipe::wipe_files(&files);
        let kept = wipe_result.failed + wipe_result.skipped;
        job.summary = Some(format!(
            "Deleted {} files. {} files kept (not uploaded or failed).",
            wipe_result.deleted, kept
        ));
        job.error = if wipe_result.failed > 0 {
            Some(format!(
                "Wipe completed with errors: deleted={}, failed={}, skipped={}",
                wipe_result.deleted, wipe_result.failed, wipe_result.skipped
            ))
        } else {
            None
        };
    } else {
        job.summary = Some(format!("Wipe skipped by user. {} files kept.", files.len()));
    }

    job.awaiting_wipe_confirmation = false;
    job.pending_wipe_count = 0;
    set_job(job.clone())?;

    let _ = logs::append_log(
        "app.log",
        &format!(
            "import_wipe_confirmed job_id={} confirm={} remaining_pending={}",
            job_id,
            confirm,
            files.len()
        ),
    );

    Ok(job)
}

#[tauri::command]
pub async fn scan_source(path: String) -> Result<ScanResult, String> {
    media_scanner::scan_directory(PathBuf::from(path).as_path())
}

#[tauri::command]
pub async fn scan_sources(paths: Vec<String>) -> Result<ScanResult, String> {
    if paths.is_empty() {
        return Err("At least one path is required".to_string());
    }
    let mut merged = ScanResult {
        files: Vec::new(),
        total_size_bytes: 0,
        photo_count: 0,
        video_count: 0,
        skipped_unreadable: 0,
    };
    for p in paths {
        let result = media_scanner::scan_directory(PathBuf::from(p).as_path())?;
        merged.files.extend(result.files);
        merged.total_size_bytes += result.total_size_bytes;
        merged.photo_count += result.photo_count;
        merged.video_count += result.video_count;
        merged.skipped_unreadable += result.skipped_unreadable;
    }
    Ok(merged)
}

#[tauri::command]
pub async fn import_cancel(job_id: String) -> Result<(), String> {
    if let Ok(running) = RUNNING_IMPORTS.lock() {
        if let Some(flag) = running.get(&job_id) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    let mut jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    if let Some(existing) = jobs.iter_mut().find(|j| j.id == job_id) {
        existing.status = JobStatus::Cancelled;
        existing.awaiting_wipe_confirmation = false;
        existing.pending_wipe_count = 0;
        existing.error = None;
        existing.summary = Some("Import cancelled by user.".to_string());
        if let Ok(mut pending) = PENDING_WIPE.lock() {
            pending.remove(&job_id);
        }
        return Ok(());
    }
    Err(format!("Job not found: {job_id}"))
}

#[tauri::command]
pub async fn import_list_jobs() -> Result<Vec<ImportJob>, String> {
    let jobs = JOBS
        .lock()
        .map_err(|_| "Could not lock import job state".to_string())?;
    Ok(jobs.clone())
}
