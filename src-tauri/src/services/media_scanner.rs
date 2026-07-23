use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use walkdir::WalkDir;

use crate::models::media::MediaFile;
#[cfg(test)]
use crate::models::media::ScanResult;

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

fn is_video_ext(ext: &str) -> bool {
    matches!(ext, ".mp4" | ".mov" | ".m4v" | ".avi" | ".mkv")
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
}
