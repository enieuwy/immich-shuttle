//! Stages a user-selected subset of files for upload.
//!
//! immich-go's `from-folder` has no per-file selection (only extension/type/date
//! filters), so to import exactly the files a user picked in the preview grid we
//! build a temporary directory that links to just those files, preserving each
//! file's name and relative structure (so original filenames reach the server and
//! same-named files in different folders don't collide), then point the uploader
//! at that directory. Links are symlinks where possible, falling back to hard
//! links then a copy. Cleanup removes only the links, never the originals.

use fs4::fs_std::FileExt;
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use uuid::Uuid;

/// The paths immich-go saw for successfully linked files and their user-selected
/// originals. The run log is parsed after the staging directory is removed, so
/// callers must take this map before moving the guard into cleanup.
#[derive(Debug, Clone, Default)]
pub struct StagingPathMap {
    entries: Vec<(PathBuf, PathBuf)>,
}

impl StagingPathMap {
    /// Resolve a path from the run log back to the selected original.
    pub fn original_for(&self, staged: &Path) -> Option<&Path> {
        self.entries.iter().find_map(|(destination, original)| {
            (destination == staged).then_some(original.as_path())
        })
    }

    /// Every `(staged destination, original selection)` pair. Production reads
    /// the map only through `original_for`; the pairs themselves are needed to
    /// build run-log fixtures in tests.
    #[cfg(test)]
    pub fn entries(&self) -> &[(PathBuf, PathBuf)] {
        &self.entries
    }

    fn push(&mut self, staged: PathBuf, original: PathBuf) {
        self.entries.push((staged, original));
    }
}

/// Owns a temporary staging directory and removes it when dropped.
///
/// Normal callers should move this into [`cleanup_staging_dir`] on a blocking
/// worker. Drop remains the backstop for cancellation, early returns, and
/// panics, so staged links cannot outlive their import.
pub struct StagingDir {
    path: Option<PathBuf>,
    lock: Option<fs::File>,
    links: StagingPathMap,
}

impl StagingDir {
    pub fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("staging directory path is available until cleanup")
    }

    /// Borrow the link map while the guard still owns it. Production always
    /// takes the map instead, because cleanup consumes the guard before the run
    /// log is parsed.
    #[cfg(test)]
    pub fn links(&self) -> &StagingPathMap {
        &self.links
    }

    /// Take the link map before cleanup consumes this guard. The run log is only
    /// parsed after cleanup has removed the directory, so the map must leave the
    /// guard first; nothing may borrow it from a guard that is about to move.
    pub fn take_links(&mut self) -> StagingPathMap {
        std::mem::take(&mut self.links)
    }

    fn cleanup(&mut self) {
        let path = self.path.take();
        drop(self.lock.take());
        if let Some(path) = path {
            let _ = fs::remove_dir_all(path);
        }
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        // Cancellation and panic paths may drop this guard on an async worker.
        // Detached cleanup keeps that runtime worker from blocking on the filesystem.
        let path = self.path.take();
        drop(self.lock.take());
        if let Some(path) = path {
            // `thread::spawn` panics if the OS refuses a new thread; a panic in
            // Drop during unwinding would abort the process. Use the fallible
            // Builder and fall back to synchronous removal so cleanup still runs.
            let spawned = std::thread::Builder::new()
                .name("staging-cleanup".to_string())
                .spawn({
                    let path = path.clone();
                    move || {
                        let _ = fs::remove_dir_all(path);
                    }
                });
            if spawned.is_err() {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
}

/// Returned when `deadline` passes before staging finished.
///
/// The worker maps this to a terminal run, so the text is what the user reads
/// on the queue card when a card or share stops answering mid-staging.
pub const STAGING_TIMED_OUT_ERROR: &str = "Staging timed out: the source stopped responding";

/// Build a staging directory linking to `selected`. The returned guard removes
/// the directory if it is not explicitly cleaned up. If `cancel` is set, staging
/// stops before linking the next selected file and drops its partial directory.
///
/// `deadline` bounds the whole walk the way `scan_directory_streaming` bounds a
/// scan: it is checked before each selected file, so it caps a slow source but
/// cannot interrupt a single `is_file`/`link_file` call already blocked in the
/// kernel. Only abandoning the join bounds that case; see `join_bounded`.
pub fn create_staging_dir(
    selected: &[String],
    cancel: Option<&AtomicBool>,
    deadline: Option<Instant>,
) -> Result<StagingDir, String> {
    if selected.is_empty() {
        return Err("No files selected to stage".to_string());
    }

    let root = std::env::temp_dir().join(format!("immich-shuttle-stage-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).map_err(|e| format!("Could not create staging dir: {e}"))?;
    let lock = match acquire_dir_lock(&root) {
        Ok(lock) => lock,
        Err(e) => {
            let _ = fs::remove_dir_all(&root);
            return Err(format!("Could not lock staging dir: {e}"));
        }
    };
    let mut guard = StagingDir {
        path: Some(root),
        lock: Some(lock),
        links: StagingPathMap::default(),
    };

    let base = common_ancestor(selected);
    let mut linked = 0_usize;
    let mut used: HashSet<PathBuf> = HashSet::new();
    for entry in selected {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err("Staging cancelled".to_string());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(STAGING_TIMED_OUT_ERROR.to_string());
        }
        let src = Path::new(entry);
        if !src.is_file() {
            continue;
        }
        let rel = base
            .as_ref()
            .and_then(|b| src.strip_prefix(b).ok())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(src.file_name().unwrap_or_default()));
        // Strip any `..`/`.`/root components so a crafted selection can never
        // resolve to a destination outside the temp staging sandbox.
        let Some(rel) = safe_relative(&rel) else {
            continue;
        };
        // Disambiguate name collisions — e.g. the same filename picked from two
        // drives with no common ancestor, which both collapse to the same
        // relative path — by nesting later hits under a numeric subfolder, so a
        // second file never silently overwrites the first in the staging dir.
        let mut dest = guard.path().join(&rel);
        let mut n = 1_usize;
        while used.contains(&dest) {
            dest = guard.path().join(n.to_string()).join(&rel);
            n += 1;
        }
        // Belt-and-suspenders: never write outside `root`.
        if !dest.starts_with(guard.path()) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            if fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        // A single unreadable/locked/failed file must not abort the whole batch;
        // skip it and keep staging the rest. The `linked == 0` check below still
        // fails the run when nothing could be staged at all.
        if link_file(src, &dest).is_err() {
            continue;
        }
        guard.links.push(dest.clone(), PathBuf::from(entry));
        used.insert(dest);
        linked += 1;
    }

    if linked == 0 {
        return Err("None of the selected files could be staged".to_string());
    }
    Ok(guard)
}

/// Remove a staging directory. Only the links are removed; targets are untouched.
pub fn cleanup_staging_dir(mut dir: StagingDir) {
    dir.cleanup();
}

/// Create and exclusively lock a per-run artifact's `.lock` file.
///
/// The returned handle must remain alive for the artifact's lifetime.
pub(crate) fn acquire_dir_lock(dir: &Path) -> std::io::Result<fs::File> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.join(".lock"))?;
    if lock.try_lock_exclusive()? {
        Ok(lock)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "temporary artifact lock is already held",
        ))
    }
}

