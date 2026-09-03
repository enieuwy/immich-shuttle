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
/// Upper bound on one volume-identity lookup. Identity lookups read the filesystem or shell
/// out to platform tooling, either of which can hang on a wedged volume.
const VOLUME_ID_TIMEOUT: Duration = Duration::from_secs(3);

/// Runs `resolve` on a worker thread and abandons it after `timeout`. An abandoned or
/// panicking lookup reads as "identity unknown", so device enumeration does not stall.
fn resolve_with_timeout(
    resolve: impl FnOnce() -> Option<String> + Send + 'static,
    timeout: Duration,
) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(resolve());
    });
    rx.recv_timeout(timeout).ok().flatten()
}

/// Pulls `VolumeUUID` out of `diskutil info -plist`. Scanning for the key and the following
/// `<string>` keeps a whole plist parser out of the dependency list for one scalar, and the
/// surrounding document shape is fixed by `diskutil`.
#[cfg(target_os = "macos")]
fn parse_diskutil_volume_uuid(plist: &str) -> Option<String> {
    let after_key = plist.split("<key>VolumeUUID</key>").nth(1)?;
    let open = after_key.find("<string>")? + "<string>".len();
    let close = after_key[open..].find("</string>")? + open;
    let uuid = after_key[open..close].trim();
    (!uuid.is_empty()).then(|| uuid.to_string())
}

#[cfg(target_os = "macos")]
fn probe_volume_id(mount: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new("/usr/sbin/diskutil")
        .args(["info", "-plist", mount])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_diskutil_volume_uuid(&String::from_utf8_lossy(&output.stdout))
}

/// `/proc/self/mounts` escapes whitespace in its path fields as octal, so the raw field can
/// never be compared against a real path.
#[cfg(target_os = "linux")]
fn unescape_mount_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits: String = chars.clone().take(3).collect();
        if digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit() && b < b'8') {
            if let Ok(code) = u8::from_str_radix(&digits, 8) {
                out.push(code as char);
                chars.nth(2);
                continue;
            }
        }
        out.push('\\');
    }
    out
}

/// Field 1 of `/proc/self/mounts` is the backing device node, field 2 the mount point. The
/// last matching line wins: a later mount over the same directory is the one in effect.
#[cfg(target_os = "linux")]
fn parse_mount_source(mounts: &str, mount: &str) -> Option<String> {
    mounts.lines().rev().find_map(|line| {
        let mut fields = line.split(' ');
        let source = fields.next()?;
        let target = fields.next()?;
        (unescape_mount_field(target) == mount).then(|| unescape_mount_field(source))
    })
}

#[cfg(target_os = "linux")]
fn probe_volume_id(mount: &str) -> Option<String> {
    use std::fs;

    let mounts = fs::read_to_string("/proc/self/mounts").ok()?;
    let source = parse_mount_source(&mounts, mount)?;
    let device = fs::canonicalize(source).ok()?;
    for entry in fs::read_dir("/dev/disk/by-uuid").ok()?.flatten() {
        if fs::canonicalize(entry.path()).ok().as_deref() == Some(device.as_path()) {
            return Some(entry.file_name().to_string_lossy().into_owned());
        }
    }
    None
}

/// `mountvol <path> /L` prints the volume GUID path (`\\?\Volume{…}\`), the Windows-stable
/// per-volume identity. `mountvol` ships with the OS, so this needs no crate and no FFI.
#[cfg(target_os = "windows")]
fn parse_mountvol_guid(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|token| token.starts_with(r"\\?\Volume{"))
        .map(|token| token.trim_end_matches('\\').to_string())
}

