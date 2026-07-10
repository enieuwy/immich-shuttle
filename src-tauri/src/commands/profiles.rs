use uuid::Uuid;

use crate::{
    models::profile::{Profile, ProfileInput, ServerInfo},
    services::{
        immich_client::{normalize_server_url, ImmichClient},
        keychain, profile_store,
    },
};

#[tauri::command]
pub async fn profiles_list() -> Result<Vec<Profile>, String> {
    profile_store::list_profiles()
}

#[tauri::command]
pub async fn profile_upsert(input: ProfileInput) -> Result<Profile, String> {
    if input.server_url.trim().is_empty() {
        return Err("Server URL is required".to_string());
    }

    let is_new = input.id.is_none();
    let id = input.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let profile = Profile {
        id: id.clone(),
        display_name: input
            .display_name
            .unwrap_or_else(|| "Immich User".to_string()),
        server_url: normalize_server_url(&input.server_url),
        lan_server_url: input.lan_server_url,
        wan_server_url: input.wan_server_url,
    };

    let stored_key = match input.api_key {
        Some(api_key) if !api_key.trim().is_empty() => {
            keychain::store_api_key(&id, api_key.trim())?;
            true
        }
        _ => false,
    };

    match profile_store::upsert_profile(profile) {
        Ok(saved) => Ok(saved),
        Err(err) => {
            // Roll back a just-stored credential for a brand-new profile so a
            // failed save can't orphan an API key under an unreferenced UUID
            // (each retry would otherwise mint a new UUID and leak another key).
            if is_new && stored_key {
                let _ = keychain::delete_api_key(&id);
            }
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn profile_delete(id: String) -> Result<(), String> {
    // Remove the profile first: if this fails the credential stays intact so the
    // profile remains usable, rather than leaving a broken, keyless profile.
    profile_store::delete_profile(&id)?;
    keychain::delete_api_key(&id)
}

#[tauri::command]
pub async fn profile_validate(url: String, api_key: String) -> Result<ServerInfo, String> {
    let normalized_url = normalize_server_url(&url);
    if normalized_url.is_empty() {
        return Err("Server URL is required".to_string());
    }
    if api_key.trim().is_empty() {
        return Err("API key is required".to_string());
    }

    let client = ImmichClient::new(&normalized_url, api_key.trim());
    client.ping().await?;
    let version = client.get_server_version().await?;
    let me = client.get_my_user().await?;

    let is_compatible = (version.major, version.minor, version.patch) >= (1, 106, 0);

    Ok(ServerInfo {
        user_name: me
            .name
            .or(me.email)
            .unwrap_or_else(|| "Immich User".to_string()),
        server_version: format!("{}.{}.{}", version.major, version.minor, version.patch),
        is_compatible,
        warning: if is_compatible {
            None
        } else {
            Some(
                "Immich server version may be below the minimum supported by bundled immich-go."
                    .to_string(),
            )
        },
    })
}
