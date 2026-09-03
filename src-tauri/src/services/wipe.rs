use std::{
    fs, io,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::SystemTime,
};

use crate::services::immich_client::ImmichClient;

#[derive(Debug, Clone)]
pub struct WipeResult {
    pub deleted: usize,
    pub failed: usize,
    /// Files kept because the path no longer names an existing regular file.
    pub skipped: usize,
    /// Files kept because the file at the path is demonstrably NOT the one that
    /// was verified against the server (see `FileIdentity`).
    pub changed: usize,
    /// Files kept because their identity could no longer be *proven*: the
    /// volume was remounted, the re-stat failed, or the filesystem stopped
    /// reporting a field the check needs. Nothing says these files changed —
    /// only that the app can no longer show they did not, which is equally
    /// disqualifying but a different thing to tell the user.
    pub unprovable: usize,
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
    /// `None` when the platform/filesystem does not report a modification time.
    /// Unknown is not identical, so an absent mtime on either side fails the
    /// match: length alone cannot tell a same-size rewrite from the bytes the
    /// server confirmed.
    pub modified: Option<SystemTime>,
    /// `None` only on a platform that reports no stable file record. Same rule
    /// as `modified` — an absent record refuses the delete instead of falling
    /// back to the weaker fields.
    pub record: Option<FileRecordId>,
}

/// Identity of the file record a path resolved to, so a *replacement* file that
/// happens to share its predecessor's length and mtime is still caught.
///
/// How much this proves is platform-dependent, and the two branches are not
/// equally strong — see `file_record` on each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRecordId {
    /// The filesystem that issued `record`, so a record number is only ever
    /// compared against one from the same filesystem. Unix `st_dev`; on Windows
    /// no volume identifier is reachable on stable, so it is a constant `0` and
    /// carries no information (see `file_record`).
    pub device: u64,
    /// Unix `st_ino`: a file created at the same path gets a fresh inode, which
    /// makes a replacement provable. Windows uses the creation time, which is
    /// weaker — it corroborates, it does not prove (see `file_record`).
    pub record: u64,
}

/// Unix reports the pair that genuinely identifies a file: the filesystem that
/// issued the inode, and the inode itself. Note that `st_dev` is stable only
/// while the volume stays mounted — an eject and reinsert, or an SMB
/// reconnect, renumbers it for a file nobody touched, which is why the
/// re-check reports a device change as unproven identity rather than as a
/// changed file.
#[cfg(unix)]
fn file_record(metadata: &fs::Metadata) -> Option<FileRecordId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileRecordId {
        device: metadata.dev(),
        record: metadata.ino(),
    })
}

/// Windows has no stable accessor for the pair that would actually identify a
/// file: `volume_serial_number()`/`file_index()` sit behind the unstable
/// `windows_by_handle` feature, and reading them any other way means a
/// `GetFileInformationByHandle` FFI call on the open handle, which this
/// signature (a plain `&Metadata`) cannot reach. So the creation time stands in,
/// and it is important to be exact about what it does and does not show:
///
/// - It DOES catch a path recreated at a different time, which is the common
///   replacement (a camera writing a new file, a fresh copy).
/// - It does NOT prove the file was not replaced. NTFS *tunneling* restores the
///   original creation time when a file is deleted and recreated under the same
///   name in the same directory within a short window — exactly the case this
///   field is meant to catch. On Windows the load-bearing evidence is therefore
///   the length + modification time pair, which a recreate cannot restore
///   without being made to; the creation time only narrows the gap further.
/// - A creation time of `0` means the volume does not record one, i.e. UNKNOWN.
///   Returning it as a value would compare equal on both stats and silently
///   authorize every delete, so it yields `None` and the wipe refuses instead.
///
/// Refusing outright on Windows (always `None`) was the alternative. It is
/// rejected because it would turn the wipe into a permanent no-op there: the
/// length + mtime pair already catches every rewrite the unix branch catches,
/// and trading a working feature for the narrow tunneling window is a worse
/// deal than documenting the window.
#[cfg(windows)]
fn file_record(metadata: &fs::Metadata) -> Option<FileRecordId> {
    use std::os::windows::fs::MetadataExt;
    let creation_time = metadata.creation_time();
    if creation_time == 0 {
        return None;
    }
    Some(FileRecordId {
        device: 0,
        record: creation_time,
    })
}

