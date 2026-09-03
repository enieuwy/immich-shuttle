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
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[cfg(test)]
thread_local! {
    static FORCE_NEXT_LINK_FAILURE_AFTER_DESTINATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FORCE_STAGING_DESTINATION_REMOVAL_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FORCED_PARTIAL_DESTINATION: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

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

/// One selection that could not be staged, and why.
///
/// Finalization derives its errors from the immich-go run log, which can never
/// mention a file that was never offered to it. Without this record a silently
/// omitted selection is reported to the user as a clean success.
#[derive(Debug, Clone)]
pub struct StagingFailure {
    /// The selected path exactly as the caller gave it, so the caller can match
    /// the failure back to the entry the user picked.
    pub source: String,
    /// User-facing reason this selection never reached the staging directory.
    pub message: String,
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
    /// How many selections staging was asked for. Compared against `links` and
    /// `failures`, this is what lets a caller see that a run staged fewer files
    /// than the user picked.
    pub requested: usize,
    /// One entry per selection that was omitted. Always populated, including on
    /// a run that staged something.
    pub failures: Vec<StagingFailure>,
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

/// Returned when the source stops making progress before staging finished.
///
/// The worker maps this to a terminal run, so the text is what the user reads on
/// the queue card when a card or share stops answering mid-staging.
pub const STAGING_TIMED_OUT_ERROR: &str = "Staging timed out: the source stopped responding";

/// Build a staging directory linking to `selected`. The returned guard removes
/// the directory if it is not explicitly cleaned up. If `cancel` is set, staging
/// stops before linking the next selected file and drops its partial directory.
///
/// `stall` bounds how long the source may make NO progress, not how long staging
/// may take. A total-duration cap would be wrong here: `link_file` falls back to
/// copying whole files when neither a symlink nor a hard link is possible — on
/// Windows without developer mode, or across volumes — so a healthy large
/// selection can legitimately run for hours. Progress is one staged file, or one
/// chunk of one file being copied, and `progress` reports it to the caller so it
/// can apply the same test to a call blocked inside the kernel, which this loop
/// cannot see.
pub fn create_staging_dir(
    selected: &[String],
    cancel: Option<&AtomicBool>,
    stall: Option<Duration>,
    progress: &AtomicU64,
) -> Result<StagingDir, String> {
    if selected.is_empty() {
        return Err("No files selected to stage".to_string());
    }

    let root = std::env::temp_dir().join(format!("immich-shuttle-stage-{}", Uuid::new_v4()));
    // Owner-only, like the run-log and immich-go config directories: on Linux
    // `temp_dir()` is the shared `/tmp`, and the staged tree carries the user's
    // photos under their original names. At the default umask another local
    // user could list the selection and read a staged copy of every picture.
    #[cfg(unix)]
    let mut dir_builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut b = fs::DirBuilder::new();
        b.mode(0o700);
        b
    };
    #[cfg(not(unix))]
    let mut dir_builder = fs::DirBuilder::new();
    dir_builder
        .recursive(true)
        .create(&root)
        .map_err(|e| format!("Could not create staging dir: {e}"))?;
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
        requested: selected.len(),
        failures: Vec::new(),
    };

    let base = common_ancestor(selected);
    let mut linked = 0_usize;
    let mut used: HashSet<PathBuf> = HashSet::new();
    let mut last_progress = Instant::now();
    let mut seen_progress = progress.load(Ordering::Relaxed);
    for entry in selected {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err("Staging cancelled".to_string());
        }
        let now_progress = progress.load(Ordering::Relaxed);
        if now_progress != seen_progress {
            seen_progress = now_progress;
            last_progress = Instant::now();
        }
        if stall.is_some_and(|stall| last_progress.elapsed() >= stall) {
            return Err(STAGING_TIMED_OUT_ERROR.to_string());
        }
        let src = Path::new(entry);
        if !src.is_file() {
            guard.failures.push(StagingFailure {
                source: entry.clone(),
                message: "Not a file: it is missing, a folder, or unreadable".to_string(),
            });
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
            guard.failures.push(StagingFailure {
                source: entry.clone(),
                message: "The selection has no usable file name".to_string(),
            });
            continue;
        };
        // Disambiguate name collisions — e.g. the same filename picked from two
        // drives with no common ancestor, which both collapse to the same
        // relative path — by nesting later hits under a numeric subfolder, so a
        // second file never silently overwrites the first in the staging dir.
        //
        // `used` is a fast pre-check only, never the decision. It compares
        // paths byte-for-byte, while the staging filesystem may fold case or
        // Unicode normalization (APFS, HFS+, NTFS): there `IMG_1.JPG` and
        // `img_1.jpg` are two entries in `used` but one entry on disk. So the
        // filesystem decides — every candidate is created exclusively, and an
        // `AlreadyExists` moves to the next candidate instead of writing
        // through what is already there, which for a staged symlink means
        // truncating another selection's original on the source media.
        //
        // The retry is bounded: a collision can only come from a destination an
        // earlier entry staged. Failed attempts either remove their destination
        // or abort staging, so no unknown bytes remain in this upload tree.
        // The margin stops a filesystem that keeps calling a name taken without
        // ever letting us create it.
        let attempts = used.len() + 8;
        let mut staged = None;
        let mut failure: Option<String> = None;
        for attempt in 0..attempts {
            let dest = match attempt {
                0 => guard.path().join(&rel),
                n => guard.path().join(n.to_string()).join(&rel),
            };
            if used.contains(&dest) {
                continue;
            }
            // Belt-and-suspenders: never write outside `root`.
            if !dest.starts_with(guard.path()) {
                failure =
                    Some("The staging destination fell outside the staging folder".to_string());
                break;
            }
            if let Some(parent) = dest.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    failure = Some(format!("Could not create the staging folder: {e}"));
                    break;
                }
            }
            match link_file(src, &dest, progress) {
                Ok(()) => {
                    staged = Some(dest);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    // The attempt may have created the destination before it
                    // failed — the copy fallback opens `dest` with `create_new`
                    // and can still die on the read or the write. Anything left
                    // there is a partial file that immich-go would upload as a
                    // complete asset, and that is absent from `links`, so it
                    // could not even be mapped back to its original.
                    match remove_staging_destination(&dest) {
                        Ok(()) => {}
                        Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => {}
                        Err(cleanup) => {
                            // Do not return a guard that points an uploader at a
                            // tree containing bytes we could not prove safe.
                            guard.cleanup();
                            return Err(format!(
                                "Could not stage the file: {e}; could not remove the partial staging output: {cleanup}"
                            ));
                        }
                    }
                    failure = Some(format!("Could not stage the file: {e}"));
                    break;
                }
            }
        }
        // A single unreadable/locked/failed file must not abort the whole batch;
        // record why it was omitted and keep staging the rest. The `linked == 0`
        // check below still fails the run when nothing could be staged at all.
        let Some(dest) = staged else {
            guard.failures.push(StagingFailure {
                source: entry.clone(),
                message: failure.unwrap_or_else(|| {
                    format!("Staging failed after {attempts} attempts: every destination name was taken")
                }),
            });
            continue;
        };
        guard.links.push(dest.clone(), PathBuf::from(entry));
        used.insert(dest);
        linked += 1;
        // A staged file is progress even when it cost one symlink syscall, which
        // is the whole cost on a healthy card. Without this tick, a run of many
        // small files would look stalled to the watchdog above.
        progress.fetch_add(1, Ordering::Relaxed);
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

