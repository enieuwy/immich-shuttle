use crate::models::profile::Profile;
use crate::services::immich_client::probe_is_immich;

/// Resolve which server URL to upload to, preferring LAN then WAN over the
/// primary. A LAN/WAN alternate is only selected once it is confirmed to be a
/// reachable Immich server (see `probe_is_immich`); failover based on bare TCP
/// port reachability would let the API key and uploads be sent to any unrelated
/// service listening on the configured host:port.
pub async fn resolve_server_url(profile: &Profile) -> String {
    if let Some(lan) = &profile.lan_server_url {
        if probe_is_immich(lan).await {
            return lan.clone();
        }
    }
    if let Some(wan) = &profile.wan_server_url {
        if probe_is_immich(wan).await {
            return wan.clone();
        }
    }
    profile.server_url.clone()
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

    /// Spawn a minimal HTTP responder that replies to every request with the
    /// given status line and JSON body. Returns its `http://127.0.0.1:<port>`
    /// base URL. Used to stand in for (and to impersonate) an Immich server.
    async fn spawn_http_stub(status_line: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        tokio::spawn(async move {
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
        format!("http://127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn returns_primary_without_optional_urls() {
        let resolved = resolve_server_url(&profile(None, None)).await;
        assert_eq!(resolved, "https://immich.example.com");
    }

    #[tokio::test]
    async fn returns_lan_when_it_responds_as_immich() {
        let lan = spawn_http_stub("200 OK", "{\"res\":\"pong\"}").await;
        let resolved = resolve_server_url(&profile(
            Some(lan.clone()),
            Some("https://wan.example.com".into()),
        ))
        .await;
        assert_eq!(resolved, lan);
    }

    #[tokio::test]
    async fn falls_back_to_wan_when_lan_is_invalid() {
        let wan = spawn_http_stub("200 OK", "{\"res\":\"pong\"}").await;
        let resolved =
            resolve_server_url(&profile(Some("not-a-url".into()), Some(wan.clone()))).await;
        assert_eq!(resolved, wan);
    }

    #[tokio::test]
    async fn does_not_select_a_non_immich_service_on_the_port() {
        // A service that merely holds the port open (answers HTTP but is not
        // Immich) must NOT be selected — otherwise the API key would be sent to
        // an unrelated/attacker-controlled listener. Failover falls through to
        // the primary instead.
        let lan = spawn_http_stub("200 OK", "{\"service\":\"not-immich\"}").await;
        let resolved = resolve_server_url(&profile(Some(lan), None)).await;
        assert_eq!(resolved, "https://immich.example.com");
    }

    #[tokio::test]
    async fn does_not_select_an_endpoint_that_errors() {
        let lan = spawn_http_stub("500 Internal Server Error", "boom").await;
        let resolved = resolve_server_url(&profile(Some(lan), None)).await;
        assert_eq!(resolved, "https://immich.example.com");
    }
}
