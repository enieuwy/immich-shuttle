use crate::models::tag::Tag;
use crate::services::{immich_client::ImmichClient, keychain, profile_store, url_resolver};

#[tauri::command]
pub async fn tags_list(profile_id: String) -> Result<Vec<Tag>, String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::require_api_key(&profile_id)?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let client = ImmichClient::new(&server_url, &api_key);
    client.list_tags().await
}
