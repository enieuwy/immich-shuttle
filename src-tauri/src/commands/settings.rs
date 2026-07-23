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

/// Build the Immich web URL for a resolved server base. Points at a specific
/// album when one is given, otherwise the main timeline. Kept pure so the
/// path-joining (trailing-slash handling) is unit-tested without a live server.
fn immich_web_url(base: &str, album_id: Option<&str>) -> String {
    let base = base.trim_end_matches('/');
    match album_id {
        Some(id) if !id.is_empty() => format!("{base}/albums/{id}"),
        _ => format!("{base}/photos"),
    }
}

/// Open the Immich web UI for a profile in the user's browser: the target album
/// when `album_id` is set, else the timeline. Resolves the reachable base URL
/// (LAN/WAN failover, same as imports) and opens from the host side, matching
/// `open_logs_dir` — so it needs no renderer opener capability.
#[tauri::command]
pub async fn open_in_immich(profile_id: String, album_id: Option<String>) -> Result<(), String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let base = url_resolver::resolve_server_url(&profile).await;
    if base.is_empty() {
        return Err("No reachable Immich server URL for this profile.".to_string());
    }
    // Only ever hand an http(s) URL to the OS opener: a stored profile URL with
    // another scheme (mailto:, file:, a custom protocol handler) must never be
    // launched host-side.
    let scheme_ok = {
        let lower = base.to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://")
    };
    if !scheme_ok {
        return Err("Immich server URL must start with http:// or https://.".to_string());
    }
    let url = immich_web_url(&base, album_id.as_deref());
    tauri_plugin_opener::open_url(url, None::<String>)
        .map_err(|e| format!("Could not open Immich: {e}"))
}

#[cfg(test)]
mod tests {
    use super::immich_web_url;

    #[test]
    fn album_url_targets_the_album() {
        assert_eq!(
            immich_web_url("https://immich.example.com", Some("abc123")),
            "https://immich.example.com/albums/abc123"
        );
    }

    #[test]
    fn no_album_falls_back_to_timeline() {
        assert_eq!(
            immich_web_url("https://immich.example.com", None),
            "https://immich.example.com/photos"
        );
        assert_eq!(
            immich_web_url("https://immich.example.com", Some("")),
            "https://immich.example.com/photos"
        );
    }

    #[test]
    fn trailing_slash_is_not_doubled() {
        assert_eq!(
            immich_web_url("http://192.168.1.10:2283/", Some("x")),
            "http://192.168.1.10:2283/albums/x"
        );
    }
}
