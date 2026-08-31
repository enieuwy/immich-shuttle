//! Atomic replacement for the app's persisted JSON stores, hardened against
//! other local users where the platform supports it.
//!
//! `config.json` holds every profile's LAN/WAN endpoints and `store.json` holds
//! the full import history including local source paths -- the same class of
//! data the logs directory is already kept owner-only for on Unix (see
//! `logs::logs_dir`). Both used to go out through a plain `fs::write`, i.e.
//! group/world-readable on a shared Unix machine, and each carried its own copy
//! of the temp-file dance. The 0600/0700 permission hardening added here is
//! `#[cfg(unix)]` only; on Windows the only access control is the per-user
//! app-data ACL Windows already applies, unchanged from before.

use std::{
    fs,
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

/// Disambiguates temp files between concurrent saves in this process. The pid is
/// belt-and-braces: `tauri_plugin_single_instance` already keeps a second
/// process off these files.
static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// Write `contents` to `path` atomically.
///
/// The temp file is created in the destination's own directory so the rename
/// stays on one filesystem, under a name unique to this process and call.
/// `fs::rename` replaces an existing destination on both Unix (`rename`) and
/// Windows (`MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`), so there is no
/// direct-write fallback: when the rename fails the previous contents survive
/// intact, which is strictly better than a half-written store.
///
/// Owner-only file and directory modes (0600 / 0700) are applied `#[cfg(unix)]`
/// only -- Windows has no POSIX mode bits, so this relies on the per-user ACL
/// Windows already applies to the app's data directory instead. Note also that
/// the 0700 chmod on the parent directory runs unconditionally on every save,
/// so a deliberate group-access or setgid bit a user set on that directory is
/// silently cleared on the next call.
///
/// After the rename, the parent directory is fsynced (`#[cfg(unix)]`) so the
/// new directory entry itself survives an unclean shutdown. A sync failure
/// cannot undo the visible rename, so it is logged as a durability warning and
/// the committed write still returns `Ok`.
pub fn write_atomic_private(path: &Path, contents: &str) -> Result<(), String> {
    write_atomic_private_with_parent_sync(
        path,
        contents,
        sync_parent_directory,
        report_parent_sync_failure,
    )
}

#[cfg(unix)]
fn sync_parent_directory(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn report_parent_sync_failure(path: &Path, err: &io::Error) {
    let warning = format!(
        "atomic_write_committed_durability_unconfirmed path={} error={err}",
        path.display()
    );
    if let Err(log_err) = crate::services::logs::append_log("app.log", &warning) {
        eprintln!("{warning}; additionally failed to append app log: {log_err}");
    }
}

fn write_atomic_private_with_parent_sync(
    path: &Path,
    contents: &str,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
    report_sync_failure: impl FnOnce(&Path, &io::Error),
) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("Could not resolve parent directory of {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }

    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
    let tmp = dir.join(format!(
        ".{stem}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed)
    ));

    let write = || -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        // The rename below is only atomic with respect to the directory entry;
        // fsync first so a crash can't publish a name pointing at empty bytes.
        file.sync_all()
    };

    if let Err(err) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Could not write {}: {err}", tmp.display()));
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Could not persist {}: {err}", path.display()));
    }
    // Fsync the parent directory so the rename's directory-entry update is
    // itself durable; the file's own fsync above only protects its bytes.
    // No Windows equivalent: NTFS has no directory-fsync primitive, and
    // `fs::rename` on Windows already goes through `MoveFileEx`, which
    // journals the metadata update itself.
    if let Err(err) = sync_parent(dir) {
        // The rename already committed. Returning the same error as a
        // pre-commit failure would make cross-store callers roll back related
        // state while the new file remains visible.
        report_sync_failure(path, &err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{write_atomic_private, write_atomic_private_with_parent_sync};
    use uuid::Uuid;

    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("immich-shuttle-private-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replaces_an_existing_file_and_leaves_no_temp_behind() {
        let dir = scratch_dir();
        let path = dir.join("store.json");

        write_atomic_private(&path, "first").unwrap();
        write_atomic_private(&path, "second").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parent_sync_failure_keeps_the_committed_write_successful() {
        let dir = scratch_dir();
        let path = dir.join("store.json");
        std::fs::write(&path, "previous").unwrap();
        let warning_reported = std::cell::Cell::new(false);

        write_atomic_private_with_parent_sync(
            &path,
            "current",
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected parent sync failure",
                ))
            },
            |warning_path, err| {
                warning_reported.set(true);
                assert_eq!(warning_path, path);
                assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            },
        )
        .expect("a parent sync failure occurs after the write commits");

        assert!(warning_reported.get());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "current");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writes_the_store_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir();
        let path = dir.join("config.json");
        write_atomic_private(&path, "{}").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "config is group/world accessible: {mode:o}"
        );
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o077, 0, "dir is group/world accessible");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tightens_permissions_on_a_previously_world_readable_store() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir();
        let path = dir.join("store.json");
        std::fs::write(&path, "legacy").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_atomic_private(&path, "current").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "legacy mode survived: {mode:o}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
