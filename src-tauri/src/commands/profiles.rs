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

    if let Some(api_key) = input.api_key {
        if !api_key.trim().is_empty() {
            keychain::store_api_key(&id, api_key.trim())?;
        }
    }

    profile_store::upsert_profile(profile)
}

#[tauri::command]
pub async fn profile_delete(id: String) -> Result<(), String> {
    keychain::delete_api_key(&id)?;
    profile_store::delete_profile(&id)
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
