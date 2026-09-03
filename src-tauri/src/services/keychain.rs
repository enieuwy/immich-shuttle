use keyring::{Entry, Error as KeyringError};
use std::fmt::Display;
use std::sync::{LazyLock, Mutex, MutexGuard};

const KEYCHAIN_SERVICE: &str = "immich-shuttle";

#[cfg(target_os = "linux")]
const KEYCHAIN_RECOVERY_GUIDANCE: &str = "Install and unlock gnome-keyring or kwallet, then retry.";

#[cfg(target_os = "macos")]
const KEYCHAIN_RECOVERY_GUIDANCE: &str =
    "Unlock the macOS Keychain and allow Immich Shuttle access, then retry.";

#[cfg(target_os = "windows")]
const KEYCHAIN_RECOVERY_GUIDANCE: &str =
    "Check that you are signed in to Windows and that Credential Manager is available, then retry.";

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const KEYCHAIN_RECOVERY_GUIDANCE: &str =
    "Check that the system credential store is available and unlocked, then retry.";

fn format_keychain_error(operation: &str, backend_error: impl Display) -> String {
    format!(
        "Could not {operation}. {KEYCHAIN_RECOVERY_GUIDANCE} Original backend error: {backend_error}"
    )
}

/// Serializes all keychain access. On first access (e.g. after a code-signature
/// change invalidates the item ACL) macOS shows one prompt; concurrent reads —
/// several fire at startup (albums + users + server info) — queue behind this
/// lock instead of racing the prompt and failing. Once the user grants access
/// the queued reads proceed and succeed, so no app restart is needed.
static KEYCHAIN_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn keychain_guard() -> MutexGuard<'static, ()> {
    KEYCHAIN_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The credential backend the public functions in this module operate on.
///
/// Errors carry the raw backend text only: every public function names the
/// operation it was performing and adds the recovery guidance, so that message
/// shape stays built in exactly one place. The trait exists so the write/verify
/// /undo sequence can be exercised against a store that misbehaves on purpose —
/// no real OS keychain can be asked to lose a secret it just accepted.
trait CredentialStore {
    fn get(&self, profile_id: &str) -> Result<Option<String>, String>;
    fn set(&self, profile_id: &str, api_key: &str) -> Result<(), String>;
    fn delete(&self, profile_id: &str) -> Result<(), String>;
}

struct SystemKeychain;

