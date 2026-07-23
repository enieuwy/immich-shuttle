use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::{Duration, Instant},
};

use uuid::Uuid;

use crate::{
    models::{
        job::{ImportInput, ImportJob, JobProgress, JobStatus},
        media::ScanResult,
    },
    services::{
        keychain, logs, media_scanner, profile_store,
        sidecar_runner::{run_upload, UploadRequest},
        staging, url_resolver, wipe,
    },
};

static JOBS: LazyLock<Mutex<Vec<ImportJob>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static PENDING_WIPE: LazyLock<Mutex<HashMap<String, PendingWipe>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static RUNNING_IMPORTS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static JOB_INPUTS: LazyLock<Mutex<HashMap<String, ImportInput>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const MAX_RETAINED_TERMINAL_JOBS: usize = 500;
const SCAN_DEADLINE: Duration = Duration::from_secs(60 * 60);

static IMPORT_START_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static ACTIVE_SCAN: LazyLock<Mutex<Option<Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(None));

struct PendingWipe {
    paths: Vec<String>,
    server_url: String,
    api_key: String,
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
    let terminal_count = jobs.iter().filter(|job| is_terminal(&job.status)).count();
    let excess = terminal_count.saturating_sub(MAX_RETAINED_TERMINAL_JOBS);
    if excess == 0 {
        return Vec::new();
    }

