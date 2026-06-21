use crate::models::device::RemovableDevice;
use crate::services::device_detector;

#[tauri::command]
pub async fn devices_list_removable() -> Result<Vec<RemovableDevice>, String> {
    Ok(device_detector::list_removable_devices())
}
