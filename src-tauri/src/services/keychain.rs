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

fn entry(profile_id: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, profile_id)
        .map_err(|error| format_keychain_error("access the keyring", error))
}

pub fn store_api_key(profile_id: &str, api_key: &str) -> Result<(), String> {
    let _guard = keychain_guard();
    let e = entry(profile_id)?;
    e.set_password(api_key)
        .map_err(|error| format_keychain_error("store API key in the keychain", error))?;

    // Verify the write persisted (guards against mock/no-op credential stores)
    let readback = entry(profile_id)?.get_password().map_err(|error| {
        format_keychain_error("verify the API key in the keychain after writing", error)
    })?;
    if readback != api_key {
        return Err(format!(
            "Keychain write succeeded but readback returned different value. {KEYCHAIN_RECOVERY_GUIDANCE}"
        ));
    }
    Ok(())
}

pub fn get_api_key(profile_id: &str) -> Result<Option<String>, String> {
    let _guard = keychain_guard();
    let e = entry(profile_id)?;
    match e.get_password() {
        Ok(v) => Ok(Some(v)),
        // No credential was ever stored for this profile — not an error.
        Err(KeyringError::NoEntry) => Ok(None),
        Err(err) => Err(format_keychain_error(
            "read the API key from the keychain",
            err,
        )),
    }
}

pub fn delete_api_key(profile_id: &str) -> Result<(), String> {
    let _guard = keychain_guard();
    let e = entry(profile_id)?;
    match e.delete_credential() {
        Ok(()) => Ok(()),
        // Already absent — deletion is idempotent.
        Err(KeyringError::NoEntry) => Ok(()),
        Err(err) => Err(format_keychain_error(
            "delete the API key from the keychain",
            err,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end round trip against the real OS credential store. Ignored by
    /// default because it needs an unlocked keychain/secret-service and would be
    /// flaky in headless CI; run explicitly with `cargo test -- --ignored` to
    /// verify the credential backend (e.g. after a keyring version bump).
    #[test]
    #[ignore]
    fn store_get_delete_round_trip() {
        let profile = format!("__it_keychain_{}", uuid::Uuid::new_v4());
        // Absent before storing.
        assert_eq!(get_api_key(&profile).unwrap(), None);
        store_api_key(&profile, "s3cr3t").unwrap();
        assert_eq!(get_api_key(&profile).unwrap(), Some("s3cr3t".to_string()));
        delete_api_key(&profile).unwrap();
        // Absent again; a second delete is idempotent.
        assert_eq!(get_api_key(&profile).unwrap(), None);
        delete_api_key(&profile).unwrap();
    }

    #[test]
    fn keychain_errors_include_recovery_guidance_and_backend_error() {
        let message =
            format_keychain_error("read the API key from the keychain", "backend failure");

        assert!(message.contains(KEYCHAIN_RECOVERY_GUIDANCE));
        assert!(message.contains("Original backend error: backend failure"));
    }
}
