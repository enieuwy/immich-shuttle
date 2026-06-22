use std::{collections::HashSet, fs, path::Path};

use crate::services::immich_client::ImmichClient;

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

#[derive(Debug, Clone)]
pub struct VerifyResult {
    /// Files the server confirmed it holds (safe to delete).
    pub confirmed: Vec<String>,
    /// Files not confirmed on the server (kept for safety).
    pub unverified: Vec<String>,
}

fn file_sha1_hex(path: &str) -> Result<String, String> {
    use sha1::{Digest, Sha1};
    let mut file = fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        let read =
            std::io::Read::read(&mut file, &mut buf).map_err(|e| format!("read {path}: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Verifies which of `paths` the Immich server already holds (matched by SHA-1
/// checksum) and partitions them into `confirmed` (present on the server, safe
/// to delete) and `unverified` (missing or unreadable, kept for safety).
pub async fn verify_uploaded(
    server_url: &str,
    api_key: &str,
    paths: &[String],
) -> Result<VerifyResult, String> {
    if paths.is_empty() {
        return Ok(VerifyResult {
            confirmed: Vec::new(),
            unverified: Vec::new(),
        });
    }

    // Hashing reads files from (possibly slow) media; keep it off the async runtime.
    let owned: Vec<String> = paths.to_vec();
    let hashed: Vec<(String, Option<String>)> = tokio::task::spawn_blocking(move || {
        owned
            .into_iter()
            .map(|path| {
                let checksum = file_sha1_hex(&path).ok();
                (path, checksum)
            })
            .collect()
    })
    .await
    .map_err(|e| format!("Checksum task failed: {e}"))?;

    let mut unverified: Vec<String> = Vec::new();
    let mut to_check: Vec<(String, String)> = Vec::new();
    for (path, checksum) in hashed {
        match checksum {
            Some(sum) => to_check.push((path, sum)),
            None => unverified.push(path),
        }
    }

    let client = ImmichClient::new(server_url, api_key);
    let present = client.bulk_upload_check(&to_check).await?;

    let mut confirmed: Vec<String> = Vec::new();
    for (path, _) in to_check {
        if present.contains(&path) {
            confirmed.push(path);
        } else {
            unverified.push(path);
        }
    }

    Ok(VerifyResult {
        confirmed,
        unverified,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{file_sha1_hex, wipe_files};

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

    #[test]
    fn computes_lowercase_hex_sha1() {
        let file = temp_file("hash", "bin");
        fs::write(&file, b"hello").expect("write file");
        let hex = file_sha1_hex(file.to_str().expect("path")).expect("hash");
        let _ = fs::remove_file(&file);
        // Immich matches assets by SHA-1; this is sha1("hello").
        assert_eq!(hex, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    }
}
