use std::{
    collections::HashSet,
    fs, io,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

use crate::services::immich_client::ImmichClient;

#[derive(Debug, Clone)]
pub struct WipeResult {
    pub deleted: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Files kept because they no longer matched the identity that was verified
    /// against the server (see `FileIdentity`).
    pub changed: usize,
    /// Paths whose verified files could not be moved to the Trash and may be retried.
    pub failed_paths: Vec<String>,
    pub errors: Vec<String>,
}

/// Filesystem identity of a source file, captured at the moment its contents
/// were hashed for the server existence check.
///
/// Verification and deletion are separated by a full SHA-1 pass over every file
/// plus a server round trip — minutes for a large video batch on slow removable
/// media. Anything writing to the card in that window (a camera sync, an editor
/// autosave, a still-finishing copy) leaves the server holding the OLD contents
/// while the path now points at NEW, unuploaded bytes. Deleting then destroys
/// data the server never received, so the identity observed at hash time is
/// carried through to the delete and re-checked there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    pub len: u64,
    /// `None` when the platform/filesystem does not report a modification time;
    /// length alone then carries the check.
    pub modified: Option<SystemTime>,
}

impl FileIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }

    /// Whether `other` is the same file contents this identity was taken from.
    ///
    /// mtime is compared with a one-second tolerance: some filesystems (FAT32 on
    /// camera cards, SMB shares) store second- or two-second-granularity
    /// timestamps, and a stat that rounds differently than the one taken at hash
    /// time must not be read as a rewrite.
    fn matches(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        match (self.modified, other.modified) {
            (Some(a), Some(b)) => a
                .duration_since(b)
                .or_else(|_| b.duration_since(a))
                .is_ok_and(|drift| drift <= Duration::from_secs(1)),
            // An unreadable mtime on either side leaves length as the only
            // signal; do not treat that as a mismatch and keep everything.
            _ => true,
        }
    }
}