fn remove_staging_destination(dest: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FORCE_STAGING_DESTINATION_REMOVAL_FAILURE.with(std::cell::Cell::get) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected staging destination removal failure",
        ));
    }
    fs::remove_file(dest)
}

/// Link `src` into `dest`, falling back to a copy, and report bytes moved.
///
/// Every attempt is exclusive, and an `AlreadyExists` error is returned as it
/// is instead of being read as a missing capability: a taken `dest` is a
/// collision only the caller can resolve, by picking another name. Falling
/// through to the copy would open whatever already sits there, and when that is
/// the symlink staging created for an earlier selection, opening it truncates
/// that user's original file.
///
/// The copy is chunked rather than `fs::copy` so a single large file still ticks
/// `progress`. Callers use that tick to tell a source that is working from one
/// that has stopped answering; a whole-file `fs::copy` looks identical to a dead
/// mount for as long as it runs, which on Windows or across volumes — the only
/// cases that reach the fallback — can be minutes per file.
fn link_file(src: &Path, dest: &Path, progress: &AtomicU64) -> std::io::Result<()> {
    #[cfg(test)]
    if FORCE_NEXT_LINK_FAILURE_AFTER_DESTINATION.with(|failure| failure.replace(false)) {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)?;
        FORCED_PARTIAL_DESTINATION.with(|path| {
            path.replace(Some(dest.to_path_buf()));
        });
        return Err(std::io::Error::other("injected failed staging link"));
    }

    #[cfg(unix)]
    {
        match std::os::unix::fs::symlink(src, dest) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(src, dest) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
    }
    match fs::hard_link(src, dest) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
        Err(_) => {}
    }
    copy_reporting_progress(src, dest, progress)
}

