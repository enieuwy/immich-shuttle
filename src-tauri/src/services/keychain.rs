use keyring::Entry;

const KEYCHAIN_SERVICE: &str = "immich-shuttle";

fn entry(profile_id: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, profile_id).map_err(|e| format!("Could not access keyring: {e}"))
}

pub fn store_api_key(profile_id: &str, api_key: &str) -> Result<(), String> {
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
