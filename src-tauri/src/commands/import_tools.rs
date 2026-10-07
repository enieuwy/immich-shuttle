use crate::services::{immich_client::ImmichClient, keychain, profile_store, url_resolver};
use serde_json::Value;

async fn client(profile_id: &str) -> Result<ImmichClient, String> {
    let profile = profile_store::get_profile(profile_id)?;
    let key = keychain::require_api_key(profile_id)?;
    Ok(ImmichClient::new(
        &url_resolver::resolve_server_url(&profile).await,
        &key,
    ))
}

#[tauri::command]
pub async fn import_storage(profile_id: String) -> Result<Value, String> {
    client(&profile_id).await?.storage_headroom().await
}

#[tauri::command]
pub async fn import_register_library(
    profile_id: String,
    name: String,
    server_path: String,
    confirm_server_path: bool,
) -> Result<Value, String> {
    if !confirm_server_path
        || name.trim().is_empty()
        || !server_path.starts_with('/')
        || server_path.contains(['\n', '\r'])
    {
        return Err("Confirm an absolute path inside the server container and enter a library name. This is not a local desktop path.".into());
    }
    client(&profile_id)
        .await?
        .register_library(name.trim(), &server_path)
        .await
}
