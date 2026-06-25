use keyring::Entry;
use once_cell::sync::Lazy;
use std::sync::{Mutex, MutexGuard};

const KEYCHAIN_SERVICE: &str = "immich-shuttle";

/// Serializes all keychain access. On first access (e.g. after a code-signature
/// change invalidates the item ACL) macOS shows one prompt; concurrent reads —
/// several fire at startup (albums + users + server info) — queue behind this
/// lock instead of racing the prompt and failing. Once the user grants access
/// the queued reads proceed and succeed, so no app restart is needed.
static KEYCHAIN_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn keychain_guard() -> MutexGuard<'static, ()> {
    KEYCHAIN_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn entry(profile_id: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, profile_id).map_err(|e| format!("Could not access keyring: {e}"))
}

pub fn store_api_key(profile_id: &str, api_key: &str) -> Result<(), String> {
    let _guard = keychain_guard();
    let e = entry(profile_id)?;
    e.set_password(api_key)
        .map_err(|err| format!("Could not store API key in keychain: {err}"))?;

    // Verify the write persisted (guards against mock/no-op credential stores)
    let readback = entry(profile_id)?
        .get_password()
        .map_err(|err| format!("Keychain write succeeded but readback failed: {err}"))?;
    if readback != api_key {
        return Err("Keychain write succeeded but readback returned different value".to_string());
    }
    Ok(())
}

pub fn get_api_key(profile_id: &str) -> Result<Option<String>, String> {
    let _guard = keychain_guard();
    let e = entry(profile_id)?;
    match e.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("No entry found")
                || msg.contains("Item not found")
                || msg.contains("No matching entry")
            {
                Ok(None)
            } else if cfg!(target_os = "linux")
                && (msg.contains("secret service")
                    || msg.contains("org.freedesktop.secrets")
                    || msg.contains("No such interface"))
            {
                Err("Could not access system keychain. Install and unlock gnome-keyring or kwallet, then retry."
                    .to_string())
            } else {
                Err(format!("Could not read API key from keychain: {err}"))
            }
        }
    }
}

pub fn delete_api_key(profile_id: &str) -> Result<(), String> {
    let _guard = keychain_guard();
    let e = entry(profile_id)?;
    match e.delete_credential() {
        Ok(_) => Ok(()),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("No entry found")
                || msg.contains("Item not found")
                || msg.contains("No matching entry")
            {
                Ok(())
            } else {
                Err(format!("Could not delete API key from keychain: {err}"))
            }
        }
    }
}
