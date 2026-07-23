use std::{fs, path::Path, time::Duration};

use fs4::fs_std::FileExt;
use uuid::Uuid;

// A cross-process ownership lease is the thorough long-term fix. Until then,
// preserve a full day of staging/config artifacts so another app instance's
// plausibly-live upload cannot be pruned during startup.
const STALE_TEMP_ARTIFACT_AGE: Duration = Duration::from_secs(24 * 60 * 60);
// Run logs are not removed until well beyond any plausible live upload window.
const STALE_RUN_LOG_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

fn is_older_than(path: &Path, age: Duration) -> bool {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .is_some_and(|modified| modified.elapsed().is_ok_and(|elapsed| elapsed >= age))
}

fn is_canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|uuid| uuid.hyphenated().to_string() == value)
}

fn is_temp_artifact_name(name: &str) -> bool {
    ["immich-shuttle-stage-", "immich-shuttle-"]
        .iter()
        .any(|prefix| name.strip_prefix(prefix).is_some_and(is_canonical_uuid))
}

fn is_run_log_name(name: &str) -> bool {
    name.strip_prefix("run-")
        .and_then(|name| name.strip_suffix(".log"))
        .is_some_and(is_canonical_uuid)
}

/// Ownership state of a per-run temp artifact, determined via its `.lock` file.
enum LeaseState {
    /// Another process holds the advisory lock — the artifact is in use.
    Live,
    /// The lock exists but is free: the owning process is gone.
    Released,
    /// No lock file (a legacy artifact predating the lease).
    NoLease,
}

/// Probe a temp artifact's ownership lease. The owning process holds an
/// exclusive advisory lock on `<dir>/.lock` for the artifact's lifetime, so if
/// we can acquire it the owner has already exited.
fn temp_artifact_lease(dir: &Path) -> LeaseState {
    let lock_path = dir.join(".lock");
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(file) => match file.try_lock_exclusive() {
            Ok(true) => LeaseState::Released,
            _ => LeaseState::Live,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LeaseState::NoLease,
        // Can't probe the lock (e.g. permissions) — assume live and keep it.
        Err(_) => LeaseState::Live,
    }
}

fn prune_stale_temp_artifacts() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_temp_artifact_name(name) && path.is_dir() {
            match temp_artifact_lease(&path) {
                // A live owner (another running instance) still holds the lease.
                LeaseState::Live => {}
                // The lease was released — the owner is gone, so remove it now.
                LeaseState::Released => {
                    let _ = fs::remove_dir_all(&path);
                }
                // Legacy artifact with no lease file — fall back to the age grace.
                LeaseState::NoLease => {
                    if is_older_than(&path, STALE_TEMP_ARTIFACT_AGE) {
                        let _ = fs::remove_dir_all(&path);
                    }
                }
            }
        }
    }
}

fn prune_stale_run_logs() {
    let Ok(dir) = crate::services::logs::logs_dir() else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_run_log_name(name) && path.is_file() && is_older_than(&path, STALE_RUN_LOG_AGE) {
            let _ = fs::remove_file(path);
        }
    }
}

fn prune_startup_artifacts() {
    prune_stale_temp_artifacts();
    prune_stale_run_logs();
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "123e4567-e89b-12d3-a456-426614174000";

    #[test]
    fn startup_prune_only_recognizes_exact_artifact_names() {
        assert!(is_temp_artifact_name(&format!("immich-shuttle-{UUID}")));
        assert!(is_temp_artifact_name(&format!(
            "immich-shuttle-stage-{UUID}"
        )));
        assert!(is_run_log_name(&format!("run-{UUID}.log")));

        assert!(!is_temp_artifact_name("immich-shuttle-photos"));
        assert!(!is_temp_artifact_name(&format!(
            "immich-shuttle-{UUID}-backup"
        )));
        assert!(!is_run_log_name(&format!("run-{UUID}.log.old")));
        assert!(!is_run_log_name("run-upload.log"));
    }
}

mod commands;
mod models;
mod services;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            crate::services::device_detector::start_polling(app.handle().clone());
            // Evict stale thumbnails so the on-disk cache can't grow without
            // bound across sessions. Off-thread: it stats/deletes cache files.
            tauri::async_runtime::spawn_blocking(crate::services::thumbnailer::prune_cache);
            // Recover temp staging/config directories and run logs left behind
            // if the prior process stopped before their normal cleanup.
            tauri::async_runtime::spawn_blocking(prune_startup_artifacts);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::profiles::profiles_list,
            commands::profiles::profile_upsert,
            commands::profiles::profile_delete,
            commands::profiles::profile_validate,
            commands::albums::albums_list,
            commands::albums::album_create,
            commands::albums::album_share_users,
            commands::albums::album_share_link,
            commands::import::import_start,
            commands::import::import_confirm_wipe,
            commands::import::import_cancel,
            commands::import::import_list_jobs,
            commands::import::import_retry,
            commands::import::import_dismiss,
            commands::import::import_clear_finished,
            commands::history::history_list,
            commands::history::history_clear,
            commands::history::history_source_last_import,
            commands::import::scan_sources_stream,
            commands::import::scan_cancel,
            commands::preview::preview_thumbnails,
            commands::preview::preview_dates,
            commands::preview::preview_cancel,
            commands::devices::devices_list_removable,
            commands::users::users_list,
            commands::settings::get_server_info,
            commands::settings::get_logs_dir,
            commands::settings::get_recent_logs,
            commands::settings::open_logs_dir
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