/// A platform with neither accessor cannot identify the file record, and the
/// wipe refuses rather than authorizing a delete on the remaining fields.
#[cfg(not(any(unix, windows)))]
fn file_record(_metadata: &fs::Metadata) -> Option<FileRecordId> {
    None
}

impl FileIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            record: file_record(metadata),
        }
    }

    /// Classify `other` against the file this identity was taken from.
    ///
    /// Only `Same` authorizes the delete: every field must be known on both
    /// sides and equal exactly, and no subset is allowed to stand in for the
    /// rest — this is the last authorization before the file leaves its path.
    /// There is deliberately no mtime tolerance: however coarse a filesystem's
    /// timestamp granularity is, repeated stats of an untouched file return the
    /// same quantized value, so two DIFFERENT values always mean the file
    /// changed.
    ///
    /// The order matters. Length and mtime are checked first because a
    /// difference there is positive proof that the path no longer holds the
    /// verified bytes, whatever volume it now lives on. Only once those agree
    /// does a device mismatch mean "remounted" rather than "different file".
    fn check(&self, other: &Self) -> IdentityCheck {
        if self.len != other.len {
            return IdentityCheck::Changed;
        }
        let (Some(mine), Some(theirs)) = (self.modified, other.modified) else {
            return IdentityCheck::Unprovable(Unprovable::Incomplete);
        };
        if mine != theirs {
            return IdentityCheck::Changed;
        }
        let (Some(mine), Some(theirs)) = (self.record, other.record) else {
            return IdentityCheck::Unprovable(Unprovable::Incomplete);
        };
        if mine.device != theirs.device {
            // Record numbers from two different filesystems are not comparable,
            // so the inode cannot rule a replacement in or out.
            return IdentityCheck::Unprovable(Unprovable::Remounted);
        }
        if mine.record != theirs.record {
            return IdentityCheck::Changed;
        }
        IdentityCheck::Same
    }
}

/// What re-stating a verified file showed, split by what it actually proves.
/// `Changed` and `Unprovable` both keep the file; they differ in what the user
/// is told, because "your file changed" is a lie when the truth is "the card
/// was reconnected and I can no longer prove anything about it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityCheck {
    /// Same, untouched file: the delete is authorized.
    Same,
    /// The path demonstrably no longer holds the verified file.
    Changed,
    /// Nothing shows the file changed, and nothing shows it did not.
    Unprovable(Unprovable),
}

/// Why identity could not be proven either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unprovable {
    /// Length and mtime still agree, but the record came from a different
    /// filesystem than the one verified: the volume was remounted or
    /// reconnected. Extremely likely to be the same file; not provably so.
    Remounted,
    /// A field the check needs was never captured, on one side or the other.
    Incomplete,
}

impl Unprovable {
    /// The user-facing reason, phrased to say what happened and what to do
    /// about it. "The file changed" would be a false statement here: the file
    /// is kept because the app lost the ability to prove anything about it, not
    /// because it observed a change.
    fn explain(self) -> &'static str {
        match self {
            Self::Remounted => {
                "its volume was remounted or reconnected since it was verified, so it can no \
                 longer be proven to be the same file. Nothing was deleted — run the verify step \
                 again now that the volume is back to re-check it."
            }
            Self::Incomplete => {
                "the filesystem no longer reports the details (modification time, file record) \
                 needed to prove it is the verified file. Nothing was deleted — run the verify \
                 step again to re-check it."
            }
        }
    }
}

