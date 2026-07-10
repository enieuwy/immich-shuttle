use std::{collections::HashSet, fs, path::Path};

use walkdir::WalkDir;

use crate::models::media::{MediaFile, ScanResult};

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

pub fn scan_directory(path: &Path) -> Result<ScanResult, String> {
    if !path.exists() {
        return Err(format!("Source path does not exist: {}", path.display()));
    }

    let exts = supported_extensions();
    let mut files = Vec::new();
    let mut total_size_bytes = 0_u64;
    let mut photo_count = 0_usize;
    let mut video_count = 0_usize;
    let mut skipped_unreadable = 0_usize;

    if path.is_file() {
        let ext = path
            .extension()
            .map(|v| format!(".{}", v.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        if exts.contains(ext.as_str()) {
            let meta =
                fs::metadata(path).map_err(|e| format!("Could not read file metadata: {e}"))?;
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
        for entry in WalkDir::new(path) {
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
}