/// A file the server confirmed it holds, paired with the identity that was
/// hashed. Deletion is conditional on the identity still matching.
#[derive(Debug, Clone)]
pub struct VerifiedFile {
    pub path: String,
    pub identity: FileIdentity,
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

/// A trash handle configured to avoid extra OS permission prompts. On macOS the
/// crate's default backend drives Finder via AppleScript (needs automation
/// permission and fails in headless sessions), so files that verified as
/// uploaded would be wrongly kept; NSFileManager needs no such permission.
#[cfg(target_os = "macos")]
fn trash_context() -> trash::TrashContext {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx
}

#[cfg(not(target_os = "macos"))]
fn trash_context() -> trash::TrashContext {
    trash::TrashContext::default()
}

/// Move server-confirmed originals to the Trash.
///
/// Every file is re-stat'd immediately before deletion and kept if its identity
/// no longer matches what was hashed for the server check — the stat and the
/// delete are deliberately adjacent so the window a concurrent writer could slip
/// into is as small as the loop body.
pub fn wipe_files(files: &[VerifiedFile]) -> WipeResult {
    let exts = allowed_media_exts();
    let trash = trash_context();
    let mut result = WipeResult {
        deleted: 0,
        failed: 0,
        skipped: 0,
        changed: 0,
        failed_paths: Vec::new(),
        errors: Vec::new(),
    };

    for file in files {
        let path = Path::new(&file.path);
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

        // Identity check last, so it is the most recent observation before the
        // delete. An unreadable stat here means the file changed underneath us in
        // a way we cannot reason about: keep it.
        match fs::metadata(path) {
            Ok(metadata) if file.identity.matches(&FileIdentity::of(&metadata)) => {}
            Ok(_) => {
                result.changed += 1;
                result.errors.push(format!(
                    "Kept {}: the file changed after it was verified on the server.",
                    path.display()
                ));
                continue;
            }
            Err(err) => {
                result.changed += 1;
                result.errors.push(format!(
                    "Kept {}: could not re-check the file before deleting it ({err}).",
                    path.display()
                ));
                continue;
            }
        }

        // Move to the OS Trash instead of a hard delete so a mistaken wipe is
        // recoverable. The verify-before-wipe gate upstream is unchanged: only
        // server-confirmed files reach this function.
        match trash.delete(path) {
            Ok(_) => result.deleted += 1,
            Err(err) => {
                result.failed += 1;
                result.failed_paths.push(file.path.clone());
                result
                    .errors
                    .push(format!("Could not move {} to Trash: {err}", path.display()));
            }
        }
    }

    result
}

#[derive(Debug, Clone)]
pub struct VerifyResult {
    /// Files the server confirmed it holds, with the identity that was hashed.
    pub confirmed: Vec<VerifiedFile>,
    /// Files not confirmed on the server (kept for safety).
    pub unverified: Vec<String>,
}

/// SHA-1 the file contents and capture the identity of the handle we read.
///
/// One open serves both: the identity comes from the same file handle the bytes
/// were read through (`File::metadata`), not from a second path lookup that could
/// resolve to a replacement file. It is taken AFTER the read so a write that
/// lands mid-hash is reflected in the recorded mtime rather than hidden by it.
fn hash_file(path: &str) -> Result<(String, FileIdentity), String> {
    use sha1::{Digest, Sha1};
    let mut file = fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        let read = io::Read::read(&mut file, &mut buf).map_err(|e| format!("read {path}: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let identity = file
        .metadata()
        .map(|metadata| FileIdentity::of(&metadata))
        .map_err(|e| format!("stat {path}: {e}"))?;
    let checksum = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((checksum, identity))
}

/// Verifies which of `paths` the Immich server already holds (matched by SHA-1
/// checksum) and partitions them into `confirmed` (present on the server, safe
/// to delete) and `unverified` (missing or unreadable, kept for safety).
///
/// Each confirmed entry carries the identity observed while hashing so the wipe
/// worker can refuse to delete a file that changed since.
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
    let hashed: Vec<(String, Option<(String, FileIdentity)>)> =
        tokio::task::spawn_blocking(move || {
            owned
                .into_iter()
                .map(|path| {
                    let hashed = hash_file(&path).ok();
                    (path, hashed)
                })
                .collect()
        })
        .await
        .map_err(|e| format!("Checksum task failed: {e}"))?;

    let mut unverified: Vec<String> = Vec::new();
    let mut to_check: Vec<(String, String)> = Vec::new();
    let mut identities: Vec<FileIdentity> = Vec::new();
    for (path, hashed) in hashed {
        match hashed {
            Some((sum, identity)) => {
                to_check.push((path, sum));
                identities.push(identity);
            }
            None => unverified.push(path),
        }
    }

    let client = ImmichClient::new(server_url, api_key);
    // Only the checksums leave the machine; the paths stay here and are paired
    // back up by position.
    let checksums: Vec<String> = to_check.iter().map(|(_, sum)| sum.clone()).collect();
    let present = client.bulk_upload_check(&checksums).await?;

    let mut confirmed: Vec<VerifiedFile> = Vec::new();
    for (index, ((path, _), identity)) in to_check.into_iter().zip(identities).enumerate() {
        if present.contains(&index) {
            confirmed.push(VerifiedFile { path, identity });
        } else {
            unverified.push(path);
        }
    }

    Ok(VerifyResult {
        confirmed,
        unverified,
    })
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ForecastResult {
    /// Files not on the server — these would upload.
    pub new: usize,
    /// Files the server already holds — these would be skipped.
    pub already_present: usize,
    /// Files that could not be read/hashed.
    pub unreadable: usize,
    /// The candidate set was capped; counts are a lower bound.
    pub truncated: bool,
}

/// Read-only preflight: partitions `paths` into files the server already holds
/// vs. new uploads, using the same SHA-1 + bulk-upload-check path as
/// verify-before-wipe. Safe to run repeatedly; never mutates anything.
pub async fn forecast_upload(
    server_url: &str,
    api_key: &str,
    paths: &[String],
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<ForecastResult, String> {
    if paths.is_empty() {
        return Ok(ForecastResult::default());
    }

    // Hashing reads files from (possibly slow) media; keep it off the async runtime.
    let owned: Vec<String> = paths.to_vec();
    let cancellation_for_hash = cancellation.clone();
    let hashed: Vec<(String, Option<String>)> = tokio::task::spawn_blocking(move || {
        hash_forecast_files(owned, cancellation_for_hash.as_deref())
    })
    .await
    .map_err(|e| format!("Checksum task failed: {e}"))??;

    let mut unreadable = 0usize;
    let mut to_check: Vec<(String, String)> = Vec::new();
    for (path, checksum) in hashed {
        match checksum {
            Some(sum) => to_check.push((path, sum)),
            None => unreadable += 1,
        }
    }

    if cancellation
        .as_ref()
        .is_some_and(|cancelled| cancelled.load(Ordering::Relaxed))
    {
        return Err("Forecast was cancelled".to_string());
    }

    let client = ImmichClient::new(server_url, api_key);
    let checksums: Vec<String> = to_check.iter().map(|(_, sum)| sum.clone()).collect();
    let present = client.bulk_upload_check(&checksums).await?;

    let (new, already_present) = partition_present(to_check.len(), &present);

    Ok(ForecastResult {
        new,
        already_present,
        unreadable,
        truncated: false,
    })
}

/// SHA-1s each path, stopping between files when `cancellation` is raised.
///
/// The check sits at the loop head, so a cancelled forecast abandons the rest of
/// the set instead of hashing thousands of files nobody is waiting for. A single
/// blocked read cannot be interrupted; the bound is per file, not per read.
fn hash_forecast_files(
    paths: Vec<String>,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut hashed = Vec::with_capacity(paths.len());
    for path in paths {
        if cancellation.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
            return Err("Forecast was cancelled".to_string());
        }
        // The forecast never deletes, so the identity is not needed here.
        let checksum = hash_file(&path).ok().map(|(checksum, _)| checksum);
        hashed.push((path, checksum));
    }
    Ok(hashed)
}

/// Splits `total` checked files into (new, already_present) from the positions
/// the server reported as duplicates. Positions outside the request are ignored:
/// a malformed response must not be able to inflate "already on the server",
/// which would understate what the import is about to upload and, on the wipe
/// path, overstate what is safe to delete.
fn partition_present(total: usize, present: &std::collections::HashSet<usize>) -> (usize, usize) {
    let already_present = present.iter().filter(|index| **index < total).count();
    (total.saturating_sub(already_present), already_present)
}

#[cfg(test)]
mod tests {
    use super::{
        hash_file, hash_forecast_files, partition_present, wipe_files, FileIdentity, VerifiedFile,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    fn temp_file(stem: &str, ext: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "immich-shuttle-test-{stem}-{}.{}",
            std::process::id(),
            ext
        ));
        path
    }

    /// A file as the wipe worker would receive it: verified against the server
    /// with the identity captured at hash time.
    fn verified(path: &Path) -> VerifiedFile {
        let (_, identity) = hash_file(path.to_str().expect("path")).expect("hash");
        VerifiedFile {
            path: path.to_string_lossy().to_string(),
            identity,
        }
    }

    /// A file whose recorded identity cannot match anything on disk.
    fn verified_with_stale_identity(path: &Path) -> VerifiedFile {
        VerifiedFile {
            path: path.to_string_lossy().to_string(),
            identity: FileIdentity {
                len: 999_999,
                modified: None,
            },
        }
    }

    #[test]
    fn moves_only_selected_media_files_to_trash() {
        let photo = temp_file("photo", "jpg");
        let other = temp_file("other", "txt");
        fs::write(&photo, b"a").expect("write photo");
        fs::write(&other, b"b").expect("write text");

        let result = wipe_files(&[verified(&photo), verified(&other)]);
        // trash::delete moves the file to the OS Trash: it leaves the origin
        // path (counted as deleted) but, unlike a hard delete, stays recoverable.

        assert_eq!(result.deleted, 1);
        assert!(
            result.failed_paths.is_empty(),
            "successful and skipped files must not enter the failed retry set"
        );
        assert!(!photo.exists());
        assert!(other.exists());

        let _ = fs::remove_file(other);
    }

    #[test]
    fn skips_missing_files() {
        let missing = temp_file("missing", "jpg");
        let result = wipe_files(&[verified_with_stale_identity(&missing)]);
        assert_eq!(result.deleted, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn skips_non_media_file_extensions() {
        let text = temp_file("notes", "txt");
        fs::write(&text, b"x").expect("write text");
        let result = wipe_files(&[verified(&text)]);
        assert_eq!(result.deleted, 0);
        assert_eq!(result.skipped, 1);
        assert!(text.exists());
        let _ = fs::remove_file(text);
    }

    /// The core verify-before-wipe invariant: a file rewritten between the
    /// server check and the delete holds bytes the server never received, so it
    /// must be kept even though the server confirmed the OLD contents.
    #[test]
    fn keeps_a_file_that_changed_after_verification() {
        let stable = temp_file("stable", "jpg");
        let rewritten = temp_file("rewritten", "jpg");
        fs::write(&stable, b"stable").expect("write stable");
        fs::write(&rewritten, b"original").expect("write original");

        let batch = vec![verified(&stable), verified(&rewritten)];

        // A camera sync/editor replaces the file after it was hashed and checked.
        fs::write(&rewritten, b"replaced with longer contents").expect("rewrite");

        let result = wipe_files(&batch);

        assert_eq!(result.changed, 1, "the rewritten file must be kept");
        assert!(rewritten.exists(), "changed file must survive the wipe");
        assert_eq!(result.deleted, 1, "the unchanged file still gets deleted");
        assert!(!stable.exists());
        assert!(
            result.errors.iter().any(|e| e.contains("changed after it")),
            "the user must be told why it was kept: {:?}",
            result.errors
        );

        let _ = fs::remove_file(rewritten);
    }

    /// Coarse-granularity filesystems (FAT32 cards, SMB) can report an mtime that
    /// rounds differently between two stats of an untouched file; that must not be
    /// read as a rewrite and block a legitimate wipe.
    #[test]
    fn sub_second_mtime_drift_is_not_treated_as_a_change() {
        let base = FileIdentity {
            len: 10,
            modified: Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(1_500)),
        };
        let rounded = FileIdentity {
            len: 10,
            modified: Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        };
        assert!(base.matches(&rounded));
        assert!(rounded.matches(&base));

        let two_seconds_later = FileIdentity {
            len: 10,
            modified: Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(4)),
        };
        assert!(!base.matches(&two_seconds_later));

        let same_time_new_length = FileIdentity {
            len: 11,
            modified: base.modified,
        };
        assert!(
            !base.matches(&same_time_new_length),
            "a length change is a rewrite regardless of mtime"
        );
    }

    #[test]
    fn computes_lowercase_hex_sha1() {
        let file = temp_file("hash", "bin");
        fs::write(&file, b"hello").expect("write file");
        let (hex, identity) = hash_file(file.to_str().expect("path")).expect("hash");
        let _ = fs::remove_file(&file);
        // Immich matches assets by SHA-1; this is sha1("hello").
        assert_eq!(hex, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
        assert_eq!(identity.len, 5, "identity is captured alongside the hash");
    }

    #[test]
    fn partition_present_splits_new_and_already_present() {
        // Positions 1 and 2 of a three-file check came back as duplicates.
        let present: std::collections::HashSet<usize> = [1, 2].into_iter().collect();
        let (new, already_present) = partition_present(3, &present);
        assert_eq!(new, 1);
        assert_eq!(already_present, 2);
    }

    /// A server echoing an id we never issued must not be able to shrink the
    /// "to upload" count, and on the wipe path must not add a file to the
    /// safe-to-delete set that was never confirmed.
    #[test]
    fn partition_present_ignores_positions_outside_the_request() {
        let present: std::collections::HashSet<usize> = [0, 7, 99].into_iter().collect();
        let (new, already_present) = partition_present(2, &present);
        assert_eq!(already_present, 1);
        assert_eq!(new, 1);
    }

    #[test]
    fn forecast_hashing_stops_at_the_next_file_once_cancelled() {
        let first = temp_file("forecast-first", "jpg");
        fs::write(&first, b"first").expect("write first file");
        let paths = vec![
            first.to_string_lossy().into_owned(),
            // Never reached once the flag is up, so it does not need to exist.
            "/forecast-never-hashed.jpg".to_string(),
        ];
        let cancellation = AtomicBool::new(false);

        let hashed = hash_forecast_files(paths.clone(), Some(&cancellation))
            .expect("an uncancelled forecast hashes every path");
        assert_eq!(hashed.len(), 2);
        assert!(hashed[0].1.is_some(), "the readable file is hashed");
        assert!(
            hashed[1].1.is_none(),
            "the missing file counts as unreadable"
        );

        cancellation.store(true, Ordering::Relaxed);
        assert_eq!(
            hash_forecast_files(paths, Some(&cancellation)),
            Err("Forecast was cancelled".to_string()),
            "a cancelled forecast must not hash the rest of the set"
        );

        let _ = fs::remove_file(first);
    }
}
