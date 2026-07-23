use std::{path::Path, sync::mpsc, thread, time::Duration};

use sysinfo::Disks;
use tauri::{AppHandle, Emitter};
use tokio::time::interval;

use crate::models::device::RemovableDevice;

const DCIM_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

fn run_probe_with_timeout(
    probe: impl FnOnce() -> bool + Send + 'static,
    timeout: Duration,
) -> bool {
    let (sender, receiver) = mpsc::sync_channel(1);
    let _ = thread::spawn(move || {
        let _ = sender.send(probe());
    });

    receiver.recv_timeout(timeout).unwrap_or(false)
}

fn has_dcim(mount: &str) -> bool {
    let dcim_path = Path::new(mount).join("DCIM");

    // A filesystem stat can hang on an unavailable network or removable mount.
    // Its JoinHandle is intentionally dropped so stalled probes cannot delay other devices.
    run_probe_with_timeout(move || dcim_path.is_dir(), DCIM_PROBE_TIMEOUT)
}
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
    let candidates = disks
        .list()
        .iter()
        .filter_map(|disk| {
            let mount = disk.mount_point().to_string_lossy().to_string();
            let removable = disk.is_removable();
            if !should_include_mount(&mount, removable) {
                return None;
            }

            Some(RemovableDevice {
                name: disk.name().to_string_lossy().to_string(),
                mount_path: mount,
                total_space: disk.total_space(),
                available_space: disk.available_space(),
                has_dcim: false,
            })
        })
        .collect::<Vec<_>>();

    candidates
        .into_iter()
        .map(|mut device| {
            device.has_dcim = has_dcim(&device.mount_path);
            device
        })
        .collect()
}

pub fn start_polling(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = interval(Duration::from_secs(2));
        let mut last: Option<String> = None;
        loop {
            ticker.tick().await;
            // Disk metadata refresh + is_dir() probes can block for seconds on a
            // sleeping/disconnected drive; keep it off the async executor.
            let current = match tauri::async_runtime::spawn_blocking(list_removable_devices).await {
                Ok(devices) => devices,
                Err(_) => continue,
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn timed_out_probe_does_not_wait_for_the_filesystem_operation() {
        let started = Instant::now();
        let result = run_probe_with_timeout(
            || {
                thread::sleep(Duration::from_millis(250));
                true
            },
            Duration::from_millis(10),
        );

        assert!(!result);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