/// Chunk size for the copy fallback. Large enough that the read/write syscalls
/// dominate the loop, small enough that a working source ticks progress often.
const COPY_CHUNK: usize = 1024 * 1024;

fn copy_reporting_progress(src: &Path, dest: &Path, progress: &AtomicU64) -> std::io::Result<()> {
    let mut reader = fs::File::open(src)?;
    // `create_new`, never `File::create`: create truncates an existing
    // destination and follows a symlink at that path, so staging would write
    // through the link made for an earlier selection and destroy that file's
    // original on the source media before anything was uploaded.
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    // A copy is real photo bytes in the shared temp directory, not a link to a
    // file the owner already protects, so it is created owner-only for the same
    // reason the staging root is. The link paths are untouched: a symlink's own
    // mode is not consulted, and a hard link is the original file's inode,
    // whose mode is the user's to choose.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut writer = opts.open(dest)?;
    // `create_new` proved nothing else owned `dest`, so the file is ours from
    // here and we must not leave it behind half-written. A read error on the
    // source or a write error on a dying mount stops the copy mid-file, and
    // immich-go uploads whatever it finds in the staging tree: a truncated
    // photo would reach the server as a complete asset, and because the caller
    // never records a failed destination in its link map, it could not be
    // mapped back to its original either.
    let copied = copy_chunks(&mut reader, &mut writer, progress);
    if copied.is_err() {
        // Close the handle first so the removal cannot race our own writer.
        // `create_staging_dir` retries this removal and aborts when it fails.
        drop(writer);
        let _ = remove_staging_destination(dest);
    }
    copied
}

