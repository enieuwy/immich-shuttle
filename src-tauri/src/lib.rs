#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::{fs, path::Path, time::Duration};

use fs4::fs_std::FileExt;
#[cfg(target_os = "macos")]
use tauri::Emitter;
use tauri::Manager;
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

#[cfg(target_os = "macos")]
static MAC_APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Whether AppKit may tear the process down right now.
///
/// Split out of the delegate callback so the decision itself is testable
/// without an NSApplication: the callback is an Objective-C entry point.
#[cfg(target_os = "macos")]
fn may_terminate_now() -> bool {
    commands::app::quit_is_approved() || !commands::import::has_live_import_worker()
}

/// Intercept AppKit's application termination callback on tao's live delegate.
///
/// The window close callback cannot see application-menu Cmd-Q, and Tauri's
/// `RunEvent::ExitRequested` does not fire for this AppKit termination path.
/// Replacing the menu item also fails because AppKit keeps `terminate:` bound
/// at the application level. The delegate callback is therefore the only point
/// that can cancel Cmd-Q before AppKit tears down the process.
#[cfg(target_os = "macos")]
fn install_application_termination_guard(app: &tauri::AppHandle) {
    let _ = MAC_APP_HANDLE.set(app.clone());
    use objc2::{
        runtime::{AnyObject, Imp, Sel},
        sel, MainThreadMarker,
    };
    use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};

    unsafe extern "C-unwind" fn application_should_terminate(
        _delegate: *mut AnyObject,
        _selector: Sel,
        _application: *mut AnyObject,
    ) -> NSApplicationTerminateReply {
        if may_terminate_now() {
            return NSApplicationTerminateReply::TerminateNow;
        }

        // Durable, because the user sees only a prompt: this line is what tells
        // support that a quit was deferred rather than ignored.
        let _ = crate::services::logs::append_log(
            "app.log",
            "app_quit_deferred import_worker_live emitting=quit-requested",
        );
        if let Some(app) = MAC_APP_HANDLE.get() {
            let _ = app.emit("quit-requested", ());
        }
        NSApplicationTerminateReply::TerminateCancel
    }

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let application = NSApplication::sharedApplication(mtm);
    let Some(delegate) = application.delegate() else {
        return;
    };
    // The delegate arrives as a protocol object; reborrow it as a plain
    // Objective-C object so its concrete class can be patched below.
    let delegate_object: &AnyObject =
        unsafe { &*objc2::rc::Retained::as_ptr(&delegate).cast::<AnyObject>() };
    let delegate_class = delegate_object.class();
    let selector = sel!(applicationShouldTerminate:);
    let callback: Imp = unsafe {
        std::mem::transmute(
            application_should_terminate
                as unsafe extern "C-unwind" fn(
                    *mut AnyObject,
                    Sel,
                    *mut AnyObject,
                ) -> NSApplicationTerminateReply,
        )
    };

    let added = unsafe {
        objc2::ffi::class_addMethod(
            delegate_class as *const _ as *mut _,
            selector,
            callback,
            c"Q@:@".as_ptr().cast(),
        )
    };
    if !added.as_bool() {
        if let Some(method) = delegate_class.instance_method(selector) {
            // SAFETY: The callback uses the exact Objective-C ABI and return
            // type declared by `applicationShouldTerminate:`.
            unsafe {
                method.set_implementation(callback);
            }
        }
    }
    let _ = crate::services::logs::append_log(
        "app.log",
        &format!("app_quit_guard_installed added={}", added.as_bool()),
    );
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

    /// The quit guard's whole purpose is refusing AppKit while a worker lives.
    /// Only that direction is asserted: sibling import tests register their own
    /// workers concurrently, so "no worker anywhere" is not a state this test
    /// can pin. The idle path (Cmd-Q quits at once) and the Objective-C
    /// plumbing are verified by hand against a real window.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_live_import_worker_refuses_application_termination() {
        let job_id = format!("quit-guard-{UUID}");
        commands::import::mark_worker_live_for_test(&job_id);
        assert!(!may_terminate_now());
        commands::import::clear_worker_live_for_test(&job_id);
    }
}

mod commands;
mod models;
mod services;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // MUST stay the first plugin: it decides whether this process is the
        // owner or a duplicate launch before any other setup runs.
        //
        // Import admission, the profile/history stores, and run-log rotation are
        // all process-local (JOBS/RUNNING_IMPORTS, CONFIG_LOCK/STORE_LOCK,
        // retention by mtime). A second instance would therefore admit a
        // concurrent import of the same card, clobber the other's profile and
        // history writes, and rotate away a live run log. One instance per
        // machine is the invariant those subsystems already assume; enforce it
        // here rather than retrofitting cross-process leases onto each of them.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // A duplicate launch is a request to reach the running window.
            let _ = crate::services::logs::append_log(
                "app.log",
                "duplicate_launch_rejected focusing_existing_window",
            );
            // macOS: makeKeyAndOrderFront alone does not bring a background app
            // forward, so unhide the application before raising its window.
            #[cfg(target_os = "macos")]
            let _ = app.show();
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            install_application_termination_guard(app.handle());
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
            commands::profiles::discover_immich_servers,
            commands::profiles::profile_upsert,
            commands::profiles::profile_delete,
            commands::profiles::profile_validate,
            commands::albums::albums_list,
            commands::albums::album_create,
            commands::albums::album_share_users,
            commands::albums::album_share_link,
            commands::tags::tags_list,
            commands::app::app_quit,
            commands::import::import_start,
            commands::import::import_forecast,
            commands::import::import_confirm_wipe,
            commands::import::import_cancel,
            commands::import::import_await_terminal,
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
            commands::users::user_profile_image,
            commands::settings::get_server_info,
            commands::settings::get_logs_dir,
            commands::settings::get_recent_logs,
            commands::settings::open_logs_dir,
            commands::settings::open_in_immich
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
