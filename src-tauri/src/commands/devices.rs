use crate::models::device::RemovableDevice;
use crate::services::device_detector;

#[tauri::command]
pub async fn devices_list_removable() -> Result<Vec<RemovableDevice>, String> {
    // Disk metadata refresh + is_dir() probes can block for seconds on a
    // sleeping/disconnected drive; keep it off the async executor, matching the
    // background polling path (see device_detector::start_polling).
    tauri::async_runtime::spawn_blocking(device_detector::list_removable_devices)
        .await
        .map_err(|e| format!("Device enumeration task failed: {e}"))
}
