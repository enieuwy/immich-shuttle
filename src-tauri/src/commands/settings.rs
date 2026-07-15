use crate::models::profile::ServerInfo;
use crate::services::{immich_client::ImmichClient, keychain, logs, profile_store, url_resolver};

#[tauri::command]
pub async fn get_server_info(profile_id: String) -> Result<ServerInfo, String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::get_api_key(&profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {profile_id}"))?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let client = ImmichClient::new(&server_url, &api_key);
    let version = client.get_server_version().await?;
    let user = client.get_my_user().await?;
    let is_compatible = (version.major, version.minor, version.patch) >= (1, 106, 0);

    Ok(ServerInfo {
        user_name: user
            .name
            .or(user.email)
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

#[tauri::command]
pub async fn get_logs_dir() -> Result<String, String> {
    Ok(logs::logs_dir()?.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn get_recent_logs() -> Result<String, String> {
    logs::read_recent("app.log", 500)
}

#[tauri::command]
pub async fn open_logs_dir() -> Result<(), String> {
    let path = logs::logs_dir()?;
    tauri_plugin_opener::open_path(&path, None::<String>)
        .map_err(|e| format!("Could not open logs folder: {e}"))
}
