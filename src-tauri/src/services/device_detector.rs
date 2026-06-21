use std::{path::Path, time::Duration};

use sysinfo::Disks;
use tauri::{AppHandle, Emitter};
use tokio::time::interval;

use crate::models::device::RemovableDevice;

fn should_include_mount(path: &str, removable: bool) -> bool {
    if removable {
        return true;
    }
    if cfg!(target_os = "macos") {
        return path.starts_with("/Volumes/") && !path.starts_with("/Volumes/Macintosh HD");
    }
    if cfg!(target_os = "linux") {
        return path.starts_with("/media/") || path.starts_with("/mnt/");
    }
    false
}

pub fn list_removable_devices() -> Vec<RemovableDevice> {
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter_map(|disk| {
            let mount = disk.mount_point().to_string_lossy().to_string();
            let removable = disk.is_removable();
            if !should_include_mount(&mount, removable) {
                return None;
            }

            let name = disk.name().to_string_lossy().to_string();
            let has_dcim = Path::new(&mount).join("DCIM").is_dir();

            Some(RemovableDevice {
                name,
                mount_path: mount,
                total_space: disk.total_space(),
                available_space: disk.available_space(),
                has_dcim,
            })
        })
        .collect()
}

pub fn start_polling(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = interval(Duration::from_secs(2));
        let mut last: Option<String> = None;
        loop {
            ticker.tick().await;
            let current = list_removable_devices();
            let serialized = match serde_json::to_string(&current) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if last.as_ref() != Some(&serialized) {
                let _ = app.emit("device-changed", &current);
                last = Some(serialized);
            }
        }
    });
}
