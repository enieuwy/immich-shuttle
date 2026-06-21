use std::time::Duration;

use reqwest::Url;
use tokio::{net::TcpStream, time::timeout};

use crate::models::profile::Profile;

async fn can_connect(url: &str) -> bool {
    let parsed = match Url::parse(url) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let host = match parsed.host_str() {
        Some(v) => v,
        None => return false,
    };
    let port = parsed
        .port_or_known_default()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });

    timeout(
        Duration::from_millis(1500),
        TcpStream::connect(format!("{host}:{port}")),
    )
    .await
    .is_ok()
}

pub async fn resolve_server_url(profile: &Profile) -> String {
    if let Some(lan) = &profile.lan_server_url {
        if can_connect(lan).await {
            return lan.clone();
        }
    }
    if let Some(wan) = &profile.wan_server_url {
        if can_connect(wan).await {
            return wan.clone();
        }
    }
    profile.server_url.clone()
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::resolve_server_url;
    use crate::models::profile::Profile;

    #[tokio::test]
    async fn returns_primary_without_optional_urls() {
        let profile = Profile {
            id: "1".to_string(),
            display_name: "Test".to_string(),
            server_url: "https://immich.example.com".to_string(),
            lan_server_url: None,
            wan_server_url: None,
        };

        let resolved = resolve_server_url(&profile).await;
        assert_eq!(resolved, "https://immich.example.com");
    }

    #[tokio::test]
    async fn falls_back_to_wan_when_lan_is_invalid() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let wan = format!("http://127.0.0.1:{}", addr.port());

        let profile = Profile {
            id: "1".to_string(),
            display_name: "Test".to_string(),
            server_url: "https://immich.example.com".to_string(),
            lan_server_url: Some("not-a-url".to_string()),
            wan_server_url: Some(wan.clone()),
        };

        let resolved = resolve_server_url(&profile).await;
        assert_eq!(resolved, wan);
    }

    #[tokio::test]
    async fn returns_lan_when_reachable() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let lan = format!("http://127.0.0.1:{}", addr.port());

        let profile = Profile {
            id: "1".to_string(),
            display_name: "Test".to_string(),
            server_url: "https://immich.example.com".to_string(),
            lan_server_url: Some(lan.clone()),
            wan_server_url: Some("https://wan.example.com".to_string()),
        };

        let resolved = resolve_server_url(&profile).await;
        assert_eq!(resolved, lan);
    }
}
