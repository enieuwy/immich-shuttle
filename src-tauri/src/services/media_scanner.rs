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