    let evicted: HashSet<String> = jobs
        .iter()
        .filter(|job| is_terminal(&job.status))
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

/// Final classification of an import process that ran to completion.
struct RunOutcome {
    status: JobStatus,
    /// Whether the uploaded originals may proceed to the verify-then-delete wipe.
    wipe_eligible: bool,
}

/// Decide the final job status and wipe eligibility from a completed run's
/// tallies. Kept pure (no globals, no I/O) because this is the verify-before-
/// delete safety surface: a regression here could delete originals after a
/// failed run or wrongly withhold deletion.
///
/// A run is a failure only when nothing landed on the server (no uploads, no
/// duplicate matches) AND it ended badly (non-zero exit or per-file errors); a
/// partial run that uploaded or matched duplicates succeeds, surfacing errors.
/// Wipe is eligible only for a successful run with keep-files off and at least
/// one completed path.
fn classify_completed_run(
    uploaded: u32,
    duplicates: u32,
    exit_nonzero: bool,
    file_errors_len: usize,
    keep_files: bool,
    completed_paths_len: usize,
) -> RunOutcome {
    let nothing_landed = uploaded == 0 && duplicates == 0;
    let failed = nothing_landed && (exit_nonzero || file_errors_len > 0);
    let status = if failed {
        JobStatus::Failed
    } else {
        JobStatus::Completed
    };
    let wipe_eligible = !failed && !keep_files && completed_paths_len > 0;
    RunOutcome {
        status,
        wipe_eligible,
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

    let source_paths = input.source_paths.clone();
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

    tauri::async_runtime::spawn(async move {
        let api_key_for_album_assignment = api_key_clone.clone();
        let staging_dir = if staging_requested {
            let selected_files = select_files.clone();
            match tauri::async_runtime::spawn_blocking(move || {
                staging::create_staging_dir(&selected_files)
            })
            .await
            {
                Ok(Ok(dir)) => Some(dir),
                Ok(Err(e)) => {
                    if let Ok(mut running) = RUNNING_IMPORTS.lock() {
                        running.remove(&job_id_clone);
                    }
                    let _ = set_job(ImportJob {
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
                    });
                    return;
                }
                Err(e) => {
                    if let Ok(mut running) = RUNNING_IMPORTS.lock() {
                        running.remove(&job_id_clone);
                    }
                    let _ = set_job(ImportJob {
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
            device_uuid,
            cancel_flag: cancel_flag.clone(),
            stack_raw_jpeg,
            stack_burst,
            date_range,
            concurrent_tasks,
            into_album,
            organization,
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

        if let Ok(mut running) = RUNNING_IMPORTS.lock() {
            running.remove(&job_id_clone);
        }

        // immich-go writes per-file events to the run log (stdout only carries a
        // `\r`-refreshed aggregate that can't be read reliably through the pipe).
        // The log is O_APPEND across multi-path runs, so one read afterwards
        // yields the authoritative totals, completed paths, and per-file errors.
        let log_contents = std::fs::read_to_string(&request.log_path).unwrap_or_default();
        let file_errors = crate::services::stdout_parser::parse_error_log(&log_contents);
        let run = crate::services::stdout_parser::parse_run_progress(&log_contents);
        // parse_run_progress counts every distinct errored file (uncapped);
        // file_errors is capped at MAX_FILE_ERRORS for the UI payload. Keep the
        // true count so the final tally never undercounts a mass-failure run.
        let progress = run.progress;
        // For a staged (selected) import the log's paths point at the temp
        // symlink dir, which is cleaned up below — so wipe must target the user's
        // selected originals instead. SHA-1 verify_uploaded still gates deletion
        // to files the server actually holds, so unuploaded picks are kept safe.
        let completed_asset_paths = if staged_import {
            select_files.clone()
        } else {
            run.completed_paths
        };

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
            }
        } else {
            // The process ran to completion. classify_completed_run owns the
            // failure + verify-before-delete decision so it can be unit-tested in
            // isolation from the async command body.
            let RunOutcome {
                status,
                wipe_eligible,
            } = classify_completed_run(
                progress.uploaded,
                progress.duplicates,
                exit_nonzero,
                file_errors.len(),
                keep_files,
                completed_asset_paths.len(),
            );
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
        let _ = set_job(update.clone());
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
            },
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
    });

    Ok(job_id)
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
                        let kept = wipe_result.failed + wipe_result.skipped + unverified_count;
                        job.summary = Some(format!(
                            "Verified {} of {} files on the server and deleted {}. Kept {} ({} not found on server).",
                            confirmed_count,
                            pending_count,
                            wipe_result.deleted,
                            kept,
                            unverified_count,
                        ));
                        job.error = if wipe_result.failed > 0 {
                            Some(format!(
                                "Wipe completed with errors: deleted={}, failed={}, skipped={}",
                                wipe_result.deleted, wipe_result.failed, wipe_result.skipped
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
                                "import_wipe_verified job_id={} confirmed={} unverified={} deleted={}",
                                job_id, confirmed_count, unverified_count, wipe_result.deleted
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

async fn scan_paths(paths: Vec<String>) -> Result<ScanResult, String> {
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
    let mut scan_task =
        tauri::async_runtime::spawn_blocking(move || -> Result<ScanResult, String> {
            let mut merged = ScanResult {
                files: Vec::new(),
                total_size_bytes: 0,
                photo_count: 0,
                video_count: 0,
                skipped_unreadable: 0,
            };
            for path in paths {
                let result = media_scanner::scan_directory_with_controls(
                    PathBuf::from(path).as_path(),
                    Some(scan_cancellation.as_ref()),
                    Some(deadline),
                )
                .map_err(|error| error.to_string())?;
                merged.files.extend(result.files);
                merged.total_size_bytes += result.total_size_bytes;
                merged.photo_count += result.photo_count;
                merged.video_count += result.video_count;
                merged.skipped_unreadable += result.skipped_unreadable;
            }
            Ok(merged)
        });

    let result = tokio::select! {
        joined = tokio::time::timeout(SCAN_DEADLINE, &mut scan_task) => match joined {
            Ok(result) => result.map_err(|error| format!("Scan task failed: {error}"))?,
            Err(_) => {
                cancellation.store(true, Ordering::Relaxed);
                // Dropping the join handle detaches a worker stuck inside a filesystem
                // syscall; it remains until the OS returns, but the IPC/UI is unblocked.
                Err(format!("Scan timed out after {} seconds", SCAN_DEADLINE.as_secs()))
            }
        },
        _ = async {
            while !cancellation.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } => {
            cancellation.store(true, Ordering::Relaxed);
            Err("Scan superseded by a newer request".to_string())
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
pub async fn scan_source(path: String) -> Result<ScanResult, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("Source path is required".to_string());
    }
    crate::services::source_guard::record_roots(std::slice::from_ref(&path));
    scan_paths(vec![path]).await
}

#[tauri::command]
pub async fn scan_sources(paths: Vec<String>) -> Result<ScanResult, String> {
    if paths.is_empty() {
        return Err("At least one path is required".to_string());
    }
    // The plural scan always receives the user's full current selection, so
    // reset the approved-root scope before recording it. This de-authorizes
    // roots that were removed from the selection (bounds the allowlist too).
    crate::services::source_guard::reset_roots();
    crate::services::source_guard::record_roots(&paths);
    scan_paths(paths).await
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

#[tauri::command]
pub async fn import_clear_finished() -> Result<Vec<ImportJob>, String> {
    let removed_ids: Vec<String> = {
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "Could not lock import job state".to_string())?;
        let removed: Vec<String> = jobs
            .iter()
            .filter(|j| {
                matches!(
                    &j.status,
                    JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
                )
            })
            .map(|j| j.id.clone())
            .collect();
        jobs.retain(|j| matches!(&j.status, JobStatus::Running | JobStatus::Pending));
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
    fn nothing_landed_and_bad_exit_is_failed_not_wipe_eligible() {
        let o = classify_completed_run(0, 0, true, 0, false, 3);
        assert!(is_failed(&o));
        assert!(!o.wipe_eligible, "a failed run must never be wipe-eligible");
    }

    #[test]
    fn nothing_landed_with_file_errors_is_failed() {
        let o = classify_completed_run(0, 0, false, 2, false, 3);
        assert!(is_failed(&o));
        assert!(!o.wipe_eligible);
    }

    #[test]
    fn uploads_present_succeed_despite_errors_and_bad_exit() {
        // A partial run that uploaded something is a success even with per-file
        // errors and a non-zero exit; deletion of the originals stays eligible.
        let o = classify_completed_run(5, 0, true, 4, false, 5);
        assert!(!is_failed(&o));
        assert!(o.wipe_eligible);
    }

    #[test]
    fn duplicates_only_count_as_landed() {
        // Everything was already on the server (all duplicates): success, and the
        // originals are still eligible for deletion.
        let o = classify_completed_run(0, 7, false, 0, false, 7);
        assert!(!is_failed(&o));
        assert!(o.wipe_eligible);
    }

    #[test]
    fn keep_files_blocks_wipe_on_success() {
        let o = classify_completed_run(5, 0, false, 0, true, 5);
        assert!(!is_failed(&o));
        assert!(!o.wipe_eligible, "keep-files must suppress deletion");
    }

    #[test]
    fn no_completed_paths_blocks_wipe() {
        // Success but nothing to delete (e.g. immich-go reported no completed
        // local paths): not wipe-eligible, so we never delete on an empty set.
        let o = classify_completed_run(0, 3, false, 0, false, 0);
        assert!(!is_failed(&o));
        assert!(!o.wipe_eligible);
    }
}
