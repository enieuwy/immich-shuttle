use crate::models::album::{Album, AlbumShareLink};
use crate::services::{immich_client::ImmichClient, keychain, profile_store, url_resolver};

#[tauri::command]
pub async fn albums_list(profile_id: String, query: Option<String>) -> Result<Vec<Album>, String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::get_api_key(&profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {profile_id}"))?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let client = ImmichClient::new(&server_url, &api_key);
    client.list_albums(query.as_deref()).await
}

#[tauri::command]
pub async fn album_create(profile_id: String, name: String) -> Result<Album, String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::get_api_key(&profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {profile_id}"))?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let client = ImmichClient::new(&server_url, &api_key);
    client.create_album(name.trim()).await
}

#[tauri::command]
pub async fn album_share_users(
    profile_id: String,
    album_id: String,
    user_ids: Vec<String>,
    role: String,
) -> Result<(), String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::get_api_key(&profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {profile_id}"))?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let client = ImmichClient::new(&server_url, &api_key);
    client.share_album_users(&album_id, &user_ids, &role).await
}

#[tauri::command]
pub async fn album_share_link(
    profile_id: String,
    album_id: String,
) -> Result<AlbumShareLink, String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::get_api_key(&profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {profile_id}"))?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let client = ImmichClient::new(&server_url, &api_key);
    client
        .create_share_link(&album_id, &profile.server_url)
        .await
}