#[cfg(target_os = "windows")]
fn probe_volume_id(mount: &str) -> Option<String> {
    use std::process::Command;

    // `mountvol` only accepts a directory path with a trailing separator.
    let mut path = mount.to_string();
    if !path.ends_with('\\') {
        path.push('\\');
    }
    let output = Command::new("cmd")
        .args(["/C", "mountvol", &path, "/L"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_mountvol_guid(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn probe_volume_id(_mount: &str) -> Option<String> {
    None
}

/// Resolves a volume identity through one fresh, bounded platform probe.
///
/// Callers must treat `None` as untrusted. This function deliberately keeps no result cache:
/// a mount path can be reused by a different card before a device refresh observes an eject.
fn resolve_volume_identity(
    path: &Path,
    resolve: impl FnOnce(String) -> Option<String> + Send + 'static,
) -> Option<String> {
    let mount = path.to_str()?.to_owned();
    resolve_with_timeout(move || resolve(mount), VOLUME_ID_TIMEOUT)
}

/// Returns the stable identity for the volume mounted at `path`.
///
/// This always performs a fresh, bounded OS probe. A replacement at the same mount path must
/// never inherit the former volume's identity. It returns `None` when the OS cannot prove an
/// identity or the probe fails or times out.
pub fn volume_identity_for_path(path: &Path) -> Option<String> {
    resolve_volume_identity(path, |mount| probe_volume_id(&mount))
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
                volume_id: None,
            })
        })
        .collect::<Vec<_>>();

    // Do not cache identities across refreshes. A new card can replace an old card at the
    // same mount path and report the same capacity before the detector sees an eject.
    candidates
        .into_iter()
        .map(|mut device| {
            device.has_dcim = has_dcim(&device.mount_path);
            device.volume_id = volume_identity_for_path(Path::new(&device.mount_path));
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

    /// `removable` comes from the OS, so a removable disk is a card wherever it
    /// is mounted; the path rules exist only for disks the OS does not flag.
    #[test]
    fn removable_mounts_are_always_included() {
        assert!(should_include_mount("/any/path", true));
    }

    /// The filter that keeps the boot volume out of the card list. A regression
    /// here offers the post-import delete prompt for the system disk.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_mount_filter_excludes_system_and_home_volumes() {
        assert!(should_include_mount("/Volumes/SD_CARD", false));
        assert!(!should_include_mount("/Volumes/Macintosh HD", false));
        assert!(!should_include_mount("/Volumes/Macintosh HD - Data", false));
        assert!(!should_include_mount("/Users/ellis/Pictures", false));
        assert!(!should_include_mount("/", false));
    }

    /// Same rule on Linux, where only the removable-media mount points count.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mount_filter_excludes_system_and_home_volumes() {
        assert!(should_include_mount("/media/ellis/SD_CARD", false));
        assert!(should_include_mount("/mnt/card", false));
        assert!(!should_include_mount("/", false));
        assert!(!should_include_mount("/home/ellis", false));
    }

    /// A path that is not a mount point anywhere. An identity the platform cannot prove
    /// must read as unknown; an error or a panic here would take down device enumeration.
    #[test]
    fn unresolvable_mount_yields_no_identity() {
        assert_eq!(
            volume_identity_for_path(Path::new("/nonexistent-immich-shuttle-mount")),
            None
        );
    }

    /// A recycled mount path and capacity must still get a new identity probe. Otherwise a
    /// rule for the ejected card can select post-import deletion for the replacement card.
    #[test]
    fn volume_identity_is_fresh_for_every_detector_refresh() {
        fn counting_probe(calls: Arc<AtomicUsize>) -> impl FnOnce(String) -> Option<String> + Send {
            move |_| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                Some(format!("VOL-UUID-{call}"))
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let mount = Path::new("/Volumes/test-identity-refresh");

        let first = resolve_volume_identity(mount, counting_probe(Arc::clone(&calls)));
        let second = resolve_volume_identity(mount, counting_probe(Arc::clone(&calls)));

        assert_eq!(first.as_deref(), Some("VOL-UUID-0"));
        assert_eq!(second.as_deref(), Some("VOL-UUID-1"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// A wedged volume must not stall enumeration: an abandoned lookup reads as unknown.
    #[test]
    fn hung_identity_lookup_times_out_to_unknown() {
        let started = Instant::now();
        let resolved = resolve_with_timeout(
            || {
                thread::sleep(Duration::from_millis(250));
                Some("late".to_string())
            },
            Duration::from_millis(10),
        );

        assert_eq!(resolved, None);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    /// The one scalar we read out of `diskutil info -plist`. A volume with no UUID (many
    /// FAT-formatted cards) must read as unknown rather than as an empty identity that
    /// every such card would share.
    #[cfg(target_os = "macos")]
    #[test]
    fn diskutil_plist_volume_uuid_is_extracted() {
        let plist = concat!(
            "<plist><dict>\n",
            "\t<key>VolumeName</key>\n\t<string>Untitled</string>\n",
            "\t<key>VolumeUUID</key>\n\t<string>0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9</string>\n",
            "</dict></plist>\n"
        );

        assert_eq!(
            parse_diskutil_volume_uuid(plist).as_deref(),
            Some("0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9")
        );
        assert_eq!(parse_diskutil_volume_uuid("<plist><dict/></plist>"), None);
        assert_eq!(
            parse_diskutil_volume_uuid("<key>VolumeUUID</key><string>  </string>"),
            None
        );
    }

    /// `/proc/self/mounts` octal-escapes whitespace, so the raw field never matches a real
    /// path and the device node would be looked up against the wrong line.
    #[cfg(target_os = "linux")]
    #[test]
    fn mount_source_is_read_from_the_last_matching_proc_line() {
        let mounts = concat!(
            "/dev/sda1 / ext4 rw 0 0\n",
            "/dev/sdb1 /media/ellis/SD\\040CARD vfat rw 0 0\n",
            "/dev/sdc1 /media/ellis/SD\\040CARD vfat rw 0 0\n"
        );

        assert_eq!(
            parse_mount_source(mounts, "/media/ellis/SD CARD").as_deref(),
            Some("/dev/sdc1")
        );
        assert_eq!(parse_mount_source(mounts, "/media/ellis/SD"), None);
    }

    /// `mountvol /L` pads the GUID path with whitespace and a trailing separator.
    #[cfg(target_os = "windows")]
    #[test]
    fn mountvol_guid_path_is_extracted() {
        let output = "\r\n    \\\\?\\Volume{9a1b2c3d-0000-0000-0000-100000000000}\\\r\n";
        assert_eq!(
            parse_mountvol_guid(output).as_deref(),
            Some("\\\\?\\Volume{9a1b2c3d-0000-0000-0000-100000000000}")
        );
        assert_eq!(parse_mountvol_guid("There is no volume mounted"), None);
    }
}