impl CredentialStore for SystemKeychain {
    fn get(&self, profile_id: &str) -> Result<Option<String>, String> {
        match entry(profile_id)?.get_password() {
            Ok(value) => Ok(Some(value)),
            // No credential was ever stored for this profile — not an error.
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, profile_id: &str, api_key: &str) -> Result<(), String> {
        entry(profile_id)?
            .set_password(api_key)
            .map_err(|error| error.to_string())
    }

    fn delete(&self, profile_id: &str) -> Result<(), String> {
        match entry(profile_id)?.delete_credential() {
            Ok(()) => Ok(()),
            // Already absent — deletion is idempotent.
            Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[cfg(not(test))]
fn credential_store() -> impl CredentialStore {
    SystemKeychain
}

#[cfg(test)]
fn credential_store() -> impl CredentialStore {
    test_store::FakeStore
}

fn entry(profile_id: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, profile_id).map_err(|error| error.to_string())
}

pub fn store_api_key(profile_id: &str, api_key: &str) -> Result<(), String> {
    let _guard = keychain_guard();
    store_api_key_in(&credential_store(), profile_id, api_key)
}

/// Writes the credential and verifies it, leaving the store exactly as it was
/// found when the verification fails.
///
/// Callers read an `Err` as "the credential did not change": `profile_upsert`
/// reports the save as failed and returns through `?` before its own rollback
/// runs. So a new secret surviving a failed save would let the next import
/// authenticate with a key the user was told was not stored, and a clobbered
/// previous secret would be gone with nothing left to restore it from.
fn store_api_key_in<S: CredentialStore>(
    store: &S,
    profile_id: &str,
    api_key: &str,
) -> Result<(), String> {
    // Captured before the write: it is the only copy of what the write is
    // about to destroy.
    let previous = store
        .get(profile_id)
        .map_err(|error| format_keychain_error("read the API key from the keychain", error))?;

    store
        .set(profile_id, api_key)
        .map_err(|error| format_keychain_error("store API key in the keychain", error))?;

    // Verify the write persisted (guards against mock/no-op credential stores)
    let failure = match store.get(profile_id) {
        Ok(readback) if readback.as_deref() == Some(api_key) => return Ok(()),
        // The write itself succeeded, so this is an integrity failure in the
        // credential store, not a locked or unapproved keychain. Recovery
        // guidance about unlocking would name a cause this branch disproves.
        Ok(_) => "Keychain write succeeded but readback returned a different value. The system credential store is not storing secrets reliably."
            .to_string(),
        Err(error) => {
            format_keychain_error("verify the API key in the keychain after writing", error)
        }
    };

    Err(undo_write(store, profile_id, previous, failure))
}

/// Best-effort undo of the write [`store_api_key_in`] just made, so a save the
/// caller is told failed is not observable in the credential store. Deleting is
/// the correct undo when there was no previous credential: leaving the rejected
/// key behind would make an unsaved profile look connected.
fn undo_write<S: CredentialStore>(
    store: &S,
    profile_id: &str,
    previous: Option<String>,
    failure: String,
) -> String {
    let undo = match &previous {
        Some(api_key) => store.set(profile_id, api_key),
        None => store.delete(profile_id),
    };
    match undo {
        Ok(()) => failure,
        Err(undo_error) => {
            format!("{failure}; additionally failed to restore the previous API key: {undo_error}")
        }
    }
}

pub fn get_api_key(profile_id: &str) -> Result<Option<String>, String> {
    let _guard = keychain_guard();
    credential_store()
        .get(profile_id)
        .map_err(|error| format_keychain_error("read the API key from the keychain", error))
}

/// Prefix of the error every command returns when a profile has no stored key.
///
/// The frontend matches this to render "connect your API key" instead of a
/// generic failure (`albums.ts`), so the wording is a contract, not a message.
/// It lived inline in ten command sites before this; keep it in one place.
pub const MISSING_API_KEY_ERROR: &str = "No API key found for profile";

/// The profile's API key, or an error naming the profile when none is stored.
pub fn require_api_key(profile_id: &str) -> Result<String, String> {
    get_api_key(profile_id)?.ok_or_else(|| format!("{MISSING_API_KEY_ERROR}: {profile_id}"))
}

pub fn delete_api_key(profile_id: &str) -> Result<(), String> {
    let _guard = keychain_guard();
    credential_store()
        .delete(profile_id)
        .map_err(|error| format_keychain_error("delete the API key from the keychain", error))
}

/// In-memory credential backend for unit tests, in this crate's test builds
/// only. Every `keychain::` call from any test module reaches this store, so
/// tests never touch the developer's real keychain.
#[cfg(test)]
pub(crate) mod test_store {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex, MutexGuard};

    #[derive(Default)]
    struct State {
        secrets: HashMap<String, String>,
        /// One-shot: the next `set` persists this instead of the value it was
        /// given, standing in for a credential store that accepts a write and
        /// then serves something else back.
        corrupt_next_write: Option<String>,
        /// One-shot: the rollback's restoring `set` fails with this message.
        /// Consumed only after `corrupt_next_write`, so a test can arm both and
        /// have the write under test be accepted-then-corrupted while the undo
        /// that follows it fails — the case where the store keeps a secret the
        /// caller was told was not saved.
        fail_restore: Option<String>,
        /// One-shot: the next `delete` fails with this message. This is the
        /// rollback's other branch, taken when there was no previous secret.
        fail_next_delete: Option<String>,
    }

    static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

    fn state() -> MutexGuard<'static, State> {
        STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The fake store is process-wide, exactly like the keychain it stands in
    /// for, and every injected misbehaviour is a single one-shot slot. Tests
    /// that arm one or assert whole-store state must hold this and reset first,
    /// or a sibling test consumes the armed write, delete, or failure.
    static EXCLUSIVE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    pub(crate) fn exclusive() -> MutexGuard<'static, ()> {
        EXCLUSIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn reset() {
        let mut state = state();
        state.secrets.clear();
        state.corrupt_next_write = None;
        state.fail_restore = None;
        state.fail_next_delete = None;
    }

    pub(crate) fn seed(profile_id: &str, api_key: &str) {
        state()
            .secrets
            .insert(profile_id.to_string(), api_key.to_string());
    }

    /// Reads without going through `get_api_key`, so an assertion cannot be
    /// satisfied by the same bug it is meant to catch.
    pub(crate) fn peek(profile_id: &str) -> Option<String> {
        state().secrets.get(profile_id).cloned()
    }

    pub(crate) fn corrupt_next_write(stored_instead: &str) {
        state().corrupt_next_write = Some(stored_instead.to_string());
    }

    pub(crate) fn fail_restore(error: &str) {
        state().fail_restore = Some(error.to_string());
    }

    pub(crate) fn fail_next_delete(error: &str) {
        state().fail_next_delete = Some(error.to_string());
    }

    pub(super) struct FakeStore;

    impl super::CredentialStore for FakeStore {
        fn get(&self, profile_id: &str) -> Result<Option<String>, String> {
            Ok(state().secrets.get(profile_id).cloned())
        }

        fn set(&self, profile_id: &str, api_key: &str) -> Result<(), String> {
            let mut state = state();
            if let Some(stored) = state.corrupt_next_write.take() {
                state.secrets.insert(profile_id.to_string(), stored);
                return Ok(());
            }
            if let Some(error) = state.fail_restore.take() {
                return Err(error);
            }
            state
                .secrets
                .insert(profile_id.to_string(), api_key.to_string());
            Ok(())
        }

        fn delete(&self, profile_id: &str) -> Result<(), String> {
            let mut state = state();
            if let Some(error) = state.fail_next_delete.take() {
                return Err(error);
            }
            state.secrets.remove(profile_id);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end round trip against the real OS credential store. Ignored by
    /// default because it needs an unlocked keychain/secret-service and would be
    /// flaky in headless CI; run explicitly with `cargo test -- --ignored` to
    /// verify the credential backend (e.g. after a keyring version bump).
    /// Addresses `SystemKeychain` directly: the public functions are wired to
    /// the in-memory store in test builds.
    #[test]
    #[ignore]
    fn store_get_delete_round_trip() {
        let profile = format!("__it_keychain_{}", uuid::Uuid::new_v4());
        let store = SystemKeychain;
        // Absent before storing.
        assert_eq!(store.get(&profile).unwrap(), None);
        store_api_key_in(&store, &profile, "s3cr3t").unwrap();
        assert_eq!(store.get(&profile).unwrap(), Some("s3cr3t".to_string()));
        store.delete(&profile).unwrap();
        // Absent again; a second delete is idempotent.
        assert_eq!(store.get(&profile).unwrap(), None);
        store.delete(&profile).unwrap();
    }

    #[test]
    fn keychain_errors_include_recovery_guidance_and_backend_error() {
        let message =
            format_keychain_error("read the API key from the keychain", "backend failure");

        assert!(message.contains(KEYCHAIN_RECOVERY_GUIDANCE));
        assert!(message.contains("Original backend error: backend failure"));
    }

    /// A failed verification must not leave the new secret in place: the caller
    /// is told the save failed, so the previous key has to stay the live one.
    #[test]
    fn failed_readback_restores_the_previous_api_key() {
        let _guard = test_store::exclusive();
        test_store::reset();
        let profile = format!("__unit_keychain_{}", uuid::Uuid::new_v4());
        test_store::seed(&profile, "old-key");
        test_store::corrupt_next_write("garbage");

        let error = store_api_key(&profile, "new-key").expect_err("readback mismatch must fail");

        assert!(error.contains("readback returned a different value"));
        assert_eq!(test_store::peek(&profile).as_deref(), Some("old-key"));
        assert_eq!(get_api_key(&profile).unwrap().as_deref(), Some("old-key"));
    }

    /// With no previous credential there is nothing to restore, so the rejected
    /// key must be removed — a leftover key makes an unsaved profile look
    /// connected and lets a later command authenticate with it.
    #[test]
    fn failed_readback_leaves_no_credential_when_there_was_none() {
        let _guard = test_store::exclusive();
        test_store::reset();
        let profile = format!("__unit_keychain_{}", uuid::Uuid::new_v4());
        test_store::corrupt_next_write("garbage");

        let error = store_api_key(&profile, "new-key").expect_err("readback mismatch must fail");

        assert!(error.contains("readback returned a different value"));
        assert_eq!(test_store::peek(&profile), None);
        assert_eq!(get_api_key(&profile).unwrap(), None);
    }

    /// When the rollback's restore fails too, the store is left holding a
    /// secret the caller is being told was not saved. The reported message is
    /// then the only thing that can tell the user their previous key is gone,
    /// so it has to name both failures, not just the one that started it.
    #[test]
    fn failed_readback_reports_the_restore_failure_as_well() {
        let _guard = test_store::exclusive();
        test_store::reset();
        let profile = format!("__unit_keychain_{}", uuid::Uuid::new_v4());
        test_store::seed(&profile, "old-key");
        test_store::corrupt_next_write("garbage");
        test_store::fail_restore("keychain is locked");

        let error = store_api_key(&profile, "new-key").expect_err("readback mismatch must fail");

        assert!(
            error.contains("readback returned a different value"),
            "message must keep the failure that started this: {error}"
        );
        assert!(
            error.contains(
                "additionally failed to restore the previous API key: keychain is locked"
            ),
            "message must name the failed rollback and the backend reason: {error}"
        );
        // The undo could not run, so the rejected value is still there. Nothing
        // is silently correct here; the message above is the whole remedy.
        assert_eq!(test_store::peek(&profile).as_deref(), Some("garbage"));
    }

    /// Same contract for the rollback's other branch: with no previous key the
    /// undo is a delete, and a delete that fails leaves the rejected key live.
    #[test]
    fn failed_readback_reports_the_delete_failure_as_well() {
        let _guard = test_store::exclusive();
        test_store::reset();
        let profile = format!("__unit_keychain_{}", uuid::Uuid::new_v4());
        test_store::corrupt_next_write("garbage");
        test_store::fail_next_delete("credential manager unavailable");

        let error = store_api_key(&profile, "new-key").expect_err("readback mismatch must fail");

        assert!(
            error.contains("readback returned a different value"),
            "message must keep the failure that started this: {error}"
        );
        assert!(
            error.contains("additionally failed")
                && error.contains("credential manager unavailable"),
            "message must name the failed rollback and the backend reason: {error}"
        );
        assert_eq!(test_store::peek(&profile).as_deref(), Some("garbage"));
    }
}
