use crate::models::album::AlbumUser;
use crate::services::{immich_client::ImmichClient, keychain, profile_store};

#[tauri::command]
pub async fn users_list(profile_id: String) -> Result<Vec<AlbumUser>, String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::get_api_key(&profile_id)?
        .ok_or_else(|| format!("No API key found for profile: {profile_id}"))?;
    let client = ImmichClient::new(&profile.server_url, &api_key);
    client.list_users().await
}