fn copy_chunks(
    reader: &mut fs::File,
    writer: &mut fs::File,
    progress: &AtomicU64,
) -> std::io::Result<()> {
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        let read = reader.read(&mut buf)?;
        if read == 0 {
            return Ok(());
        }
        writer.write_all(&buf[..read])?;
        progress.fetch_add(read as u64, Ordering::Relaxed);
    }
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

    struct StagingLinkFailure;

    impl StagingLinkFailure {
        fn enable(cleanup_fails: bool) -> Self {
            FORCE_NEXT_LINK_FAILURE_AFTER_DESTINATION.with(|failure| failure.set(true));
            FORCE_STAGING_DESTINATION_REMOVAL_FAILURE.with(|failure| failure.set(cleanup_fails));
            FORCED_PARTIAL_DESTINATION.with(|path| {
                path.replace(None);
            });
            Self
        }

        fn partial_destination(&self) -> Option<PathBuf> {
            FORCED_PARTIAL_DESTINATION.with(|path| path.borrow().clone())
        }
    }

    impl Drop for StagingLinkFailure {
        fn drop(&mut self) {
            FORCE_NEXT_LINK_FAILURE_AFTER_DESTINATION.with(|failure| failure.set(false));
            FORCE_STAGING_DESTINATION_REMOVAL_FAILURE.with(|failure| failure.set(false));
            FORCED_PARTIAL_DESTINATION.with(|path| {
                path.replace(None);
            });
        }
    }

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
            &AtomicU64::new(0),
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

    /// On Linux `std::env::temp_dir()` is the shared `/tmp`, so the staged tree
    /// must not be readable by other local users: the filenames alone disclose
    /// what the user is importing, and a copied destination is the photo itself.
    #[cfg(unix)]
    #[test]
    fn the_staging_root_and_copied_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = std::env::temp_dir().join(format!("stage-modes-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("IMG_1.JPG");
        fs::write(&src, b"photo bytes").unwrap();

        let staged = create_staging_dir(
            &[src.to_string_lossy().to_string()],
            None,
            None,
            &AtomicU64::new(0),
        )
        .unwrap();
        let root_mode = fs::metadata(staged.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(root_mode, 0o700, "staging root mode was {root_mode:o}");

        // Only the copy fallback writes photo bytes into the staging tree, and
        // a healthy unix host always gets a symlink, so the copy is called
        // directly — the same seam the other copy-fallback tests use.
        let copied = staged.path().join("copied.JPG");
        copy_reporting_progress(&src, &copied, &AtomicU64::new(0)).unwrap();
        let copy_mode = fs::metadata(&copied).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            copy_mode, 0o600,
            "copied destination mode was {copy_mode:o}"
        );

        cleanup_staging_dir(staged);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn empty_selection_errors() {
        assert!(create_staging_dir(&[], None, None, &AtomicU64::new(0)).is_err());
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
            &AtomicU64::new(0),
        ) {
            Err(error) => error,
            Ok(_) => panic!("pre-cancelled staging must fail"),
        };
        assert_eq!(error, "Staging cancelled");
        assert!(a.exists() && b.exists());
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// A source that has gone silent must end the run instead of holding the
    /// import worker — and its liveness markers — forever. The originals stay
    /// untouched, and the partial staging directory drops with the guard.
    #[test]
    fn a_stalled_source_stops_staging_and_keeps_the_originals() {
        let tmp = std::env::temp_dir().join(format!("stage-stall-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let a = tmp.join("a.jpg");
        fs::write(&a, b"a").unwrap();
        // Zero tolerance for silence: the check runs before the first file is
        // staged, when nothing has reported progress yet.
        let error = match create_staging_dir(
            &[a.to_string_lossy().to_string()],
            None,
            Some(Duration::ZERO),
            &AtomicU64::new(0),
        ) {
            Err(error) => error,
            Ok(_) => panic!("a stalled source must fail the run"),
        };

        assert_eq!(error, STAGING_TIMED_OUT_ERROR);
        assert!(a.exists(), "a timed-out staging run must not touch sources");
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// The bound is silence, not duration: a source that keeps staging files is
    /// never called unresponsive, however long the whole selection takes. This is
    /// what a large hand-picked import looks like when `link_file` has to copy.
    #[test]
    fn a_source_that_keeps_working_is_never_called_stalled() {
        let tmp = std::env::temp_dir().join(format!("stage-progress-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let selected: Vec<String> = (0..12)
            .map(|index| {
                let file = tmp.join(format!("IMG_{index}.jpg"));
                fs::write(&file, b"x").unwrap();
                file.to_string_lossy().to_string()
            })
            .collect();
        let progress = AtomicU64::new(0);

        // Short enough that a total-duration bound of this size would fire part
        // way through; each staged file resets it instead.
        let staged =
            create_staging_dir(&selected, None, Some(Duration::from_millis(50)), &progress)
                .expect("a source making progress must stage every file");

        assert_eq!(staged.links().entries().len(), 12);
        // At least one tick per staged file. Not an equality: where a symlink is
        // impossible the copy fallback also ticks per chunk, so the exact count
        // is platform-dependent while the invariant the watchdog relies on — a
        // working source keeps ticking — is not.
        assert!(
            progress.load(Ordering::Relaxed) >= 12,
            "each staged file reports progress, symlink or copy"
        );
        cleanup_staging_dir(staged);
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
            &AtomicU64::new(0),
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

    /// Staging must never write through what already sits at a destination. The
    /// entry there is a symlink to a user's original, so an opening truncate
    /// destroys the very file the run exists to upload — before one byte is
    /// sent. Both the copy fallback and `link_file` must refuse instead.
    #[test]
    fn an_existing_destination_is_refused_and_the_source_survives() {
        let tmp = std::env::temp_dir().join(format!("stage-clobber-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let original = tmp.join("original.jpg");
        fs::write(&original, b"original bytes").unwrap();
        let other = tmp.join("other.jpg");
        fs::write(&other, b"other bytes").unwrap();
        // The destination an earlier selection would have staged: a symlink
        // pointing straight at a source file.
        let dest = tmp.join("staged.jpg");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&original, &dest).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&original, &dest).unwrap();

        let error = copy_reporting_progress(&other, &dest, &AtomicU64::new(0))
            .expect_err("the copy fallback must refuse an existing destination");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&original).unwrap(), b"original bytes");

        // A taken destination is a collision, not a missing link capability, so
        // `link_file` reports it rather than falling back to the copy.
        let error = link_file(&other, &dest, &AtomicU64::new(0))
            .expect_err("a taken destination must not reach the copy fallback");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&original).unwrap(), b"original bytes");
        assert_eq!(fs::read(&other).unwrap(), b"other bytes");
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// Two selections whose staged relative names differ only by ASCII case
    /// alias to ONE destination on a case-insensitive volume (default APFS,
    /// NTFS), which the byte-for-byte `used` set cannot see. Each selection
    /// still needs its own staged entry, and no source may be written through.
    #[test]
    fn case_variant_names_stage_to_distinct_destinations() {
        let tmp = std::env::temp_dir().join(format!("stage-case-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let lower = tmp.join("photo.jpg");
        let upper = tmp.join("PHOTO.JPG");
        fs::write(&lower, b"lower bytes").unwrap();
        fs::write(&upper, b"upper bytes").unwrap();
        // One write clobbering the other is how the test learns the volume
        // folds case: there the two selections are one file under two names.
        let folds_case = fs::read(&lower).unwrap() == b"upper bytes";

        let staged = create_staging_dir(
            &[
                lower.to_string_lossy().to_string(),
                upper.to_string_lossy().to_string(),
            ],
            None,
            None,
            &AtomicU64::new(0),
        )
        .unwrap();

        let destinations: Vec<PathBuf> = staged
            .links()
            .entries()
            .iter()
            .map(|(destination, _)| destination.clone())
            .collect();
        assert_eq!(destinations.len(), 2, "both selections must be staged");
        assert_ne!(
            destinations[0], destinations[1],
            "case variants must not share one staged destination"
        );
        assert_eq!(
            walkdir_files(staged.path()).len(),
            2,
            "each selection needs its own entry on the staging filesystem"
        );
        if folds_case {
            // The filesystem, not `used`, caught the alias, so the second entry
            // took the next disambiguated candidate.
            assert!(destinations[1].starts_with(staged.path().join("1")));
        }
        for destination in &destinations {
            assert!(
                !fs::read(destination).unwrap().is_empty(),
                "a staged entry must resolve to its source's bytes"
            );
        }

        // The sources are exactly as the setup left them: nothing was truncated.
        let expected_lower: &[u8] = if folds_case {
            b"upper bytes"
        } else {
            b"lower bytes"
        };
        assert_eq!(fs::read(&lower).unwrap(), expected_lower);
        assert_eq!(fs::read(&upper).unwrap(), b"upper bytes");

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
        let staged = create_staging_dir(
            &[crafted, normal.to_string_lossy().to_string()],
            None,
            None,
            &AtomicU64::new(0),
        )
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
            let staged = create_staging_dir(
                &[source.to_string_lossy().to_string()],
                None,
                None,
                &AtomicU64::new(0),
            )
            .unwrap();
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

    // A mixed selection stages each real file and ignores missing or directory entries.
    #[test]
    fn mixed_selection_stages_only_existing_files() {
        let tmp = std::env::temp_dir().join(format!("stage-mixed-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let first = tmp.join("first.jpg");
        let second = tmp.join("second.jpg");
        let directory = tmp.join("album");
        let missing = tmp.join("missing.jpg");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        fs::create_dir_all(&directory).unwrap();

        let selected = vec![
            first.to_string_lossy().to_string(),
            missing.to_string_lossy().to_string(),
            directory.to_string_lossy().to_string(),
            second.to_string_lossy().to_string(),
        ];
        let staged = create_staging_dir(&selected, None, None, &AtomicU64::new(0)).unwrap();

        assert_eq!(staged.links().entries().len(), 2);
        assert_eq!(
            staged
                .links()
                .entries()
                .iter()
                .filter(|(_, original)| original == &first || original == &second)
                .count(),
            2
        );
        assert!(staged
            .links()
            .entries()
            .iter()
            .all(|(_, original)| original != &missing && original != &directory));

        // Accepting 2 of 4 is only correct if the caller can see the other 2.
        // Finalization builds its errors from the immich-go run log, which can
        // never mention a file that was never offered to it.
        assert_eq!(staged.requested, 4);
        let omitted: Vec<String> = staged
            .failures
            .iter()
            .map(|failure| failure.source.clone())
            .collect();
        assert_eq!(
            omitted,
            vec![
                missing.to_string_lossy().to_string(),
                directory.to_string_lossy().to_string(),
            ]
        );
        assert!(
            staged
                .failures
                .iter()
                .all(|failure| failure.message.contains("Not a file")),
            "each omission must name its reason: {:?}",
            staged.failures
        );

        cleanup_staging_dir(staged);
        fs::remove_dir_all(&tmp).unwrap();
    }

    // An all-invalid selection reports that no selected file can be staged.
    #[test]
    fn selection_with_only_missing_files_returns_staging_error() {
        let tmp = std::env::temp_dir().join(format!("stage-missing-{}", Uuid::new_v4()));
        let first = tmp.join("one.jpg");
        let second = tmp.join("two.jpg");
        let error = match create_staging_dir(
            &[
                first.to_string_lossy().to_string(),
                second.to_string_lossy().to_string(),
            ],
            None,
            None,
            &AtomicU64::new(0),
        ) {
            Err(error) => error,
            Ok(_) => panic!("an all-missing selection must fail"),
        };

        assert!(
            error.contains("None of the selected files could be staged"),
            "unexpected staging error: {error}"
        );
    }

    /// The copy fallback opens `dest` with `create_new` and only then starts
    /// reading, so a source that dies mid-copy leaves a TRUNCATED file in the
    /// staging tree. immich-go uploads whatever it finds there, and the failed
    /// destination is absent from the link map, so that partial photo would
    /// reach the server as a complete asset and escape the completed-path
    /// bookkeeping delete-after-import relies on. Nothing may survive a failed
    /// copy.
    ///
    /// Unix only: the failure is provoked deterministically by copying from a
    /// directory, where `open` succeeds and the first `read` returns `EISDIR`,
    /// which is exactly a source that stops answering after the destination
    /// exists. Windows refuses the `open`, so the destination is never created
    /// there and the test would prove nothing.
    #[cfg(unix)]
    #[test]
    fn a_failed_copy_leaves_no_partial_file_at_the_destination() {
        let tmp = std::env::temp_dir().join(format!("stage-partial-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        // Opens like a file, fails on the first read.
        let unreadable_source = tmp.join("source");
        fs::create_dir(&unreadable_source).unwrap();
        let dest = tmp.join("staged.jpg");

        let error = copy_reporting_progress(&unreadable_source, &dest, &AtomicU64::new(0))
            .expect_err("a source that cannot be read must fail the copy");

        assert_ne!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "the destination must have been created before the read failed"
        );
        assert!(
            !dest.exists(),
            "a failed copy must not leave a partial file at {}",
            dest.display()
        );
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// A failed attempt can leave a destination behind after creating it. If
    /// removing that artifact also fails, returning a partial `StagingDir`
    /// would let immich-go scan and upload the unknown file.
    #[test]
    fn cleanup_failure_aborts_staging_and_removes_the_upload_tree() {
        let tmp = std::env::temp_dir().join(format!("stage-cleanup-failure-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("source.jpg");
        fs::write(&source, b"original bytes").unwrap();

        let fault = StagingLinkFailure::enable(true);
        let error = match create_staging_dir(
            &[source.to_string_lossy().to_string()],
            None,
            None,
            &AtomicU64::new(0),
        ) {
            Err(error) => error,
            Ok(staged) => {
                cleanup_staging_dir(staged);
                panic!("a cleanup failure must not return an upload tree");
            }
        };
        let partial_destination = fault
            .partial_destination()
            .expect("the injected failed attempt must create a destination");
        drop(fault);

        assert!(
            error.contains("could not remove the partial staging output"),
            "the cleanup error must reject staging: {error}"
        );
        assert!(
            !partial_destination.parent().unwrap().exists(),
            "an error must leave no staging tree for an uploader to scan"
        );
        assert_eq!(fs::read(&source).unwrap(), b"original bytes");
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn a_failed_attempt_with_successful_cleanup_allows_later_staging() {
        let tmp = std::env::temp_dir().join(format!("stage-cleanup-success-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let failed = tmp.join("failed.jpg");
        let valid = tmp.join("valid.jpg");
        fs::write(&failed, b"first original").unwrap();
        fs::write(&valid, b"second original").unwrap();

        let fault = StagingLinkFailure::enable(false);
        let staged = create_staging_dir(
            &[
                failed.to_string_lossy().to_string(),
                valid.to_string_lossy().to_string(),
            ],
            None,
            None,
            &AtomicU64::new(0),
        )
        .expect("a cleaned failed attempt must not abort later staging");
        let partial_destination = fault
            .partial_destination()
            .expect("the injected failed attempt must create a destination");
        drop(fault);

        assert!(!partial_destination.exists());
        assert_eq!(staged.links().entries().len(), 1);
        assert_eq!(staged.failures.len(), 1);
        assert_eq!(
            staged
                .links()
                .original_for(&staged.path().join("valid.jpg")),
            Some(valid.as_path())
        );
        assert_eq!(fs::read(&failed).unwrap(), b"first original");
        assert_eq!(fs::read(&valid).unwrap(), b"second original");

        cleanup_staging_dir(staged);
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// Omitted selections must not consume the destination names later
    /// selections need. A run of same-name failures used to be able to occupy
    /// candidate after candidate, and the bound counted only successes, so a
    /// perfectly readable file that resolved to that name found no free
    /// candidate and was dropped from the import without a word.
    #[test]
    fn a_run_of_same_name_failures_still_lets_a_later_valid_file_stage() {
        let tmp = std::env::temp_dir().join(format!("stage-starve-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        // A relative entry leaves the selection with no common ancestor, so
        // every entry stages under its bare file name — the "same file name
        // from two drives" shape, which is the only way distinct selections
        // compete for one relative name. The name is one no working directory
        // holds, so this entry is itself an omitted selection.
        let mut selected = vec![format!("no-such-relative-{}.jpg", Uuid::new_v4())];
        for index in 0..8 {
            let folder = tmp.join(format!("d{index}"));
            fs::create_dir_all(&folder).unwrap();
            // Named exactly like the valid file below, and unstageable.
            let decoy = folder.join("photo.jpg");
            fs::create_dir(&decoy).unwrap();
            selected.push(decoy.to_string_lossy().to_string());
        }
        let valid_folder = tmp.join("real");
        fs::create_dir_all(&valid_folder).unwrap();
        let valid = valid_folder.join("photo.jpg");
        fs::write(&valid, b"valid bytes").unwrap();
        selected.push(valid.to_string_lossy().to_string());

        let staged = create_staging_dir(&selected, None, None, &AtomicU64::new(0))
            .expect("a valid file after a run of failures must still stage");

        assert_eq!(staged.links().entries().len(), 1);
        let first_candidate = staged.path().join("photo.jpg");
        assert_eq!(
            staged.links().original_for(&first_candidate),
            Some(valid.as_path()),
            "the failures freed nothing, so the valid file took the first candidate"
        );
        assert_eq!(fs::read(&first_candidate).unwrap(), b"valid bytes");
        assert_eq!(staged.requested, 10);
        assert_eq!(
            staged.failures.len(),
            9,
            "every omitted selection is reported: {:?}",
            staged.failures
        );

        cleanup_staging_dir(staged);
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// `requested` and `failures` are the caller's only evidence that a run
    /// staged fewer files than the user picked, so they must be exact: one
    /// entry per omitted selection, in selection order, and duplicates of a
    /// valid file counted once each rather than folded together.
    #[test]
    fn requested_and_failures_are_exact_for_a_mixed_selection() {
        let tmp = std::env::temp_dir().join(format!("stage-count-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let valid = tmp.join("kept.jpg");
        fs::write(&valid, b"kept").unwrap();
        let folder = tmp.join("album");
        fs::create_dir_all(&folder).unwrap();
        let missing = tmp.join("gone.jpg");
        let also_missing = tmp.join("gone-too.jpg");

        let selected = vec![
            missing.to_string_lossy().to_string(),
            valid.to_string_lossy().to_string(),
            folder.to_string_lossy().to_string(),
            valid.to_string_lossy().to_string(),
            also_missing.to_string_lossy().to_string(),
        ];
        let staged = create_staging_dir(&selected, None, None, &AtomicU64::new(0)).unwrap();

        assert_eq!(staged.requested, 5);
        assert_eq!(staged.links().entries().len(), 2);
        let omitted: Vec<String> = staged
            .failures
            .iter()
            .map(|failure| failure.source.clone())
            .collect();
        assert_eq!(
            omitted,
            vec![
                missing.to_string_lossy().to_string(),
                folder.to_string_lossy().to_string(),
                also_missing.to_string_lossy().to_string(),
            ]
        );
        // Staged plus omitted accounts for every selection, which is what lets
        // finalization refuse to call a partial run a clean success.
        assert_eq!(
            staged.links().entries().len() + staged.failures.len(),
            staged.requested
        );
        for failure in &staged.failures {
            assert!(
                !failure.message.is_empty(),
                "every omission needs a user-facing reason"
            );
        }

        cleanup_staging_dir(staged);
        fs::remove_dir_all(&tmp).unwrap();
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
