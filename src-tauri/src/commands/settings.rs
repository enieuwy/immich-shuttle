use crate::models::profile::ServerInfo;
use crate::services::{
    immich_client::{server_compatibility, ImmichClient},
    keychain, logs, profile_store, url_resolver,
};

#[tauri::command]
pub async fn get_server_info(profile_id: String) -> Result<ServerInfo, String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let api_key = keychain::require_api_key(&profile_id)?;
    let server_url = url_resolver::resolve_server_url(&profile).await;
    let client = ImmichClient::new(&server_url, &api_key);
    let version = client.get_server_version().await?;
    let user = client.get_my_user().await?;
    let (is_compatible, warning) = server_compatibility(&version);

    Ok(ServerInfo {
        user_name: user
            .name
            .or(user.email)
            .unwrap_or_else(|| "Immich User".to_string()),
        server_version: format!("{}.{}.{}", version.major, version.minor, version.patch),
        is_compatible,
        warning,
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

/// Decide what may be handed to the OS opener for a resolved server base.
///
/// This is the authorization step, not a formatting step, so it is a pure
/// function rather than inline command code: the scheme guard is the only thing
/// standing between a stored profile URL and a host-side launch, and a silent
/// regression here would let `file:`, `mailto:`, or a custom protocol handler
/// through. A pure function can be tested without a profile store or a live
/// server, so the guard cannot rot unnoticed.
fn openable_immich_url(base: &str, album_id: Option<&str>) -> Result<String, String> {
    if base.is_empty() {
        return Err("No reachable Immich server URL for this profile.".to_string());
    }
    // Only ever hand an http(s) URL to the OS opener: a stored profile URL with
    // another scheme (mailto:, file:, a custom protocol handler) must never be
    // launched host-side.
    let lower = base.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("Immich server URL must start with http:// or https://.".to_string());
    }
    Ok(immich_web_url(base, album_id))
}

/// Open the Immich web UI for a profile in the user's browser: the target album
/// when `album_id` is set, else the timeline. Resolves the reachable base URL
/// (LAN/WAN failover, same as imports) and opens from the host side, matching
/// `open_logs_dir` — so it needs no renderer opener capability.
#[tauri::command]
pub async fn open_in_immich(profile_id: String, album_id: Option<String>) -> Result<(), String> {
    let profile = profile_store::get_profile(&profile_id)?;
    let base = url_resolver::resolve_server_url(&profile).await;
    let url = openable_immich_url(&base, album_id.as_deref())?;
    tauri_plugin_opener::open_url(url, None::<String>)
        .map_err(|e| format!("Could not open Immich: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{immich_web_url, openable_immich_url};

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

    /// The opener runs host-side, so a stored profile URL with a launchable
    /// scheme would be a local code-execution surface, not a broken link.
    #[test]
    fn only_http_schemes_reach_the_opener() {
        for hostile in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "mailto:someone@example.com",
            "immich-shuttle://open",
            "ftp://immich.example.com",
        ] {
            assert_eq!(
                openable_immich_url(hostile, None),
                Err("Immich server URL must start with http:// or https://.".to_string()),
                "{hostile} must not reach the OS opener"
            );
        }

        assert_eq!(
            openable_immich_url("HTTPS://Immich.Example.com", Some("abc")),
            Ok("HTTPS://Immich.Example.com/albums/abc".to_string())
        );
    }

    /// An unreachable profile must say so instead of opening a bare path.
    #[test]
    fn an_unresolved_base_is_refused() {
        assert_eq!(
            openable_immich_url("", None),
            Err("No reachable Immich server URL for this profile.".to_string())
        );
    }
}