/// A file the server confirmed it holds, paired with the checksum the server
/// answered for and the identity that was hashed. Deletion is conditional on
/// the bytes at the path STILL hashing to `checksum`, with the identity as a
/// cheap first filter.
#[derive(Debug, Clone)]
pub struct VerifiedFile {
    pub path: String,
    pub identity: FileIdentity,
    /// The SHA-1 the server confirmed. Re-computed immediately before the
    /// delete: it is the only field that proves the CONTENTS, rather than
    /// proving that a path still resolves to the same filesystem record.
    pub checksum: String,
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
/// There is intentionally no extension gate: the verified-confirmed set is the
/// authority, and `retain_paths_under_sources` already enforced containment
/// upstream. This prevents an allowlist from silently stranding media the
/// server already holds and reporting the upload as skipped.
///
/// Every file is re-stat'd and then re-hashed immediately before deletion, and
/// is kept unless the bytes at the path still hash to the checksum the server
/// confirmed. The stat is the cheap filter — a length, mtime, or record change
/// is positive proof without reading the file — and the hash is the
/// authorization: metadata identity alone can be forged by a same-length,
/// same-mtime replacement that reuses the inode, which ext4 hands out again
/// for a file recreated in the same directory. Only the content proof makes
/// the delete safe on every filesystem, and it is safe by definition: the
/// server holds bytes with exactly that checksum.
///
/// A file that is kept is reported as `changed` only when the re-check PROVES
/// it differs; when identity merely became unprovable (a remount, an
/// unreadable stat, a read that failed) it is reported as `unprovable`,
/// because telling the user their files changed when they only reconnected the
/// card sends them looking for a problem that is not there.
pub fn wipe_files(files: &[VerifiedFile]) -> WipeResult {
    let trash = trash_context();
    let mut result = WipeResult {
        deleted: 0,
        failed: 0,
        skipped: 0,
        changed: 0,
        unprovable: 0,
        failed_paths: Vec::new(),
        errors: Vec::new(),
    };

    for file in files {
        let path = Path::new(&file.path);
        if !path.exists() || !path.is_file() {
            result.skipped += 1;
            continue;
        }

        // Both re-checks sit immediately before the delete, so the window a
        // concurrent writer could slip into is the loop body.
        let outcome = match fs::metadata(path) {
            Ok(metadata) => file.identity.check(&FileIdentity::of(&metadata)),
            // A stat that fails says nothing about the file's contents, so this
            // is lost proof rather than an observed change. Same outcome: keep.
            Err(err) => {
                result.unprovable += 1;
                result.errors.push(format!(
                    "Kept {}: could not re-check the file before deleting it ({err}). Nothing was \
                     deleted — run the verify step again to re-check it.",
                    path.display()
                ));
                continue;
            }
        };
        match outcome {
            IdentityCheck::Same => {}
            IdentityCheck::Changed => {
                result.changed += 1;
                result.errors.push(format!(
                    "Kept {}: the file changed after it was verified on the server.",
                    path.display()
                ));
                continue;
            }
            IdentityCheck::Unprovable(reason) => {
                result.unprovable += 1;
                result
                    .errors
                    .push(format!("Kept {}: {}", path.display(), reason.explain()));
                continue;
            }
        }

        // The authorization. `hash_file` reads and stats through ONE handle, so
        // a replacement swapped in between this read and the stat cannot look
        // like the verified file. A checksum that still matches makes the
        // delete safe whatever the metadata says: the server holds these exact
        // bytes.
        match hash_file(&file.path) {
            Ok((checksum, _)) if checksum == file.checksum => {}
            Ok(_) => {
                result.changed += 1;
                result.errors.push(format!(
                    "Kept {}: the file changed after it was verified on the server.",
                    path.display()
                ));
                continue;
            }
            // A read that fails proves nothing about the contents, so this is
            // lost proof rather than an observed change. Same outcome: keep.
            Err(err) => {
                result.unprovable += 1;
                result.errors.push(format!(
                    "Kept {}: could not re-read the file to prove it is the verified one \
                     ({err}). Nothing was deleted — run the verify step again to re-check it.",
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
    // Only explicitly-live matches authorize a delete; an absent/unknown
    // `isTrashed` is not a confirmation.
    let present = client.bulk_upload_check(&checksums).await?.confirmed_live;

    let mut confirmed: Vec<VerifiedFile> = Vec::new();
    for (index, ((path, checksum), identity)) in to_check.into_iter().zip(identities).enumerate() {
        if present.contains(&index) {
            confirmed.push(VerifiedFile {
                path,
                identity,
                checksum,
            });
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
    // The forecast only counts what an upload would skip, so a match the server
    // did not explicitly mark trashed is enough here.
    let present = client.bulk_upload_check(&checksums).await?.duplicates;

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
        hash_file, hash_forecast_files, partition_present, wipe_files, FileIdentity, FileRecordId,
        IdentityCheck, Unprovable, VerifiedFile,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, SystemTime},
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
    /// with the checksum the server answered for and the identity captured at
    /// hash time.
    fn verified(path: &Path) -> VerifiedFile {
        let (checksum, identity) = hash_file(path.to_str().expect("path")).expect("hash");
        VerifiedFile {
            path: path.to_string_lossy().to_string(),
            identity,
            checksum,
        }
    }

    /// A file whose recorded identity cannot match anything on disk. The
    /// checksum is a placeholder: this helper is used where the identity gate
    /// (or a missing path) decides the outcome before any byte is read, and it
    /// must work for a path that does not exist.
    fn verified_with_stale_identity(path: &Path) -> VerifiedFile {
        VerifiedFile {
            path: path.to_string_lossy().to_string(),
            identity: FileIdentity {
                len: 999_999,
                modified: None,
                record: None,
            },
            checksum: "0".repeat(40),
        }
    }

    /// Pins a file's mtime so a test drifts it by an exact amount instead of
    /// depending on what the clock did between two writes.
    fn set_mtime(path: &Path, mtime: SystemTime) {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open to set mtime")
            .set_modified(mtime)
            .expect("set mtime");
    }

    #[test]
    fn moves_only_selected_media_files_to_trash() {
        let photo = temp_file("photo", "jpg");
        let other = temp_file("other", "png");
        fs::write(&photo, b"a").expect("write photo");
        fs::write(&other, b"b").expect("write other media");

        let result = wipe_files(&[verified(&photo), verified(&other)]);
        // trash::delete moves the file to the OS Trash: it leaves the origin
        // path (counted as deleted) but, unlike a hard delete, stays recoverable.

        assert_eq!(result.deleted, 2);
        assert!(
            result.failed_paths.is_empty(),
            "successful files must not enter the failed retry set"
        );
        assert!(!photo.exists());
        assert!(!other.exists());
    }

    #[test]
    fn skips_missing_files() {
        let missing = temp_file("missing", "jpg");
        let result = wipe_files(&[verified_with_stale_identity(&missing)]);
        assert_eq!(result.deleted, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn deletes_server_confirmed_file_with_unlisted_extension() {
        let media = temp_file("transport-stream", "mts");
        fs::write(&media, b"video").expect("write media");

        let result = wipe_files(&[verified(&media)]);

        assert_eq!(result.deleted, 1);
        assert_eq!(result.skipped, 0);
        assert!(!media.exists());
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

    /// Two different mtimes always mean the file changed: repeated stats of an
    /// untouched file return the same quantized value however coarse the
    /// filesystem's granularity is, so there is no rounding to forgive. This
    /// used to forgive up to a second of drift, which let a same-length rewrite
    /// pass as identical.
    ///
    /// The classification is asserted, not just the boolean: only `Same`
    /// authorizes a delete, and everything else has to land on the right side
    /// of "proven different" vs. "no longer provable".
    #[test]
    fn sub_second_mtime_drift_is_treated_as_a_change() {
        let record = Some(FileRecordId {
            device: 1,
            record: 2,
        });
        let base = FileIdentity {
            len: 10,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_500)),
            record,
        };
        let drifted = FileIdentity {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            ..base.clone()
        };
        assert_eq!(
            base.check(&drifted),
            IdentityCheck::Changed,
            "half a second of drift is a rewrite"
        );
        assert_eq!(drifted.check(&base), IdentityCheck::Changed);
        assert_eq!(
            base.check(&base.clone()),
            IdentityCheck::Same,
            "an identical stat still authorizes the delete"
        );

        let same_time_new_length = FileIdentity {
            len: 11,
            ..base.clone()
        };
        assert_eq!(
            base.check(&same_time_new_length),
            IdentityCheck::Changed,
            "a length change is a rewrite regardless of mtime"
        );

        let unknown_mtime = FileIdentity {
            modified: None,
            ..base.clone()
        };
        assert_eq!(
            base.check(&unknown_mtime),
            IdentityCheck::Unprovable(Unprovable::Incomplete),
            "an unknown mtime is not an identical one, and proves nothing either"
        );
        assert_eq!(
            unknown_mtime.check(&base),
            IdentityCheck::Unprovable(Unprovable::Incomplete)
        );
        assert_eq!(
            unknown_mtime.check(&unknown_mtime.clone()),
            IdentityCheck::Unprovable(Unprovable::Incomplete),
            "two unknown mtimes leave length alone, which must never authorize a delete"
        );

        let unknown_record = FileIdentity {
            record: None,
            ..base.clone()
        };
        assert_eq!(
            base.check(&unknown_record),
            IdentityCheck::Unprovable(Unprovable::Incomplete),
            "a file record that could not be captured must not weaken the check"
        );
        assert_eq!(
            unknown_record.check(&base),
            IdentityCheck::Unprovable(Unprovable::Incomplete)
        );

        let other_record = FileIdentity {
            record: Some(FileRecordId {
                device: 1,
                record: 3,
            }),
            ..base.clone()
        };
        assert_eq!(
            base.check(&other_record),
            IdentityCheck::Changed,
            "a new file record at the same path on the same filesystem is a replacement"
        );

        let other_device = FileIdentity {
            record: Some(FileRecordId {
                device: 2,
                record: 2,
            }),
            ..base.clone()
        };
        assert_eq!(
            base.check(&other_device),
            IdentityCheck::Unprovable(Unprovable::Remounted),
            "a record from a different filesystem is not comparable, so nothing is proven"
        );
    }

    /// The rewrite the old one-second tolerance waved through: same length, a
    /// different SHA-1, and an mtime only half a second later.
    #[test]
    fn keeps_a_same_length_rewrite_whose_mtime_moved_half_a_second() {
        let path = temp_file("half-second-rewrite", "jpg");
        fs::write(&path, b"original").expect("write original");
        let file = verified(&path);
        let hashed_at = file.identity.modified.expect("mtime captured at hash time");

        // Identically sized, different bytes: only the mtime can reveal it.
        fs::write(&path, b"replaced").expect("rewrite");
        set_mtime(&path, hashed_at + Duration::from_millis(500));

        let result = wipe_files(&[file]);

        assert_eq!(result.changed, 1, "the rewrite must be reported as changed");
        assert_eq!(result.deleted, 0);
        assert!(
            path.exists(),
            "bytes the server never received must stay on disk"
        );

        let _ = fs::remove_file(&path);
    }

    /// A path that was unlinked and recreated is not the verified file, even
    /// when the replacement matches the verified length and its mtime is
    /// restored. The test states no premise about the inode: whether a
    /// recreated file gets a fresh record is the filesystem's choice — APFS
    /// hands out a new one, ext4 hands the old one straight back — so the
    /// authorization cannot rest on it.
    #[test]
    fn keeps_a_replacement_file_with_the_same_length_and_mtime() {
        let path = temp_file("replaced-record", "jpg");
        fs::write(&path, b"original").expect("write original");
        let file = verified(&path);
        let hashed_at = file.identity.modified.expect("mtime captured at hash time");

        fs::remove_file(&path).expect("unlink the verified file");
        fs::write(&path, b"replaced").expect("write the replacement");
        set_mtime(&path, hashed_at);

        let replacement = fs::metadata(&path).expect("stat the replacement");
        assert_eq!(
            file.identity.modified,
            replacement.modified().ok(),
            "test premise: length and mtime are indistinguishable"
        );

        let result = wipe_files(&[file]);

        assert_eq!(result.changed, 1, "a replacement is not the verified file");
        assert_eq!(result.deleted, 0);
        assert!(path.exists(), "the replacement must stay on disk");

        let _ = fs::remove_file(&path);
    }

    /// The case the metadata check cannot see, pinned without depending on the
    /// filesystem to reuse an inode: the recorded identity is taken from the
    /// REPLACEMENT, so length, mtime, and file record all match exactly and
    /// only the checksum is still the original's. This is what ext4 produces
    /// for a same-size, mtime-restored recreate, and it must not delete.
    #[test]
    fn keeps_a_replacement_whose_whole_identity_matches_the_verified_file() {
        let path = temp_file("record-reused", "jpg");
        fs::write(&path, b"original").expect("write original");
        let original = verified(&path);

        fs::write(&path, b"replaced").expect("rewrite with the same length");
        let mut file = verified(&path);
        file.checksum = original.checksum.clone();
        assert_eq!(
            file.identity,
            FileIdentity::of(&fs::metadata(&path).expect("stat")),
            "test premise: every metadata field the check reads matches"
        );
        assert_ne!(
            file.checksum,
            hash_file(path.to_str().expect("path")).expect("hash").0,
            "test premise: the bytes are not the ones the server confirmed"
        );

        let result = wipe_files(&[file]);

        assert_eq!(
            result.changed, 1,
            "matching metadata is not proof of matching contents"
        );
        assert_eq!(result.deleted, 0);
        assert!(path.exists());

        let _ = fs::remove_file(&path);
    }

    /// Length alone is not authorization: without a verified mtime the file is
    /// kept even though it is otherwise untouched. It is kept as unprovable,
    /// not as changed — a missing timestamp is missing evidence, not evidence.
    #[test]
    fn keeps_a_file_whose_verified_identity_has_no_mtime() {
        let path = temp_file("unknown-mtime", "jpg");
        fs::write(&path, b"bytes").expect("write file");
        let mut file = verified(&path);
        file.identity.modified = None;

        let result = wipe_files(&[file]);

        assert_eq!(result.unprovable, 1);
        assert_eq!(result.changed, 0);
        assert_eq!(result.deleted, 0);
        assert!(
            path.exists(),
            "an unreadable mtime must not authorize a delete"
        );

        let _ = fs::remove_file(&path);
    }

    /// The Windows record is the file's creation time, and `0` there means the
    /// volume does not record one — unknown, not a value. Encoding unknown as
    /// `0` would compare equal on both stats and wave every delete through, so
    /// `file_record` yields `None` and the delete is refused instead.
    #[test]
    fn refuses_to_delete_when_the_file_record_is_absent() {
        let path = temp_file("absent-record", "jpg");
        fs::write(&path, b"bytes").expect("write file");
        let mut file = verified(&path);
        // Exactly what a zero Windows creation time produces.
        file.identity.record = None;

        let on_disk = FileIdentity::of(&fs::metadata(&path).expect("stat the file"));
        assert_eq!(
            file.identity.check(&on_disk),
            IdentityCheck::Unprovable(Unprovable::Incomplete),
            "an absent record cannot prove this is the verified file"
        );

        let result = wipe_files(&[file]);

        assert_eq!(result.unprovable, 1);
        assert_eq!(
            result.changed, 0,
            "an unknown record is not evidence of a change"
        );
        assert_eq!(result.deleted, 0);
        assert!(
            path.exists(),
            "an unknown file record must never authorize a delete"
        );

        let _ = fs::remove_file(&path);
    }

    /// A card ejected and reinserted, or an SMB share that reconnected, gives
    /// an UNTOUCHED file a new device id. Refusing the delete is right, but
    /// reporting it as a changed file sends the user hunting a problem that
    /// does not exist: it is reported as unprovable, and the message names the
    /// remount and the way out.
    #[test]
    fn reports_a_remount_as_unprovable_rather_than_as_a_changed_file() {
        let path = temp_file("remounted-volume", "jpg");
        fs::write(&path, b"bytes").expect("write file");
        let mut file = verified(&path);
        let verified_record = file
            .identity
            .record
            .expect("this platform reports a file record");
        // Same length, same mtime, same record number: only the volume moved.
        file.identity.record = Some(FileRecordId {
            device: verified_record.device.wrapping_add(1),
            ..verified_record
        });

        let result = wipe_files(&[file]);

        assert_eq!(
            result.unprovable, 1,
            "a remount leaves identity unproven, not disproven"
        );
        assert_eq!(
            result.changed, 0,
            "nothing observed says the file itself changed"
        );
        assert_eq!(result.deleted, 0);
        assert!(
            path.exists(),
            "an unprovable identity must not authorize a delete"
        );
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("remounted") && e.contains("verify")),
            "the message must name the remount and what to do next: {:?}",
            result.errors
        );

        let _ = fs::remove_file(&path);
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
