use std::{
    collections::HashSet,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Condvar, LazyLock, Mutex,
    },
    time::{Duration, Instant},
};

use walkdir::WalkDir;

use crate::models::media::MediaFile;
#[cfg(test)]
use crate::models::media::ScanResult;

/// How long a claim waits for the previous walk of the same root to return.
///
/// A cancelled walk on a responsive source exits at its next [`WalkDir`] entry,
/// so the wait normally ends at once. Only a source whose filesystem call is
/// genuinely blocked holds the claim for the whole grace period.
pub(crate) const CLAIM_GRACE: Duration = Duration::from_secs(5);

/// Source roots whose blocking walks have not returned yet.
///
/// A cancelled `spawn_blocking` task keeps running when its filesystem call
/// blocks. This registry limits a hung source to one leaked walking thread.
static IN_FLIGHT_SCAN_ROOTS: LazyLock<(Mutex<HashSet<String>>, Condvar)> =
    LazyLock::new(|| (Mutex::new(HashSet::new()), Condvar::new()));

/// Which walk owns a claim. The grid scan, the preflight forecast, and the
/// staging step of a hand-picked import walk the same roots for different
/// answers and legitimately run at the same time, so they claim in separate
/// namespaces and never block each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanPurpose {
    Scan,
    Forecast,
    Stage,
}

impl fmt::Display for ScanPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanPurpose::Scan => f.write_str("scan"),
            ScanPurpose::Forecast => f.write_str("forecast"),
            ScanPurpose::Stage => f.write_str("stage"),
        }
    }
}

fn claim_keys(purpose: ScanPurpose, roots: &[String]) -> Vec<String> {
    roots
        .iter()
        .map(|root| format!("{purpose}:{root}"))
        .collect()
}

/// Owns source-root entries until the blocking walk returns.
///
/// The guard must move into the walking task. Dropping the caller's join handle
/// cannot stop a running `WalkDir` call, so cleanup there would admit duplicates.
#[derive(Debug)]
pub(crate) struct InFlightScanRoots {
    keys: Vec<String>,
}

impl Drop for InFlightScanRoots {
    fn drop(&mut self) {
        let (lock, released) = &*IN_FLIGHT_SCAN_ROOTS;
        let mut roots = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in &self.keys {
            roots.remove(key);
        }
        drop(roots);
        // Wake every caller waiting to re-claim one of these roots.
        released.notify_all();
    }
}

/// Claims every collapsed root for one blocking walk of `purpose`.
///
/// Waits up to [`CLAIM_GRACE`] for a previous walk of the same root to return,
/// because the caller normally cancels that walk immediately before claiming and
/// a responsive source releases it within one entry. After the grace the claim
/// fails fast and names the source that has not returned: an empty result would
/// incorrectly tell the caller that the source holds no media.
pub(crate) fn acquire_scan_roots(
    purpose: ScanPurpose,
    roots: &[String],
) -> Result<InFlightScanRoots, String> {
    let keys = claim_keys(purpose, roots);
    let (lock, released) = &*IN_FLIGHT_SCAN_ROOTS;
    let mut in_flight = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let deadline = Instant::now() + CLAIM_GRACE;
    while let Some(taken) = keys.iter().position(|key| in_flight.contains(key)) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "Source is still being scanned and is not responding: {}",
                roots[taken]
            ));
        }
        let (guard, _) = released
            .wait_timeout(in_flight, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        in_flight = guard;
    }
    in_flight.extend(keys.iter().cloned());
    Ok(InFlightScanRoots { keys })
}

/// The reason a directory scan stopped before producing a complete result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    Cancelled,
    TimedOut,
    Failed(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Directory scan was cancelled"),
            Self::TimedOut => f.write_str("Directory scan timed out"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ScanError {}

fn check_scan_controls(
    cancellation: Option<&AtomicBool>,
    deadline: Option<Instant>,
) -> Result<(), ScanError> {
    if cancellation.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
        return Err(ScanError::Cancelled);
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(ScanError::TimedOut);
    }
    Ok(())
}

fn supported_extensions() -> HashSet<&'static str> {
    [
        ".jpg", ".jpeg", ".png", ".heic", ".heif", ".avif", ".tiff", ".tif", ".gif", ".bmp",
        ".webp", ".raw", ".dng", ".cr2", ".cr3", ".nef", ".arw", ".orf", ".rw2", ".raf", ".mp4",
        ".mov", ".m4v", ".avi", ".mkv",
    ]
    .into_iter()
    .collect()
}