fn link_file(src: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(src, dest).is_ok() {
            return Ok(());
        }
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(src, dest).is_ok() {
            return Ok(());
        }
    }
    if fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| format!("Could not stage {}: {e}", src.display()))
}

/// Longest common directory prefix of the parents of all paths.
fn common_ancestor(paths: &[String]) -> Option<PathBuf> {
    let mut parents = paths.iter().map(|p| {
        Path::new(p)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    });
    let first = parents.next()?;
    let mut common: Vec<Component> = first.components().collect();
    for parent in parents {
        let comps: Vec<Component> = parent.components().collect();
        let mut i = 0;
        while i < common.len() && i < comps.len() && common[i] == comps[i] {
            i += 1;
        }
        common.truncate(i);
        if common.is_empty() {
            return None;
        }
    }
    let mut out = PathBuf::new();
    for c in &common {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// Keep only the `Normal` components of `rel`, dropping any root/prefix/`.`/`..`
/// segments. This guarantees `root.join(result)` stays nested under `root`,
/// closing the path-traversal hole where a selection like `../../evil.jpg`
/// could otherwise link/copy outside the temp staging dir. Returns `None` when
/// nothing usable remains.
fn safe_relative(rel: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in rel.components() {
        if let Component::Normal(part) = comp {
            out.push(part);
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_selected_files_preserving_names() {
        let tmp = std::env::temp_dir().join(format!("stage-src-{}", Uuid::new_v4()));
        fs::create_dir_all(tmp.join("100")).unwrap();
        fs::create_dir_all(tmp.join("101")).unwrap();
        let a = tmp.join("100/IMG_1.JPG");
        let b = tmp.join("101/IMG_2.JPG");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();

        let staged = create_staging_dir(
            &[
                a.to_string_lossy().to_string(),
                b.to_string_lossy().to_string(),
            ],
            None,
            None,
        )
        .unwrap();

        assert!(staged.path().join("100/IMG_1.JPG").exists());
        assert!(staged.path().join("101/IMG_2.JPG").exists());
        assert_eq!(staged.links().entries().len(), 2);
        assert_eq!(
            staged
                .links()
                .original_for(&staged.path().join("100/IMG_1.JPG")),
            Some(a.as_path())
        );
        assert_eq!(
            staged
                .links()
                .original_for(&staged.path().join("101/IMG_2.JPG")),
            Some(b.as_path())
        );

        let staged_path = staged.path().to_path_buf();
        cleanup_staging_dir(staged);
        assert!(!staged_path.exists());
        // Originals survive cleanup.
        assert!(a.exists() && b.exists());
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn empty_selection_errors() {
        assert!(create_staging_dir(&[], None, None).is_err());
    }

    #[test]
    fn pre_cancelled_staging_aborts_before_linking_files() {
        let tmp = std::env::temp_dir().join(format!("stage-cancel-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let a = tmp.join("a.jpg");
        let b = tmp.join("b.jpg");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let cancel = AtomicBool::new(true);

        let error = match create_staging_dir(
            &[
                a.to_string_lossy().to_string(),
                b.to_string_lossy().to_string(),
            ],
            Some(&cancel),
            None,
        ) {
            Err(error) => error,
            Ok(_) => panic!("pre-cancelled staging must fail"),
        };
        assert_eq!(error, "Staging cancelled");
        assert!(a.exists() && b.exists());
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// A source that answers too slowly must end the run instead of holding the
    /// import worker — and its liveness markers — forever. The originals stay
    /// untouched, and the partial staging directory drops with the guard.
    #[test]
    fn an_expired_deadline_stops_staging_and_keeps_the_originals() {
        let tmp = std::env::temp_dir().join(format!("stage-deadline-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let a = tmp.join("a.jpg");
        fs::write(&a, b"a").unwrap();
        // Already passed: the check runs before the first file is linked.
        let deadline = Instant::now() - std::time::Duration::from_secs(1);

        let error =
            match create_staging_dir(&[a.to_string_lossy().to_string()], None, Some(deadline)) {
                Err(error) => error,
                Ok(_) => panic!("staging past its deadline must fail"),
            };

        assert_eq!(error, STAGING_TIMED_OUT_ERROR);
        assert!(a.exists(), "a timed-out staging run must not touch sources");
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn disambiguates_colliding_destination_names() {
        // Two selections that resolve to the same relative path (here, one file
        // picked twice — the same shape as identically-named files from two
        // drives with no common ancestor) must both be staged, not silently
        // overwrite each other.
        let tmp = std::env::temp_dir().join(format!("stage-collide-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let a = tmp.join("photo.jpg");
        fs::write(&a, b"a").unwrap();

        let staged = create_staging_dir(
            &[
                a.to_string_lossy().to_string(),
                a.to_string_lossy().to_string(),
            ],
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            walkdir_files(staged.path()).len(),
            2,
            "both entries must be staged without collision"
        );
        assert_eq!(staged.links().entries().len(), 2);
        assert_eq!(
            staged
                .links()
                .original_for(&staged.path().join("photo.jpg")),
            Some(a.as_path())
        );
        assert_eq!(
            staged
                .links()
                .original_for(&staged.path().join("1/photo.jpg")),
            Some(a.as_path())
        );

        cleanup_staging_dir(staged);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn safe_relative_strips_traversal() {
        assert_eq!(
            safe_relative(Path::new("../../evil.jpg")),
            Some(PathBuf::from("evil.jpg"))
        );
        assert_eq!(
            safe_relative(Path::new("a/../../b/c.jpg")),
            Some(PathBuf::from("a/b/c.jpg"))
        );
        assert_eq!(safe_relative(Path::new("../..")), None);
    }

    #[test]
    fn staged_files_never_escape_root() {
        // Two selections whose common ancestor leaves a `..`-laden relative path
        // for the first entry (the path-traversal PoC). Every staged file must
        // still land under the returned staging root.
        let tmp = std::env::temp_dir().join(format!("stage-trav-{}", Uuid::new_v4()));
        fs::create_dir_all(tmp.join("album/normal")).unwrap();
        let escape = tmp.join("evilfile.jpg");
        let normal = tmp.join("album/normal/file2.jpg");
        fs::write(&escape, b"x").unwrap();
        fs::write(&normal, b"y").unwrap();

        // Craft the first entry with literal `..` segments relative to the second.
        let crafted = format!("{}/album/../evilfile.jpg", tmp.to_string_lossy());
        let staged =
            create_staging_dir(&[crafted, normal.to_string_lossy().to_string()], None, None)
                .unwrap();

        for entry in walkdir_files(staged.path()) {
            assert!(
                entry.starts_with(staged.path()),
                "staged path escaped root: {}",
                entry.display()
            );
        }
        // The escaping original is untouched (never overwritten in place).
        assert_eq!(fs::read(&escape).unwrap(), b"x");

        cleanup_staging_dir(staged);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn dropping_staging_dir_removes_links() {
        let tmp = std::env::temp_dir().join(format!("stage-drop-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("photo.jpg");
        fs::write(&source, b"x").unwrap();

        let staged_path = {
            let staged =
                create_staging_dir(&[source.to_string_lossy().to_string()], None, None).unwrap();
            let path = staged.path().to_path_buf();
            assert!(path.exists());
            path
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while staged_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !staged_path.exists(),
            "detached cleanup did not finish within 1 second"
        );
        assert!(source.exists());
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn directory_lock_blocks_another_owner_until_released() {
        let dir = std::env::temp_dir().join(format!("stage-lock-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();

        let first = acquire_dir_lock(&dir).unwrap();
        let second = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(".lock"))
            .unwrap();
        assert!(!second.try_lock_exclusive().unwrap());

        drop(first);
        assert!(second.try_lock_exclusive().unwrap());

        drop(second);
        fs::remove_dir_all(dir).unwrap();
    }

    fn walkdir_files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().and_then(|n| n.to_str()) != Some(".lock") {
                    // Skip the per-run lease marker; it is not part of the staged payload.
                    out.push(path);
                }
            }
        }
        out
    }
}
