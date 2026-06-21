use std::{collections::HashSet, fs, path::Path};

#[derive(Debug, Clone)]
pub struct WipeResult {
    pub deleted: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

fn allowed_media_exts() -> HashSet<&'static str> {
    [
        ".jpg", ".jpeg", ".png", ".heic", ".heif", ".avif", ".tiff", ".tif", ".gif", ".bmp",
        ".webp", ".raw", ".dng", ".cr2", ".cr3", ".nef", ".arw", ".orf", ".rw2", ".raf", ".mp4",
        ".mov", ".m4v", ".avi", ".mkv",
    ]
    .into_iter()
    .collect()
}

pub fn wipe_files(paths: &[String]) -> WipeResult {
    let exts = allowed_media_exts();
    let mut result = WipeResult {
        deleted: 0,
        failed: 0,
        skipped: 0,
        errors: Vec::new(),
    };

    for raw in paths {
        let path = Path::new(raw);
        if !path.exists() || !path.is_file() {
            result.skipped += 1;
            continue;
        }

        let ext = path
            .extension()
            .map(|v| format!(".{}", v.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        if !exts.contains(ext.as_str()) {
            result.skipped += 1;
            continue;
        }

        match fs::remove_file(path) {
            Ok(_) => result.deleted += 1,
            Err(err) => {
                result.failed += 1;
                result
                    .errors
                    .push(format!("Could not delete {}: {err}", path.display()));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::wipe_files;

    fn temp_file(stem: &str, ext: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "immich-shuttle-test-{stem}-{}.{}",
            std::process::id(),
            ext
        ));
        path
    }

    #[test]
    fn wipes_only_selected_media_files() {
        let photo = temp_file("photo", "jpg");
        let other = temp_file("other", "txt");
        fs::write(&photo, b"a").expect("write photo");
        fs::write(&other, b"b").expect("write text");

        let result = wipe_files(&[
            photo.to_string_lossy().to_string(),
            other.to_string_lossy().to_string(),
        ]);

        assert_eq!(result.deleted, 1);
        assert!(!photo.exists());
        assert!(other.exists());

        let _ = fs::remove_file(other);
    }

    #[test]
    fn skips_missing_files() {
        let missing = temp_file("missing", "jpg");
        let result = wipe_files(&[missing.to_string_lossy().to_string()]);
        assert_eq!(result.deleted, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn skips_non_media_file_extensions() {
        let text = temp_file("notes", "txt");
        fs::write(&text, b"x").expect("write text");
        let result = wipe_files(&[text.to_string_lossy().to_string()]);
        assert_eq!(result.deleted, 0);
        assert_eq!(result.skipped, 1);
        assert!(text.exists());
        let _ = fs::remove_file(text);
    }
}
