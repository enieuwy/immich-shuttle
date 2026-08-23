use std::sync::atomic::{AtomicBool, Ordering};

static QUIT_APPROVED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
/// Returns whether the frontend has completed its import shutdown sequence.
pub(crate) fn quit_is_approved() -> bool {
    QUIT_APPROVED.load(Ordering::Acquire)
}

#[tauri::command]
pub fn app_quit(app: tauri::AppHandle) {
    QUIT_APPROVED.store(true, Ordering::Release);
    app.exit(0);
}
