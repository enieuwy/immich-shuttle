use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
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

/// `IN_FLIGHT_PROBES` keys are namespaced by probe kind. A DCIM probe and an identity probe
/// of the same mount are independent operations, so neither may suppress the other; the only
/// redundant work worth dropping is a second probe of the same kind for the same mount.
fn dcim_probe_key(mount: &str) -> String {
    format!("dcim:{mount}")
}

fn identity_probe_key(mount: &str) -> String {
    format!("identity:{mount}")
}

/// Registers `key` as outstanding and hands back the guard that releases it, or `None` when
/// a probe for `key` is already running. The guard has to be moved into the probe closure so
/// the entry clears when the probe thread actually finishes, not when the caller stops
/// waiting for it.
fn reserve_in_flight_probe(key: &str) -> Option<InFlightProbeGuard> {
    let mut in_flight = IN_FLIGHT_PROBES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    in_flight
        .insert(key.to_string())
        .then(|| InFlightProbeGuard {
            key: key.to_string(),
        })
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
    let Some(guard) = reserve_in_flight_probe(key) else {
        return false;
    };
    run_probe_with_timeout(
        move || {
            let _guard = guard;
            probe()
        },
        timeout,
    )
}

/// The identity counterpart of `probe_mount_if_not_in_flight`: runs `resolve` for `key`
/// through `resolve_with_timeout` unless an identity probe for `key` is already outstanding.
///
/// `list_removable_devices` asks for an identity for every candidate on every 2s poll, and a
/// timed-out lookup abandons its worker thread. Without this guard a wedged volume therefore
/// leaked one permanently blocked thread plus one abandoned `diskutil`/`mountvol` child per
/// poll, forever. `None` is the same fail-safe answer a timed-out probe gives, and callers
/// must already treat an unknown identity as untrusted.
fn resolve_identity_if_not_in_flight(
    key: &str,
    resolve: impl FnOnce() -> Option<String> + Send + 'static,
    timeout: Duration,
) -> Option<String> {
    let guard = reserve_in_flight_probe(key)?;
    resolve_with_timeout(
        move || {
            let _guard = guard;
            resolve()
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
    probe_mount_if_not_in_flight(
        &dcim_probe_key(mount),
        move || dcim_path.is_dir(),
        DCIM_PROBE_TIMEOUT,
    )
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

/// Builds the argv for one `mountvol` identity probe: the absolute path of the System32
/// executable, then exactly two arguments.
///
/// `mountvol` must never be invoked through `cmd /C`. The mount path comes from the OS, and a
/// volume mounted at a directory whose name contains `&`, `|` or `^` would make `cmd` run the
/// suffix as a command under the app account. Passing the path as a single argument to the
/// executable itself removes the shell from the picture entirely. The absolute path also
/// stops a `mountvol.exe` planted earlier on `PATH` from being picked up.
#[cfg(any(target_os = "windows", test))]
fn mountvol_command(mount: &str, system_root: Option<&str>) -> (PathBuf, Vec<String>) {
    // `mountvol` only accepts a directory path with a trailing separator.
    let mut path = mount.to_string();
    if !path.ends_with('\\') {
        path.push('\\');
    }
    let root = system_root.unwrap_or(r"C:\Windows");
    (
        PathBuf::from(format!(r"{root}\System32\mountvol.exe")),
        vec![path, "/L".to_string()],
    )
}

#[cfg(target_os = "windows")]
fn probe_volume_id(mount: &str) -> Option<String> {
    use std::process::Command;

    let system_root = std::env::var("SystemRoot").ok();
    let (program, args) = mountvol_command(mount, system_root.as_deref());
    let output = Command::new(program).args(args).output().ok()?;
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
/// The in-flight guard is not a cache: it drops a probe only while an earlier probe of the
/// same mount is still running, and that probe's caller has already been told `None`.
fn resolve_volume_identity(
    path: &Path,
    resolve: impl FnOnce(String) -> Option<String> + Send + 'static,
) -> Option<String> {
    let mount = path.to_str()?.to_owned();
    let key = identity_probe_key(&mount);
    resolve_identity_if_not_in_flight(&key, move || resolve(mount), VOLUME_ID_TIMEOUT)
}

/// Returns the stable identity for the volume mounted at `path`.
///
/// `path` must be a mount point: every platform probe requires one. `diskutil` exits non-zero
/// for a file, `mountvol` rejects it, and `/proc/self/mounts` has no line for it. Use
/// `file_volume_identity_resolver` for asset paths.
///
/// This always performs a fresh, bounded OS probe. A replacement at the same mount path must
/// never inherit the former volume's identity. It returns `None` when the OS cannot prove an
/// identity or the probe fails or times out.
pub fn volume_identity_for_path(path: &Path) -> Option<String> {
    resolve_volume_identity(path, |mount| probe_volume_id(&mount))
}

/// Returns the mount point that contains `path`, which may be a regular file.
///
/// `None` means the mount point could not be proven, and callers must then treat the volume
/// identity as unknown rather than guess a root.
#[cfg(unix)]
pub fn mount_root_for_path(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    // Canonicalize first. `ancestors()` walks the path text, so a relative path or a symlinked
    // prefix (`/tmp` -> `/private/tmp` on macOS) would otherwise be walked as written and end
    // somewhere that is not a mount point at all.
    let resolved = std::fs::canonicalize(path).ok()?;
    let device = std::fs::metadata(&resolved).ok()?.dev();

    let mut root = resolved.as_path();
    for ancestor in resolved.ancestors().skip(1) {
        match std::fs::metadata(ancestor) {
            Ok(metadata) if metadata.dev() == device => root = ancestor,
            // The first ancestor on a different device is the far side of a mount boundary, so
            // the previous one is the mount point. We stop there instead of taking the highest
            // matching ancestor overall, because a bind mount can put the same device back
            // above a boundary and that higher directory is not this file's mount point.
            _ => break,
        }
    }
    Some(root.to_path_buf())
}

/// Windows has no `st_dev`, so "is this directory a mount point" has to be asked of the OS:
/// walk upwards and take the first ancestor whose volume probe succeeds. That accepts a
/// drive-letter root and a directory mount point alike.
#[cfg(target_os = "windows")]
pub fn mount_root_for_path(path: &Path) -> Option<PathBuf> {
    mount_root_for_path_with(path, |candidate| {
        probe_volume_id(&candidate.to_string_lossy()).is_some()
    })
}

/// Split out from `mount_root_for_path` so the walk itself is testable on any host: the
/// injected `is_mount_point` stands in for the OS acceptance test.
#[cfg(any(target_os = "windows", test))]
fn mount_root_for_path_with(
    path: &Path,
    mut is_mount_point: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let resolved = std::path::absolute(path).ok()?;
    // A file path is never a mount point, and probing it would only waste a subprocess, so
    // start at the parent unless the path itself is a directory.
    let start = if resolved.is_dir() {
        resolved.as_path()
    } else {
        resolved.parent()?
    };
    start
        .ancestors()
        .find(|candidate| is_mount_point(candidate))
        .map(Path::to_path_buf)
}

#[cfg(not(any(unix, target_os = "windows")))]
pub fn mount_root_for_path(_path: &Path) -> Option<PathBuf> {
    None
}

/// Returns a resolver that maps any path — including a regular file — to the identity of the
/// volume that holds it.
///
/// Each returned closure performs at most one fresh probe per distinct mount root and reuses
/// that answer for its own lifetime only, so a 2,000-file import costs one probe per card
/// instead of one blocking probe and one subprocess per file.
///
/// The memo is deliberately closure-local, and there is deliberately no static cache: a
/// replacement card mounted at the same path must never inherit the ejected card's identity.
/// Every new resolver therefore starts empty and re-probes.
pub fn file_volume_identity_resolver() -> impl FnMut(&Path) -> Option<String> + Send {
    file_volume_identity_resolver_with(mount_root_for_path, volume_identity_for_path)
}

fn file_volume_identity_resolver_with(
    mount_root: impl Fn(&Path) -> Option<PathBuf> + Send,
    probe: impl Fn(&Path) -> Option<String> + Send,
) -> impl FnMut(&Path) -> Option<String> + Send {
    let mut resolved: HashMap<PathBuf, Option<String>> = HashMap::new();
    move |path| {
        let root = mount_root(path)?;
        // An unprovable identity is memoized too. Re-probing it per file would reintroduce
        // exactly the per-file subprocess storm this resolver exists to remove, and `None`
        // already means "refuse to act on this volume".
        if let Some(identity) = resolved.get(&root) {
            return identity.clone();
        }
        let identity = probe(&root);
        resolved.insert(root, identity.clone());
        identity
    }
}

/// The production resolver with only its identity probe replaced: the mount-root
/// step, which is the part a wipe candidate depends on, stays real. A test can
/// therefore prove WHICH path the probe is asked about without depending on the
/// host having a provable volume id for its temp directory — a CI container's
/// root filesystem usually has none.
#[cfg(test)]
pub(crate) fn file_volume_identity_resolver_probing(
    probe: impl Fn(&Path) -> Option<String> + Send,
) -> impl FnMut(&Path) -> Option<String> + Send {
    file_volume_identity_resolver_with(mount_root_for_path, probe)
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
    use uuid::Uuid;

    /// Test seam for a probe that never answers on its own.
    ///
    /// The probe counts itself in `starts`, signals the returned "started" receiver, then parks
    /// in `recv()` until the test drops the returned sender. Blocking on channels the test owns
    /// makes the timeout path the only outcome the caller can reach and lets the test wait for
    /// facts instead of sleeping, so no assertion depends on how fast the machine is.
    fn parked_probe<T: Send + 'static>(
        answer: T,
        starts: Arc<AtomicUsize>,
    ) -> (
        impl FnOnce() -> T + Send + 'static,
        mpsc::Sender<()>,
        mpsc::Receiver<()>,
    ) {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let probe = move || {
            starts.fetch_add(1, Ordering::SeqCst);
            let _ = started_tx.send(());
            let _ = release_rx.recv();
            answer
        };
        (probe, release_tx, started_rx)
    }

    fn probe_starts() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
    }

    #[test]
    fn timed_out_probe_does_not_wait_for_the_filesystem_operation() {
        let (probe, release, started) = parked_probe(true, probe_starts());

        // The probe cannot answer while the test holds `release`, so returning at all proves
        // the caller did not wait for the filesystem operation, and `false` is the fail-safe.
        let result = run_probe_with_timeout(probe, Duration::from_millis(10));
        assert!(!result);

        started.recv().expect("the probe thread ran");
        drop(release);
    }

    #[test]
    fn panicking_probe_returns_false_and_is_distinguishable_from_timeout() {
        let panicked = run_probe_with_timeout(|| panic!("probe failed"), Duration::from_millis(10));
        assert!(!panicked);

        let disconnected =
            run_probe_with_timeout_outcome(|| panic!("probe failed"), Duration::from_millis(10));
        assert_eq!(disconnected, ProbeOutcome::Disconnected);

        let (probe, release, started) = parked_probe(true, probe_starts());
        let timed_out = run_probe_with_timeout_outcome(probe, Duration::from_millis(10));
        assert_eq!(timed_out, ProbeOutcome::TimedOut);
        started.recv().expect("the probe thread ran");
        drop(release);
    }

    #[test]
    fn stalled_probe_suppresses_further_probes_of_the_same_mount() {
        let starts = probe_starts();
        let key = "test-mount-stalled-probe";

        // First call parks a probe thread on a channel only this test can release, so the 10ms
        // timeout is the only way out and the caller sees the fail-safe `false`.
        let (first_probe, first_release, first_started) = parked_probe(true, Arc::clone(&starts));
        let first = probe_mount_if_not_in_flight(key, first_probe, Duration::from_millis(10));
        assert!(!first);

        // Waiting for the probe's own signal replaces a sleep: from here the first probe is
        // provably running, so the in-flight entry it registered is provably present.
        first_started.recv().expect("the first probe thread ran");
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        // Second call while the first probe is still outstanding must return the same
        // fail-safe answer without spawning another worker thread.
        let (second_probe, _second_release, second_started) =
            parked_probe(true, Arc::clone(&starts));
        let second = probe_mount_if_not_in_flight(key, second_probe, Duration::from_millis(10));
        assert!(!second);
        // The second probe closure was dropped without ever running, so its start signal can
        // never arrive — a stronger statement than a count that might still be catching up.
        assert!(second_started.try_recv().is_err());
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        // Release the parked probe so its thread exits and clears the in-flight entry —
        // otherwise it leaks a permanently blocked thread into the rest of the suite.
        drop(first_release);
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
        let (probe, release, started) = parked_probe(Some("late".to_string()), probe_starts());

        // The lookup cannot answer while this test holds `release`, so an answer of `None` can
        // only mean the caller stopped waiting for it.
        let resolved = resolve_with_timeout(probe, Duration::from_millis(10));
        assert_eq!(resolved, None);

        started.recv().expect("the lookup thread ran");
        drop(release);
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

    /// The defect this guards: every asset path went straight into a platform probe that only
    /// accepts a mount point, so the identity was always unknown and the wipe prompt could
    /// never confirm a volume on any platform. An asset path has to be resolved through the
    /// mount root that contains it.
    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn file_identity_is_resolved_through_the_files_mount_root() {
        let dir = std::env::temp_dir().join(format!("device-mount-root-{}", Uuid::new_v4()));
        let nested = dir.join("DCIM").join("100CANON");
        std::fs::create_dir_all(&nested).expect("temp tree");
        let file = nested.join("IMG_0001.JPG");
        std::fs::write(&file, b"asset").expect("temp asset");

        let root = mount_root_for_path(&file).expect("a file on a mounted filesystem has a root");
        assert!(
            root.is_dir(),
            "the mount root must be a directory: {root:?}"
        );
        assert!(
            std::fs::canonicalize(&file)
                .expect("canonical asset path")
                .starts_with(&root),
            "{root:?} must be an ancestor of the asset"
        );
        assert_ne!(root, file);

        // Still true of the mount-point-only probe, on every platform: no OS identifies a
        // volume from a file path. This is exactly what the import path used to ask for.
        assert_eq!(volume_identity_for_path(&file), None);
        // Going through the mount root gives the file the identity of the volume it lives on,
        // which is whatever the platform proves for that mount point.
        assert_eq!(
            file_volume_identity_resolver()(&file),
            volume_identity_for_path(&root)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path the filesystem cannot even stat has no provable mount root. Guessing one would
    /// hand the caller another volume's identity.
    #[cfg(unix)]
    #[test]
    fn missing_path_has_no_provable_mount_root() {
        assert_eq!(
            mount_root_for_path(Path::new("/nonexistent-immich-shuttle-asset.jpg")),
            None
        );
    }

    /// One card must cost one probe. The import path resolves an identity per selected asset,
    /// so the old per-path probe meant one 3s-bounded blocking lookup and one `diskutil`/
    /// `mountvol` subprocess for every file in the import.
    #[test]
    fn resolver_probes_each_mount_root_once() {
        fn counting_probe(probes: Arc<AtomicUsize>) -> impl Fn(&Path) -> Option<String> + Send {
            move |root| {
                probes.fetch_add(1, Ordering::SeqCst);
                Some(format!("VOL-{}", root.display()))
            }
        }

        let probes = probe_starts();
        let roots = |path: &Path| path.parent().map(Path::to_path_buf);
        let mut resolve =
            file_volume_identity_resolver_with(roots, counting_probe(Arc::clone(&probes)));

        assert_eq!(
            resolve(Path::new("/card/IMG_0001.JPG")).as_deref(),
            Some("VOL-/card")
        );
        assert_eq!(
            resolve(Path::new("/card/IMG_0002.JPG")).as_deref(),
            Some("VOL-/card")
        );
        assert_eq!(probes.load(Ordering::SeqCst), 1);

        // A second mount root is a second probe: the memo is per root, never one answer for
        // every path the resolver sees.
        assert_eq!(
            resolve(Path::new("/other/IMG_0003.JPG")).as_deref(),
            Some("VOL-/other")
        );
        assert_eq!(probes.load(Ordering::SeqCst), 2);

        // No provable mount root means unknown identity and no probe at all.
        assert_eq!(resolve(Path::new("/")), None);
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    /// The memo must not outlive its resolver. A replacement card can be mounted at the path an
    /// ejected card used, and inheriting the old identity would let a wipe rule approved for
    /// the ejected card select the new card's files for deletion.
    #[test]
    fn a_new_resolver_reprobes_a_mount_root_it_has_seen_before() {
        fn numbering_probe(probes: Arc<AtomicUsize>) -> impl Fn(&Path) -> Option<String> + Send {
            move |_| {
                let call = probes.fetch_add(1, Ordering::SeqCst);
                Some(format!("VOL-UUID-{call}"))
            }
        }

        let probes = probe_starts();
        let asset = Path::new("/card/IMG_0001.JPG");
        let roots = |path: &Path| path.parent().map(Path::to_path_buf);

        let mut first =
            file_volume_identity_resolver_with(roots, numbering_probe(Arc::clone(&probes)));
        assert_eq!(first(asset).as_deref(), Some("VOL-UUID-0"));
        assert_eq!(first(asset).as_deref(), Some("VOL-UUID-0"));

        let mut second =
            file_volume_identity_resolver_with(roots, numbering_probe(Arc::clone(&probes)));
        assert_eq!(second(asset).as_deref(), Some("VOL-UUID-1"));
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    /// A wedged volume must stop accumulating work. `list_removable_devices` asks for an
    /// identity for every candidate on every 2s poll, and a timed-out lookup keeps its blocked
    /// thread and its abandoned `diskutil`/`mountvol` child, so without this guard one dead
    /// mount leaked both on every poll, forever.
    #[test]
    fn wedged_identity_probe_admits_one_lookup_per_mount() {
        let starts = probe_starts();
        let key = identity_probe_key("/Volumes/test-wedged-identity");

        let (first_probe, first_release, first_started) =
            parked_probe(Some("late".to_string()), Arc::clone(&starts));
        assert_eq!(
            resolve_identity_if_not_in_flight(&key, first_probe, Duration::from_millis(10)),
            None
        );
        first_started.recv().expect("the first lookup ran");
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        let (second_probe, _second_release, second_started) =
            parked_probe(Some("later".to_string()), Arc::clone(&starts));
        assert_eq!(
            resolve_identity_if_not_in_flight(&key, second_probe, Duration::from_millis(10)),
            None
        );
        // The second lookup closure was dropped without running: no second thread, no second
        // subprocess, and the answer is the same fail-safe unknown.
        assert!(second_started.try_recv().is_err());
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        drop(first_release);
    }

    /// A DCIM probe and an identity probe of the same mount answer different questions, and
    /// `list_removable_devices` runs both on the same tick. Sharing one in-flight key would
    /// make a slow DCIM probe report the card's identity as unknown.
    #[test]
    fn a_stalled_dcim_probe_does_not_suppress_the_identity_probe() {
        let mount = "/Volumes/test-probe-key-namespace";
        let starts = probe_starts();

        let (dcim_probe, dcim_release, dcim_started) = parked_probe(true, Arc::clone(&starts));
        assert!(!probe_mount_if_not_in_flight(
            &dcim_probe_key(mount),
            dcim_probe,
            Duration::from_millis(10)
        ));
        dcim_started.recv().expect("the dcim probe ran");

        let identity = resolve_identity_if_not_in_flight(
            &identity_probe_key(mount),
            || Some("VOL-UUID-1".to_string()),
            VOLUME_ID_TIMEOUT,
        );
        assert_eq!(identity.as_deref(), Some("VOL-UUID-1"));

        drop(dcim_release);
    }

    /// The mount path comes from the OS. Running it through `cmd /C` let a volume mounted at a
    /// directory named `SD & calc` execute the suffix as a command under the app account.
    #[test]
    fn mountvol_probe_argv_has_no_shell_and_exactly_one_path() {
        let (program, args) = mountvol_command(r"C:\mnt\SD & CARD", Some(r"D:\WINNT"));

        assert_eq!(program, PathBuf::from(r"D:\WINNT\System32\mountvol.exe"));
        assert_eq!(
            args,
            vec![r"C:\mnt\SD & CARD\".to_string(), "/L".to_string()]
        );
        assert!(!args.iter().any(|arg| arg.eq_ignore_ascii_case("/c")));
        assert!(!program.to_string_lossy().contains("cmd"));

        // An absent `SystemRoot` falls back to the default install path instead of resolving
        // `mountvol.exe` through `PATH`, where anything earlier on the path would win.
        let (fallback, args) = mountvol_command(r"C:\mnt\SD & CARD\", None);
        assert_eq!(fallback, PathBuf::from(r"C:\Windows\System32\mountvol.exe"));
        // The trailing separator `mountvol` requires is added once, never doubled.
        assert_eq!(args[0], r"C:\mnt\SD & CARD\");
    }

    /// The Windows mount-root walk has no `st_dev` to compare, so it asks the OS which ancestor
    /// is a mount point. Driving that walk through an injected acceptance test keeps it covered
    /// on every platform instead of only on Windows hardware.
    #[test]
    fn mount_root_walk_takes_the_first_ancestor_the_os_accepts() {
        let dir = std::env::temp_dir().join(format!("device-mount-walk-{}", Uuid::new_v4()));
        let nested = dir.join("DCIM");
        std::fs::create_dir_all(&nested).expect("temp tree");
        let file = nested.join("IMG_0001.JPG");
        std::fs::write(&file, b"asset").expect("temp asset");

        let mut asked: Vec<PathBuf> = Vec::new();
        let root = mount_root_for_path_with(&file, |candidate| {
            asked.push(candidate.to_path_buf());
            candidate == dir.as_path()
        });
        assert_eq!(root.as_deref(), Some(dir.as_path()));
        // The file itself is never offered as a mount point: no OS accepts a file, so probing
        // it would only spend a subprocess to be told no.
        assert_eq!(asked.first().map(PathBuf::as_path), Some(nested.as_path()));

        // A directory that is itself the mount point resolves to itself.
        assert_eq!(
            mount_root_for_path_with(&dir, |candidate| candidate == dir.as_path()).as_deref(),
            Some(dir.as_path())
        );
        // No ancestor accepted means nothing proven, so the identity stays unknown.
        assert_eq!(mount_root_for_path_with(&file, |_| false), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
