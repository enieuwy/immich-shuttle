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

/// Scan the local network for reachable Immich servers, returning confirmed
/// base URLs the user can one-click into a profile. Read-only; probes only the
/// unauthenticated ping endpoint.
#[tauri::command]
pub async fn discover_immich_servers() -> Result<Vec<String>, String> {
    Ok(crate::services::discovery::discover_immich_servers().await)
}

#[tauri::command]
pub async fn profile_upsert(input: ProfileInput) -> Result<Profile, String> {
    if input.server_url.trim().is_empty() {
        return Err("Server URL is required".to_string());
    }

    let id = input
        .id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let profile = profile_from_input(&input, id.clone());

    let api_key = input
        .api_key
        .filter(|api_key| !api_key.trim().is_empty())
        .map(|api_key| api_key.trim().to_string());
    let _guard = profile_store::lock_config();
    let previous_api_key = if api_key.is_some() {
        keychain::get_api_key(&id)?
    } else {
        None
    };

    let stored_key = if let Some(api_key) = api_key {
        keychain::store_api_key(&id, &api_key)?;
        true
    } else {
        false
    };

    match profile_store::upsert_profile_locked(profile) {
        Ok(saved) => Ok(saved),
        Err(err) if stored_key => {
            let rollback = match previous_api_key {
                Some(api_key) => keychain::store_api_key(&id, &api_key),
                None => keychain::delete_api_key(&id),
            };
            match rollback {
                Ok(()) => Err(err),
                Err(rollback_err) => Err(format!(
                    "{err}; additionally failed to roll back the API key change: {rollback_err}"
                )),
            }
        }
        Err(err) => Err(err),
    }
}

fn profile_from_input(input: &ProfileInput, id: String) -> Profile {
    Profile {
        id,
        display_name: input
            .display_name
            .clone()
            .unwrap_or_else(|| "Immich User".to_string()),
        server_url: normalize_server_url(&input.server_url),
        lan_server_url: input
            .lan_server_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(normalize_server_url),
        wan_server_url: input
            .wan_server_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(normalize_server_url),
    }
}

#[tauri::command]
pub async fn profile_delete(id: String) -> Result<(), String> {
    let _guard = profile_store::lock_config();
    let previous_api_key = keychain::get_api_key(&id)?;
    keychain::delete_api_key(&id)?;

    if let Err(err) = profile_store::delete_profile_locked(&id) {
        if let Some(api_key) = previous_api_key {
            if let Err(rollback_err) = keychain::store_api_key(&id, &api_key) {
                return Err(format!(
                    "{err}; additionally failed to restore the API key after the profile delete failed: {rollback_err}"
                ));
            }
        }
        return Err(err);
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::profile_from_input;
    use crate::models::profile::ProfileInput;

    #[test]
    fn profile_builder_normalizes_optional_lan_and_wan_urls() {
        let profile = profile_from_input(
            &ProfileInput {
                id: None,
                display_name: None,
                server_url: "https://immich.example.com".to_string(),
                lan_server_url: Some(" https://lan.example.com/ ".to_string()),
                wan_server_url: Some("https://wan.example.com/api".to_string()),
                api_key: None,
            },
            "profile-id".to_string(),
        );

        assert_eq!(
            profile.lan_server_url.as_deref(),
            Some("https://lan.example.com")
        );
        assert_eq!(
            profile.wan_server_url.as_deref(),
            Some("https://wan.example.com")
        );
    }
}