pub(crate) fn is_video_ext(ext: &str) -> bool {
    matches!(ext, ".mp4" | ".mov" | ".m4v" | ".avi" | ".mkv")
}

/// Lowercased, leading-dot extension of a path (e.g. "/a/IMG.JPG" -> ".jpg").
/// Empty string when the path has no extension.
pub(crate) fn extension_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default()
}

pub const STREAM_BATCH_SIZE: usize = 256;

/// Scan a source path in bounded batches, stopping before processing an entry
/// when cancelled or when `deadline` has elapsed.
///
/// The controls are checked before the scan begins and between [`WalkDir`]
/// entries. `on_batch` receives each full batch and the final remainder; the
/// return value is the number of unreadable entries skipped during the walk.
pub fn scan_directory_streaming(
    path: &Path,
    cancellation: Option<&AtomicBool>,
    deadline: Option<Instant>,
    on_batch: &mut dyn FnMut(Vec<MediaFile>),
) -> Result<usize, ScanError> {
    check_scan_controls(cancellation, deadline)?;
    if !path.exists() {
        return Err(ScanError::Failed(format!(
            "Source path does not exist: {}",
            path.display()
        )));
    }

    let exts = supported_extensions();
    let mut files: Vec<MediaFile> = Vec::with_capacity(STREAM_BATCH_SIZE);
    let mut skipped_unreadable = 0_usize;

    if path.is_file() {
        check_scan_controls(cancellation, deadline)?;
        let ext = path
            .extension()
            .map(|v| format!(".{}", v.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        if exts.contains(ext.as_str()) {
            let meta = fs::metadata(path)
                .map_err(|e| ScanError::Failed(format!("Could not read file metadata: {e}")))?;
            files.push(MediaFile {
                path: path.to_string_lossy().to_string(),
                name: path
                    .file_name()
                    .map(|v| v.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string()),
                extension: ext.clone(),
                size_bytes: meta.len(),
                is_video: is_video_ext(ext.as_str()),
            });
        }
    } else {
        let mut entries = WalkDir::new(path).into_iter();
        loop {
            check_scan_controls(cancellation, deadline)?;
            let Some(entry) = entries.next() else {
                break;
            };
            check_scan_controls(cancellation, deadline)?;
            let entry = match entry {
                Ok(v) => v,
                Err(_) => {
                    skipped_unreadable += 1;
                    continue;
                }
            };
            // Don't follow symlinks discovered inside the tree: a link pointing
            // outside the selected source could otherwise be scanned (and later
            // staged/uploaded), leaking files from outside the chosen folder.
            if entry.path_is_symlink() {
                continue;
            }
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .map(|v| format!(".{}", v.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            if !exts.contains(ext.as_str()) {
                continue;
            }
            let meta = match fs::metadata(p) {
                Ok(v) => v,
                Err(_) => {
                    skipped_unreadable += 1;
                    continue;
                }
            };
            files.push(MediaFile {
                path: p.to_string_lossy().to_string(),
                name: p
                    .file_name()
                    .map(|v| v.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.display().to_string()),
                extension: ext.clone(),
                size_bytes: meta.len(),
                is_video: is_video_ext(ext.as_str()),
            });
            if files.len() >= STREAM_BATCH_SIZE {
                on_batch(std::mem::take(&mut files));
            }
        }
    }

    if !files.is_empty() {
        on_batch(files);
    }
    Ok(skipped_unreadable)
}

/// Enumerate every regular file under `path`, with no extension filter, in
/// bounded batches.
///
/// This exists for the pre-sidecar source manifest, which asks a different
/// question from the preview grid: not "which files does this app show" but
/// "which files existed under this source before the sidecar started". The
/// preview allowlist is deliberately narrower than what immich-go uploads (it
/// omits `.mts`, `.webm`, `.3gp`, and several vendor raw formats), so filtering
/// here would leave a legitimately uploaded clip outside the manifest — and the
/// import worker then reports the run as unproven and keeps that original
/// forever.
///
/// The cancellation, deadline, symlink, and skipped-unreadable semantics are
/// exactly those of [`scan_directory_streaming`]; only the extension test and
/// the per-file metadata read are gone, because a manifest needs the path and
/// nothing else.
///
/// `progress` counts entries the walk has examined, including skipped ones. It
/// is the only signal a caller has that a walk blocked inside the kernel is
/// still alive: the deadline above is checked between entries, so it can never
/// interrupt a `readdir` that never returns.
pub fn manifest_directory_streaming(
    path: &Path,
    cancellation: Option<&AtomicBool>,
    deadline: Option<Instant>,
    progress: &AtomicU64,
    on_batch: &mut dyn FnMut(Vec<PathBuf>),
) -> Result<usize, ScanError> {
    check_scan_controls(cancellation, deadline)?;
    if !path.exists() {
        return Err(ScanError::Failed(format!(
            "Source path does not exist: {}",
            path.display()
        )));
    }

    let mut files: Vec<PathBuf> = Vec::with_capacity(STREAM_BATCH_SIZE);
    let mut skipped_unreadable = 0_usize;

    if path.is_file() {
        check_scan_controls(cancellation, deadline)?;
        progress.fetch_add(1, Ordering::Relaxed);
        files.push(path.to_path_buf());
    } else {
        let mut entries = WalkDir::new(path).into_iter();
        loop {
            check_scan_controls(cancellation, deadline)?;
            let Some(entry) = entries.next() else {
                break;
            };
            check_scan_controls(cancellation, deadline)?;
            progress.fetch_add(1, Ordering::Relaxed);
            let entry = match entry {
                Ok(v) => v,
                Err(_) => {
                    skipped_unreadable += 1;
                    continue;
                }
            };
            // Same reason as the preview walk: a link pointing outside the
            // selected source must not become part of this source's manifest.
            if entry.path_is_symlink() {
                continue;
            }
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            files.push(p.to_path_buf());
            if files.len() >= STREAM_BATCH_SIZE {
                on_batch(std::mem::take(&mut files));
            }
        }
    }

    if !files.is_empty() {
        on_batch(files);
    }
    Ok(skipped_unreadable)
}

#[cfg(test)]
pub fn scan_directory(path: &Path) -> Result<ScanResult, String> {
    scan_directory_with_controls(path, None, None).map_err(|error| error.to_string())
}

/// Scan a source path, stopping before processing an entry when cancelled or
/// when `deadline` has elapsed.
///
/// `deadline` is an absolute [`Instant`]. The controls are checked before the
/// scan begins and between [`WalkDir`] entries; a filesystem call already in
/// progress cannot be interrupted by this synchronous iterator.
#[cfg(test)]
pub fn scan_directory_with_controls(
    path: &Path,
    cancellation: Option<&AtomicBool>,
    deadline: Option<Instant>,
) -> Result<ScanResult, ScanError> {
    check_scan_controls(cancellation, deadline)?;
    if !path.exists() {
        return Err(ScanError::Failed(format!(
            "Source path does not exist: {}",
            path.display()
        )));
    }

    let exts = supported_extensions();
    let mut files = Vec::new();
    let mut total_size_bytes = 0_u64;
    let mut photo_count = 0_usize;
    let mut video_count = 0_usize;
    let mut skipped_unreadable = 0_usize;

    if path.is_file() {
        check_scan_controls(cancellation, deadline)?;
        let ext = path
            .extension()
            .map(|v| format!(".{}", v.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        if exts.contains(ext.as_str()) {
            let meta = fs::metadata(path)
                .map_err(|e| ScanError::Failed(format!("Could not read file metadata: {e}")))?;
            let is_video = is_video_ext(ext.as_str());
            if is_video {
                video_count += 1;
            } else {
                photo_count += 1;
            }
            total_size_bytes += meta.len();
            files.push(MediaFile {
                path: path.to_string_lossy().to_string(),
                name: path
                    .file_name()
                    .map(|v| v.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string()),
                extension: ext,
                size_bytes: meta.len(),
                is_video,
            });
        }
    } else {
        let mut entries = WalkDir::new(path).into_iter();
        loop {
            check_scan_controls(cancellation, deadline)?;
            let Some(entry) = entries.next() else {
                break;
            };
            check_scan_controls(cancellation, deadline)?;
            let entry = match entry {
                Ok(v) => v,
                Err(_) => {
                    skipped_unreadable += 1;
                    continue;
                }
            };
            // Don't follow symlinks discovered inside the tree: a link pointing
            // outside the selected source could otherwise be scanned (and later
            // staged/uploaded), leaking files from outside the chosen folder.
            if entry.path_is_symlink() {
                continue;
            }
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .map(|v| format!(".{}", v.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            if !exts.contains(ext.as_str()) {
                continue;
            }
            let meta = match fs::metadata(p) {
                Ok(v) => v,
                Err(_) => {
                    skipped_unreadable += 1;
                    continue;
                }
            };
            let is_video = is_video_ext(ext.as_str());
            if is_video {
                video_count += 1;
            } else {
                photo_count += 1;
            }
            total_size_bytes += meta.len();
            files.push(MediaFile {
                path: p.to_string_lossy().to_string(),
                name: p
                    .file_name()
                    .map(|v| v.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.display().to_string()),
                extension: ext,
                size_bytes: meta.len(),
                is_video,
            });
        }
    }

    Ok(ScanResult {
        files,
        total_size_bytes,
        photo_count,
        video_count,
        skipped_unreadable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn counts_photos_and_videos_by_extension() {
        let tmp = std::env::temp_dir().join(format!("scan-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("a.jpg"), b"a").unwrap();
        fs::write(tmp.join("b.mp4"), b"b").unwrap();
        fs::write(tmp.join("c.txt"), b"c").unwrap(); // unsupported, ignored

        let result = scan_directory(&tmp).unwrap();
        assert_eq!(result.photo_count, 1);
        assert_eq!(result.video_count, 1);
        assert_eq!(result.files.len(), 2);

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn a_hung_walk_fails_a_second_claim_after_the_grace_period() {
        use std::{sync::mpsc, thread};

        let root = format!("/unresponsive-source-{}", Uuid::new_v4());
        let (release_sender, release_receiver) = mpsc::sync_channel(0);
        let guard = acquire_scan_roots(ScanPurpose::Scan, std::slice::from_ref(&root))
            .expect("first walk claims root");
        let worker = thread::spawn(move || {
            let _guard = guard;
            release_receiver.recv().expect("test releases blocked walk");
        });

        let started = Instant::now();
        let second = acquire_scan_roots(ScanPurpose::Scan, std::slice::from_ref(&root))
            .expect_err("a walk that never returns must fail the next claim");
        assert!(second.contains(&root));
        // The claim waited rather than failing on contact, so a walk that is
        // merely finishing is not reported as unresponsive.
        assert!(started.elapsed() >= CLAIM_GRACE);

        release_sender.send(()).expect("blocked walk is released");
        worker.join().expect("walking task exits");
        drop(
            acquire_scan_roots(ScanPurpose::Scan, std::slice::from_ref(&root))
                .expect("the guard clears on the walking task"),
        );
    }

    #[test]
    fn a_released_walk_admits_the_next_claim_without_waiting_out_the_grace() {
        use std::{thread, time::Duration};

        let root = format!("/slow-but-alive-{}", Uuid::new_v4());
        let guard = acquire_scan_roots(ScanPurpose::Scan, std::slice::from_ref(&root))
            .expect("first walk claims root");
        // Models the real sequence: the caller cancels the previous walk, which
        // then returns at its next entry while the new claim is already waiting.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            drop(guard);
        });

        let started = Instant::now();
        let second = acquire_scan_roots(ScanPurpose::Scan, std::slice::from_ref(&root))
            .expect("a walk that returns must not fail the next claim");
        assert!(started.elapsed() < CLAIM_GRACE);
        drop(second);
    }

    #[test]
    fn a_forecast_and_a_scan_claim_the_same_root_together() {
        let root = format!("/shared-source-{}", Uuid::new_v4());
        // The grid scan and the preflight forecast walk the same source for
        // different answers; neither may report the other as unresponsive.
        let scan = acquire_scan_roots(ScanPurpose::Scan, std::slice::from_ref(&root))
            .expect("the scan claims the root");
        let forecast = acquire_scan_roots(ScanPurpose::Forecast, std::slice::from_ref(&root))
            .expect("the forecast claims the same root in its own namespace");
        drop((scan, forecast));
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinks_pointing_outside_the_tree() {
        let tmp = std::env::temp_dir().join(format!("scan-link-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let outside = std::env::temp_dir().join(format!("secret-{}.jpg", Uuid::new_v4()));
        fs::write(&outside, b"secret").unwrap();
        fs::write(tmp.join("real.jpg"), b"real").unwrap();
        std::os::unix::fs::symlink(&outside, tmp.join("link.jpg")).unwrap();

        let result = scan_directory(&tmp).unwrap();
        // Only the real file is scanned; the escaping symlink is skipped.
        assert_eq!(result.files.len(), 1);
        assert!(result.files.iter().all(|f| f.name == "real.jpg"));

        fs::remove_dir_all(&tmp).unwrap();
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn nonexistent_path_returns_err() {
        let missing = std::env::temp_dir().join(format!("scan-missing-{}", Uuid::new_v4()));
        assert!(scan_directory(&missing).is_err());
    }

    #[test]
    fn scan_controls_return_typed_cancellation_errors() {
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            scan_directory_with_controls(std::path::Path::new("/not-used"), Some(&cancelled), None),
            Err(ScanError::Cancelled)
        ));

        assert!(matches!(
            scan_directory_with_controls(
                std::path::Path::new("/not-used"),
                None,
                Some(Instant::now())
            ),
            Err(ScanError::TimedOut)
        ));
    }

    #[test]
    fn single_supported_file_returns_one_photo() {
        let tmp = std::env::temp_dir().join(format!("scan-one-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let photo = tmp.join("shot.JPG"); // uppercase ext must normalize to .jpg
        fs::write(&photo, b"hello").unwrap();

        let result = scan_directory(&photo).unwrap();
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.photo_count, 1);
        assert_eq!(result.video_count, 0);
        assert_eq!(result.total_size_bytes, 5);
        assert!(!result.files[0].is_video);
        assert_eq!(result.files[0].extension, ".jpg");

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn single_video_file_is_flagged_video() {
        let tmp = std::env::temp_dir().join(format!("scan-vid-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let clip = tmp.join("clip.mov");
        fs::write(&clip, b"vid").unwrap();

        let result = scan_directory(&clip).unwrap();
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.video_count, 1);
        assert_eq!(result.photo_count, 0);
        assert!(result.files[0].is_video);

        fs::remove_dir_all(&tmp).unwrap();
    }
    #[test]
    fn streaming_scan_honors_cancellation_between_entries() {
        let tmp = std::env::temp_dir().join(format!("scan-cancel-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        for index in 0..=STREAM_BATCH_SIZE {
            fs::write(tmp.join(format!("{index}.jpg")), b"photo").unwrap();
        }

        let cancellation = AtomicBool::new(false);
        let mut batches = 0;
        let result = scan_directory_streaming(&tmp, Some(&cancellation), None, &mut |batch| {
            batches += 1;
            assert_eq!(batch.len(), STREAM_BATCH_SIZE);
            cancellation.store(true, Ordering::Relaxed);
        });

        assert!(matches!(result, Err(ScanError::Cancelled)));
        assert_eq!(batches, 1);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn streaming_scan_flushes_all_batches_and_counts_unreadable_entries() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = std::env::temp_dir().join(format!("scan-stream-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        for index in 0..(STREAM_BATCH_SIZE + 1) {
            fs::write(tmp.join(format!("{index}.jpg")), b"photo").unwrap();
        }

        let unreadable = tmp.join("unreadable");
        fs::create_dir(&unreadable).unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

        let mut batches = Vec::new();
        let skipped = scan_directory_streaming(&tmp, None, None, &mut |batch| {
            batches.push(batch);
        })
        .unwrap();

        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&tmp).unwrap();

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), STREAM_BATCH_SIZE);
        assert_eq!(batches[1].len(), 1);
        assert_eq!(batches.into_iter().flatten().count(), STREAM_BATCH_SIZE + 1);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn single_unsupported_file_returns_empty() {
        let tmp = std::env::temp_dir().join(format!("scan-uns-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let doc = tmp.join("notes.txt");
        fs::write(&doc, b"x").unwrap();

        let result = scan_directory(&doc).unwrap();
        assert!(result.files.is_empty());
        assert_eq!(result.photo_count, 0);
        assert_eq!(result.video_count, 0);

        fs::remove_dir_all(&tmp).unwrap();
    }
    // A selected media file streams to the preview, while a selected document stays hidden.
    #[test]
    fn streaming_scan_handles_single_media_and_non_media_files() {
        let tmp = std::env::temp_dir().join(format!("scan-stream-single-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let video = tmp.join("clip.MOV");
        let document = tmp.join("notes.txt");
        fs::write(&video, b"video").unwrap();
        fs::write(&document, b"document").unwrap();

        let mut media_batches = Vec::new();
        let skipped = scan_directory_streaming(&video, None, None, &mut |batch| {
            media_batches.extend(batch);
        })
        .unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(media_batches.len(), 1);
        assert_eq!(media_batches[0].name, "clip.MOV");
        assert_eq!(media_batches[0].extension, ".mov");
        assert!(media_batches[0].is_video);

        let mut non_media_batches = Vec::new();
        let skipped = scan_directory_streaming(&document, None, None, &mut |batch| {
            non_media_batches.extend(batch);
        })
        .unwrap();
        assert_eq!(skipped, 0);
        assert!(non_media_batches.is_empty());

        fs::remove_dir_all(&tmp).unwrap();
    }

    /// The manifest walk keeps the preview walk's controls, and reports
    /// progress: the import worker bounds this walk by SILENCE, so a counter
    /// that never moved would declare a healthy but slow card unresponsive.
    #[test]
    fn the_manifest_walk_reports_progress_and_still_obeys_its_controls() {
        let tmp = std::env::temp_dir().join(format!("manifest-controls-{}", Uuid::new_v4()));
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::write(tmp.join("clip.mts"), b"clip").unwrap();
        fs::write(tmp.join("sub/notes.txt"), b"notes").unwrap();

        let progress = AtomicU64::new(0);
        let mut seen: Vec<PathBuf> = Vec::new();
        let skipped = manifest_directory_streaming(&tmp, None, None, &progress, &mut |batch| {
            seen.extend(batch)
        })
        .expect("a readable source enumerates");
        assert_eq!(skipped, 0);
        assert!(seen.contains(&tmp.join("clip.mts")));
        assert!(
            seen.contains(&tmp.join("sub/notes.txt")),
            "the manifest is every file that existed, not every file the grid shows"
        );
        assert!(
            progress.load(Ordering::Relaxed) >= seen.len() as u64,
            "every examined entry must move the counter the caller's bound reads"
        );

        let cancelled = AtomicBool::new(true);
        assert_eq!(
            manifest_directory_streaming(&tmp, Some(&cancelled), None, &progress, &mut |_| {}),
            Err(ScanError::Cancelled)
        );
        assert_eq!(
            manifest_directory_streaming(&tmp, None, Some(Instant::now()), &progress, &mut |_| {}),
            Err(ScanError::TimedOut)
        );

        fs::remove_dir_all(&tmp).unwrap();
    }
}
