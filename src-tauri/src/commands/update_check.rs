//! Explicit, notification-only release checks. No updater or background fetch.
use std::time::Duration;
use serde::{Deserialize, Serialize};

pub const RELEASES_URL: &str = "https://github.com/enieuwy/immich-shuttle/releases";
const RELEASE_API: &str = "https://api.github.com/repos/enieuwy/immich-shuttle/releases/latest";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

#[derive(Debug, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub releases_url: &'static str,
}

fn compare_release(current: &str, latest: &str) -> Result<bool, String> {
    let parse = |value: &str| semver::Version::parse(value.strip_prefix('v').unwrap_or(value))
        .map_err(|_| "Release version is not valid semantic versioning".to_string());
    let current = parse(current)?;
    let latest = parse(latest)?;
    // Build metadata does not change version precedence.
    Ok(latest.cmp_precedence(&current).is_gt())
}

async fn fetch_release(client: &reqwest::Client, url: &str) -> Result<Release, String> {
    let mut response = client.get(url)
        .header("Accept", "application/vnd.github+json")
        .send().await.map_err(|e| format!("Could not check releases: {e}"))?
        .error_for_status().map_err(|e| format!("Release service returned an error: {e}"))?;
    if response.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
        return Err("Release response exceeds the size limit".into());
    }
    let mut body = Vec::with_capacity(response.content_length().unwrap_or(0) as usize);
    while let Some(chunk) = response.chunk().await.map_err(|e| format!("Could not read release response: {e}"))? {
        if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
            return Err("Release response exceeds the size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|e| format!("Invalid release response: {e}"))
}

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateStatus, String> {
    let current_version = app.package_info().version.to_string();
    let client = reqwest::Client::builder()
        .user_agent(concat!("Immich-Shuttle/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build().map_err(|e| format!("Could not create release client: {e}"))?;
    let release = fetch_release(&client, RELEASE_API).await?;
    if release.draft || release.prerelease {
        return Err("Release service did not return a stable public release".into());
    }
    let update_available = compare_release(&current_version, &release.tag_name)?;
    Ok(UpdateStatus { current_version, latest_version: release.tag_name, update_available, releases_url: RELEASES_URL })
}

#[tauri::command]
pub async fn open_project_releases() -> Result<(), String> {
    // Never open a URL supplied by an API response or renderer.
    tauri_plugin_opener::open_url(RELEASES_URL, None::<String>)
        .map_err(|e| format!("Could not open the project releases page: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_use_semantic_precedence_not_text_order() {
        assert!(compare_release("0.9.9", "v0.10.0").unwrap());
        assert!(!compare_release("1.10.0", "1.9.9").unwrap());
        assert!(!compare_release("1.0.0+local", "1.0.0+release").unwrap());
        assert!(compare_release("1.0.0-rc.9", "1.0.0-rc.10").unwrap());
        assert!(compare_release("1.0.0-rc.10", "1.0.0").unwrap());
        assert!(!compare_release("1.0.0", "1.0.0-rc.10").unwrap());
        assert!(compare_release("0.8.1", "not-a-version").is_err());
    }

    #[tokio::test]
    async fn release_fetch_handles_local_service_errors_and_valid_response() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for body in [r#"{"tag_name":"v0.10.0","draft":false,"prerelease":false}"#, "invalid-json"] {
                let (mut connection, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                connection.read(&mut request).unwrap();
                write!(connection, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
        let url = format!("http://{address}/latest");
        let release = fetch_release(&client, &url).await.unwrap();
        assert!(compare_release("0.8.1", &release.tag_name).unwrap());
        assert!(fetch_release(&client, &url).await.is_err());
        server.join().unwrap();
    }
}
