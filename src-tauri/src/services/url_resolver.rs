use reqwest::Url;

use crate::{
    models::profile::Profile,
    services::immich_client::{normalize_server_url, probe_is_immich},
};

/// Resolve which server URL to upload to, preferring LAN then WAN over the
/// primary. A LAN/WAN alternate is only selected once it is confirmed to be a
/// reachable Immich server (see `probe_is_immich`); failover based on bare TCP
/// port reachability would let the API key and uploads be sent to any unrelated
/// service listening on the configured host:port.
pub async fn resolve_server_url(profile: &Profile) -> String {
    if let Some(lan) = profile
        .lan_server_url
        .as_deref()
        .and_then(normalized_server_url)
    {
        if probe_is_immich(&lan).await {
            return lan;
        }
    }
    if let Some(wan) = profile
        .wan_server_url
        .as_deref()
        .and_then(normalized_server_url)
    {
        if probe_is_immich(&wan).await {
            return wan;
        }
    }
    normalized_server_url(&profile.server_url).unwrap_or_default()
}

/// A stored profile may predate URL credential stripping. Do not pass its
/// original string to a probe, the sidecar, or an error path. Invalid values
/// cannot identify a safe destination, so resolution returns no URL.
fn normalized_server_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return None;
    }

    let _ = url.set_username("");
    let _ = url.set_password(None);
    Some(normalize_server_url(url.as_str()))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::resolve_server_url;
    use crate::models::profile::Profile;

    fn profile(lan: Option<String>, wan: Option<String>) -> Profile {
        Profile {
            id: "1".to_string(),
            display_name: "Test".to_string(),
            server_url: "https://immich.example.com".to_string(),
            lan_server_url: lan,
            wan_server_url: wan,
        }
    }

    /// Test guard for a stub HTTP server. Its accept loop is aborted on drop so
    /// no background task or open socket outlives the test that spawned it.
    struct HttpStub {
        url: String,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for HttpStub {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    /// Spawn a minimal HTTP responder that replies to every request with the
    /// given status line and JSON body. Used to stand in for (and to impersonate)
    /// an Immich server; the returned guard exposes `.url` and stops the server
    /// when dropped.
    async fn spawn_http_stub(status_line: &'static str, body: &'static str) -> HttpStub {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    continue;
                };
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        HttpStub {
            url: format!("http://127.0.0.1:{}", addr.port()),
            handle,
        }
    }

    #[tokio::test]
    async fn returns_primary_without_optional_urls() {
        let resolved = resolve_server_url(&profile(None, None)).await;
        assert_eq!(resolved, "https://immich.example.com");
    }

    #[tokio::test]
    async fn strips_userinfo_from_a_legacy_primary_before_returning_it() {
        let mut legacy = profile(None, None);
        legacy.server_url = "https://user:password@immich.example.com/api/".to_string();

        let resolved = resolve_server_url(&legacy).await;

        assert_eq!(resolved, "https://immich.example.com");
        assert!(!resolved.contains("user"));
        assert!(!resolved.contains("password"));
    }

    #[tokio::test]
    async fn rejects_an_invalid_legacy_primary_without_returning_userinfo() {
        let mut legacy = profile(None, None);
        legacy.server_url = "https://user:password@[".to_string();

        let resolved = resolve_server_url(&legacy).await;

        assert!(resolved.is_empty());
        assert!(!resolved.contains("user"));
        assert!(!resolved.contains("password"));
    }

    #[tokio::test]
    async fn returns_lan_when_it_responds_as_immich() {
        let lan = spawn_http_stub("200 OK", "{\"res\":\"pong\"}").await;
        let resolved = resolve_server_url(&profile(
            Some(lan.url.clone()),
            Some("https://wan.example.com".into()),
        ))
        .await;
        assert_eq!(resolved, lan.url);
    }

    #[tokio::test]
    async fn strips_userinfo_before_returning_a_reachable_legacy_lan_url() {
        let lan = spawn_http_stub("200 OK", "{\"res\":\"pong\"}").await;
        let legacy_lan = lan.url.replacen("http://", "http://user:password@", 1);

        let resolved = resolve_server_url(&profile(Some(legacy_lan), None)).await;

        assert_eq!(resolved, lan.url);
        assert!(!resolved.contains("user"));
        assert!(!resolved.contains("password"));
    }

    #[tokio::test]
    async fn falls_back_to_wan_when_lan_is_invalid() {
        let wan = spawn_http_stub("200 OK", "{\"res\":\"pong\"}").await;
        let resolved =
            resolve_server_url(&profile(Some("not-a-url".into()), Some(wan.url.clone()))).await;
        assert_eq!(resolved, wan.url);
    }

    #[tokio::test]
    async fn does_not_select_a_non_immich_service_on_the_port() {
        // A service that merely holds the port open (answers HTTP but is not
        // Immich) must NOT be selected — otherwise the API key would be sent to
        // an unrelated/attacker-controlled listener. Failover falls through to
        // the primary instead.
        let lan = spawn_http_stub("200 OK", "{\"service\":\"not-immich\"}").await;
        let resolved = resolve_server_url(&profile(Some(lan.url.clone()), None)).await;
        assert_eq!(resolved, "https://immich.example.com");
    }

    #[tokio::test]
    async fn does_not_select_an_endpoint_that_errors() {
        let lan = spawn_http_stub("500 Internal Server Error", "boom").await;
        let resolved = resolve_server_url(&profile(Some(lan.url.clone()), None)).await;
        assert_eq!(resolved, "https://immich.example.com");
    }
}
