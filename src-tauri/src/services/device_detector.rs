use std::{
    collections::HashSet,
    path::Path,
    sync::{mpsc, LazyLock, Mutex},
    thread,
    time::Duration,
};

use sysinfo::Disks;
use tauri::{AppHandle, Emitter};
use tokio::time::interval;

use crate::models::device::RemovableDevice;

const DCIM_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
enum ProbeOutcome {
    Value(bool),
    TimedOut,
    Disconnected,
}

/// Keep channel disconnect separate from timeout: a disconnected sender means
/// the probe thread panicked, so silently treating it as "no DCIM" would hide
/// a detector failure.
fn run_probe_with_timeout_outcome(
    probe: impl FnOnce() -> bool + Send + 'static,
    timeout: Duration,
) -> ProbeOutcome {
    let (sender, receiver) = mpsc::sync_channel(1);
    let _ = thread::spawn(move || {
        let _ = sender.send(probe());
    });

    match receiver.recv_timeout(timeout) {
        Ok(value) => ProbeOutcome::Value(value),
        Err(mpsc::RecvTimeoutError::Timeout) => ProbeOutcome::TimedOut,
        Err(mpsc::RecvTimeoutError::Disconnected) => ProbeOutcome::Disconnected,
    }
}

fn run_probe_with_timeout(
    probe: impl FnOnce() -> bool + Send + 'static,
    timeout: Duration,
) -> bool {
    match run_probe_with_timeout_outcome(probe, timeout) {
        ProbeOutcome::Value(value) => value,
        ProbeOutcome::TimedOut => false,
        ProbeOutcome::Disconnected => {
            let _ = crate::services::logs::append_log("app.log", "device_detector_probe_panicked");
            false
        }
    }
}

/// Mount paths with a probe thread currently running. Keyed so that a mount whose
/// filesystem stat never returns (sleeping USB drive, dead SMB/NFS mount) can own at most
/// one probe thread for the lifetime of the process, instead of accumulating a new
/// permanently-blocked thread on every 2s `start_polling` tick.
static IN_FLIGHT_PROBES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Clears `key` from `IN_FLIGHT_PROBES` once the probe thread that registered it returns
/// (normally or via panic). Moved into the probe thread's closure so cleanup happens
/// whenever that thread actually finishes, however long that takes — not when the caller's
/// `recv_timeout` gives up on it.
struct InFlightProbeGuard {
    key: String,
}

impl Drop for InFlightProbeGuard {
    fn drop(&mut self) {
        IN_FLIGHT_PROBES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

/// Runs `probe` for `key` through `run_probe_with_timeout`, unless a probe for `key` is
/// already outstanding — in which case this returns `false` immediately without spawning a
/// thread. A mount that hasn't answered a filesystem stat within `timeout` isn't a usable
/// card, so "no DCIM" is the correct fail-safe answer here and it's exactly what a timed-out
/// probe already returns. A mount that keeps answering is probed again on every call once
/// its previous probe thread has cleared the in-flight entry.
fn probe_mount_if_not_in_flight(
    key: &str,
    probe: impl FnOnce() -> bool + Send + 'static,
    timeout: Duration,
) -> bool {
    let mut in_flight = IN_FLIGHT_PROBES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !in_flight.insert(key.to_string()) {
        return false;
    }
    drop(in_flight);

    let guard = InFlightProbeGuard {
        key: key.to_string(),
    };
    run_probe_with_timeout(
        move || {
            let _guard = guard;
            probe()
        },
        timeout,
    )
}

fn has_dcim(mount: &str) -> bool {
    let dcim_path = Path::new(mount).join("DCIM");

    // At most one probe thread is ever outstanding per mount: if a probe for this mount is
    // already in flight we report "no DCIM" immediately instead of stacking another thread,
    // so a permanently hung mount (sleeping USB drive, dead SMB/NFS mount) costs one thread
    // total rather than one per poll.
    probe_mount_if_not_in_flight(mount, move || dcim_path.is_dir(), DCIM_PROBE_TIMEOUT)
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
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

    #[test]
    fn panicking_probe_returns_false_and_is_distinguishable_from_timeout() {
        let panicked = run_probe_with_timeout(|| panic!("probe failed"), Duration::from_millis(10));
        assert!(!panicked);

        let disconnected =
            run_probe_with_timeout_outcome(|| panic!("probe failed"), Duration::from_millis(10));
        assert_eq!(disconnected, ProbeOutcome::Disconnected);

        let timed_out = run_probe_with_timeout_outcome(
            || {
                thread::sleep(Duration::from_millis(250));
                true
            },
            Duration::from_millis(10),
        );
        assert_eq!(timed_out, ProbeOutcome::TimedOut);
    }

    #[test]
    fn stalled_probe_suppresses_further_probes_of_the_same_mount() {
        // A probe that increments `spawn_count` before blocking on `release_rx` lets us
        // observe exactly how many probe threads were actually started, independent of how
        // many times `probe_mount_if_not_in_flight` is called.
        fn make_counting_probe(
            spawn_count: Arc<AtomicUsize>,
            release_rx: Arc<Mutex<mpsc::Receiver<()>>>,
        ) -> impl FnOnce() -> bool {
            move || {
                spawn_count.fetch_add(1, Ordering::SeqCst);
                let _ = release_rx
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv();
                true
            }
        }

        let spawn_count = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let key = "test-mount-stalled-probe";

        // First call spawns a probe thread that blocks on `release_rx`; the 10ms timeout
        // elapses long before it can answer, so the caller sees the fail-safe `false`.
        let first = probe_mount_if_not_in_flight(
            key,
            make_counting_probe(Arc::clone(&spawn_count), Arc::clone(&release_rx)),
            Duration::from_millis(10),
        );
        assert!(!first);

        // Give the spawned thread time to register itself as in-flight before probing again.
        thread::sleep(Duration::from_millis(50));

        // Second call while the first probe is still outstanding must return the same
        // fail-safe answer without spawning another worker thread.
        let second = probe_mount_if_not_in_flight(
            key,
            make_counting_probe(Arc::clone(&spawn_count), Arc::clone(&release_rx)),
            Duration::from_millis(10),
        );
        assert!(!second);
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1);

        // Release the stalled probe so its thread exits and clears the in-flight entry —
        // otherwise it leaks a permanently blocked thread into the rest of the suite.
        let _ = release_tx.send(());
        thread::sleep(Duration::from_millis(50));
    }
}
