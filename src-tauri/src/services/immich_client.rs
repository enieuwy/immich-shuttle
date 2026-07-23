use std::{sync::LazyLock, time::Duration};

use reqwest::{Client, Method, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One shared HTTP client (connection pool + TLS config) reused across every
/// request. Building a fresh `Client` per call is wasteful and was a likely
/// source of flaky "error sending request" failures during the startup burst.
static HTTP: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| Client::new())
});

use crate::models::album::{Album, AlbumShareLink, AlbumUser};

/// JSON API responses are expected to be small. Bound reads so a malicious or
/// misconfigured endpoint cannot make the app buffer an unbounded response.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

async fn response_text_limited(mut response: Response) -> Result<String, String> {
    if let Some(content_length) = response.content_length() {
        if content_length > MAX_RESPONSE_BYTES as u64 {
            return Err(format!(
                "response exceeds the {} byte limit",
                MAX_RESPONSE_BYTES
            ));
        }
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("failed reading response chunk: {e}"))?
    {
        let total = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| "response exceeds the byte limit".to_string())?;
        if total > MAX_RESPONSE_BYTES {
            return Err(format!(
                "response exceeds the {} byte limit",
                MAX_RESPONSE_BYTES
            ));
        }
        body.extend_from_slice(&chunk);
    }

    String::from_utf8(body).map_err(|e| format!("response is not valid UTF-8: {e}"))
}

/// Immich server bases identify an origin (optionally behind a path-prefix), not
/// a resource. Discard a query and fragment so they cannot be inherited by API
/// requests or public share links.
fn server_base_url(value: &str) -> Option<Url> {
    let mut url = Url::parse(value.trim()).ok()?;
    url.set_query(None);
    url.set_fragment(None);

    let path = url.path().trim_end_matches('/').to_string();
    let root = path.strip_suffix("/api").unwrap_or(&path);
    url.set_path(if root.is_empty() { "/" } else { root });
    Some(url)
}

fn append_path_segments<'a>(
    base: &Url,
    segments: impl IntoIterator<Item = &'a str>,
) -> Result<Url, String> {
    let mut url = base.clone();
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| "Server URL cannot contain path segments".to_string())?;
        path.pop_if_empty();
        for segment in segments {
            if matches!(segment, "." | "..") {
                return Err("Server URL path cannot contain traversal segments".to_string());
            }
            if !segment.is_empty() {
                // `push` percent-encodes each segment, preventing callers from
                // injecting a separator or traversal through a dynamic id.
                path.push(segment);
            }
        }
    }
    Ok(url)
}

fn api_endpoint_urls(server_url: &str, endpoint_segments: &[&str]) -> Result<Vec<Url>, String> {
    let base =
        server_base_url(server_url).ok_or_else(|| format!("Invalid server URL: {server_url}"))?;

    // Prefer Immich's standard `/api` path, then retry the bare endpoint for a
    // reverse proxy that strips `/api`.
    Ok(vec![
        append_path_segments(
            &base,
            std::iter::once("api").chain(endpoint_segments.iter().copied()),
        )?,
        append_path_segments(&base, endpoint_segments.iter().copied())?,
    ])
}

fn share_link_url(server_url: &str, key: &str) -> Result<String, String> {
    let base =
        server_base_url(server_url).ok_or_else(|| format!("Invalid server URL: {server_url}"))?;
    Ok(append_path_segments(&base, ["share", key])?.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerVersion {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeUser {
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

pub struct ImmichClient {
    server_url: String,
    api_key: String,
    http: Client,
}

impl ImmichClient {
    pub fn new(server_url: &str, api_key: &str) -> Self {
        Self {
            server_url: normalize_server_url(server_url),
            api_key: api_key.to_string(),
            http: HTTP.clone(),
        }
    }

    async fn request_json(
        &self,
        method: Method,
        path: &[&str],
        body: Option<Value>,
    ) -> Result<Value, String> {
        let display_path = format!("/{}", path.join("/"));
        let candidates = api_endpoint_urls(&self.server_url, path)?;

        for (index, url) in candidates.iter().enumerate() {
            let has_alternate = index + 1 < candidates.len();
            let mut req = self
                .http
                .request(method.clone(), url.clone())
                .header("x-api-key", &self.api_key)
                .header("accept", "application/json");

            if let Some(v) = &body {
                req = req.json(v);
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    // A 404 proves this route did not perform the operation, so
                    // it is safe for every method to try the alternate prefix.
                    if status.as_u16() == 404 && has_alternate {
                        continue;
                    }

                    let text = response_text_limited(resp)
                        .await
                        .map_err(|e| format!("Failed reading API response: {e}"))?;
                    if !status.is_success() {
                        return Err(format!(
                            "API {method} {display_path} failed at {url} ({status}): {text}"
                        ));
                    }
                    if text.trim().is_empty() {
                        return Ok(json!({}));
                    }
                    return serde_json::from_str::<Value>(&text).map_err(|_| {
                        format!("API {method} {display_path} returned non-JSON response at {url}")
                    });
                }
                Err(e) => {
                    let mut detail = e.to_string();
                    let mut src = std::error::Error::source(&e);
                    while let Some(s) = src {
                        detail.push_str(" -> ");
                        detail.push_str(&s.to_string());
                        src = s.source();
                    }
                    let err = format!("API {method} {display_path} failed at {url}: {detail}");
                    // A GET is idempotent, so retain its existing compatibility
                    // fallback after transport failures. A write may have reached
                    // the first endpoint despite its client-side error.
                    if method == Method::GET && has_alternate {
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        Err(format!(
            "API {method} {display_path} has no request candidates"
        ))
    }

    pub async fn ping(&self) -> Result<(), String> {
        self.request_json(Method::GET, &["server", "ping"], None)
            .await?;
        Ok(())
    }

    pub async fn get_server_version(&self) -> Result<ServerVersion, String> {
        let value = self
            .request_json(Method::GET, &["server", "version"], None)
            .await?;
        serde_json::from_value(value).map_err(|e| format!("Failed parsing server version: {e}"))
    }

    pub async fn get_my_user(&self) -> Result<MeUser, String> {
        let value = self
            .request_json(Method::GET, &["users", "me"], None)
            .await?;
        serde_json::from_value(value).map_err(|e| format!("Failed parsing /users/me: {e}"))
    }

    pub async fn list_users(&self) -> Result<Vec<AlbumUser>, String> {
        let value = self.request_json(Method::GET, &["users"], None).await?;
        let raw = serde_json::from_value::<Vec<Value>>(value)
            .map_err(|e| format!("Failed parsing /users list: {e}"))?;

        let users = raw
            .into_iter()
            .filter_map(|item| {
                let id = item.get("id")?.as_str()?.to_string();
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("email").and_then(Value::as_str))
                    .unwrap_or("Immich User")
                    .to_string();
                let email = item
                    .get("email")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                Some(AlbumUser { id, name, email })
            })
            .collect();
        Ok(users)
    }

    pub async fn list_albums(&self, query: Option<&str>) -> Result<Vec<Album>, String> {
        let value = self.request_json(Method::GET, &["albums"], None).await?;
        let raw = serde_json::from_value::<Vec<Value>>(value)
            .map_err(|e| format!("Failed parsing /albums list: {e}"))?;
        let q = query.map(|v| v.to_lowercase());

        let mut albums = Vec::new();
        for item in raw {
            let id = match item.get("id").and_then(Value::as_str) {
                Some(v) => v.to_string(),
                None => continue,
            };
            let album_name = item
                .get("albumName")
                .and_then(Value::as_str)
                .unwrap_or("Untitled")
                .to_string();

            if let Some(ref filter) = q {
                if !album_name.to_lowercase().contains(filter) {
                    continue;
                }
            }

            let shared_with = item
                .get("albumUsers")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            let user = entry.get("user")?;
                            let uid = user.get("id")?.as_str()?.to_string();
                            let uname = user
                                .get("name")
                                .and_then(Value::as_str)
                                .or_else(|| user.get("email").and_then(Value::as_str))
                                .unwrap_or("Immich User")
                                .to_string();
                            let email = user
                                .get("email")
                                .and_then(Value::as_str)
                                .map(ToString::to_string);
                            Some(AlbumUser {
                                id: uid,
                                name: uname,
                                email,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            albums.push(Album {
                id,
                album_name,
                shared_with,
            });
        }
        Ok(albums)
    }

    pub async fn create_album(&self, name: &str) -> Result<Album, String> {
        let value = self
            .request_json(
                Method::POST,
                &["albums"],
                Some(json!({ "albumName": name })),
            )
            .await?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Album create response missing id".to_string())?
            .to_string();
        let album_name = value
            .get("albumName")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_string();

        Ok(Album {
            id,
            album_name,
            shared_with: Vec::new(),
        })
    }

    pub async fn share_album_users(
        &self,
        album_id: &str,
        user_ids: &[String],
        role: &str,
    ) -> Result<(), String> {
        // Only the two roles Immich accepts; reject anything else rather than
        // forwarding an arbitrary string as an authorization level.
        let role = match role {
            "viewer" | "editor" => role,
            other => return Err(format!("Invalid album share role: {other}")),
        };
        self.request_json(
            Method::PUT,
            // Each raw segment is percent-encoded independently, so the id
            // cannot introduce separators or traversal into the API path.
            &["albums", album_id, "users"],
            Some(json!({
                "albumUsers": user_ids.iter().map(|id| json!({"userId": id, "role": role})).collect::<Vec<_>>()
            })),
        )
        .await?;
        Ok(())
    }

    pub async fn create_share_link(
        &self,
        album_id: &str,
        public_server_url: &str,
    ) -> Result<AlbumShareLink, String> {
        let value = self
            .request_json(
                Method::POST,
                &["shared-links"],
                Some(share_link_payload(album_id)),
            )
            .await?;

        let key = value
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| "Share link response missing key".to_string())?;
        Ok(AlbumShareLink {
            url: share_link_url(public_server_url, key)?,
        })
    }

    /// Checks which of the given (id, sha1-hex-checksum) items already exist on
    /// the server. Returns the set of `id`s the server reports as duplicates,
    /// i.e. assets it already holds. Used to verify uploads before wiping the
    /// local source files.
    pub async fn bulk_upload_check(
        &self,
        items: &[(String, String)],
    ) -> Result<std::collections::HashSet<String>, String> {
        let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
        for chunk in items.chunks(500) {
            let assets: Vec<Value> = chunk
                .iter()
                .map(|(id, checksum)| json!({ "id": id, "checksum": checksum }))
                .collect();
            let value = self
                .request_json(
                    Method::POST,
                    &["assets", "bulk-upload-check"],
                    Some(json!({ "assets": assets })),
                )
                .await?;
            let results = value
                .get("results")
                .and_then(|r| r.as_array())
                .ok_or_else(|| "bulk-upload-check returned no results".to_string())?;
            present.extend(duplicates_from_results(results));
        }
        Ok(present)
    }
}

/// Asset ids the server reports as already-present duplicates. An asset counts
/// as confirmed-on-server ONLY when action=="reject" AND reason=="duplicate"
/// AND it is not trashed; any other reject reason is treated as NOT present.
/// This guards verify-before-wipe (wipe::verify_uploaded) so a local original is
/// never deleted unless the server actually holds a live identical copy.
///
/// `isTrashed` (Immich >= 1.115) is critical: bulk-upload-check matches a
/// checksum even when the server's only copy is soft-deleted, so treating a
/// trashed match as "present" would let us wipe the last live original and lose
/// it permanently once the server trash is emptied. Older servers omit the
/// field; there it defaults to false (unchanged, no-less-safe behavior).
fn duplicates_from_results(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .filter_map(|result| {
            let id = result.get("id").and_then(Value::as_str)?;
            let action = result.get("action").and_then(Value::as_str)?;
            let reason = result.get("reason").and_then(Value::as_str);
            let is_trashed = result
                .get("isTrashed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if action == "reject" && reason == Some("duplicate") && !is_trashed {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Payload for creating a public album share link. `showMetadata` is false so a
/// public link never leaks capture/location metadata by default; there is no UI
/// control to opt into exposure, so the private default is the only behavior.
fn share_link_payload(album_id: &str) -> Value {
    json!({
        "type": "ALBUM",
        "albumId": album_id,
        "allowUpload": false,
        "showMetadata": false
    })
}

/// Normalize a server base URL. Server-base queries and fragments are discarded
/// because API endpoints and public links must be rooted at the server path.
pub fn normalize_server_url(value: &str) -> String {
    let trimmed = value.trim();
    let Some(url) = server_base_url(trimmed) else {
        return trimmed.trim_end_matches('/').to_string();
    };

    let serialized = url.as_str();
    serialized
        .strip_suffix('/')
        .unwrap_or(serialized)
        .to_string()
}

/// Confirm a candidate endpoint is a reachable Immich server WITHOUT sending the
/// API key, by hitting the unauthenticated `/server/ping` endpoint and checking
/// for the `{"res":"pong"}` reply. Failover uses this so an upload (and the API
/// key) is never routed to an arbitrary service that merely holds the LAN/WAN
/// port open. Over plaintext HTTP a deliberate impersonator can still answer
/// this probe; that residual risk is inherent to the user's transport choice.
pub async fn probe_is_immich(server_url: &str) -> bool {
    let root = normalize_server_url(server_url);
    if root.is_empty() {
        return false;
    }
    let Ok(candidates) = api_endpoint_urls(&root, &["server", "ping"]) else {
        return false;
    };
    for url in candidates {
        let resp = HTTP
            .get(url)
            .header("accept", "application/json")
            // Short bound so failover stays snappy; covers connect + response.
            .timeout(Duration::from_millis(2000))
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(text) = response_text_limited(resp).await else {
            continue;
        };
        if serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("res").and_then(Value::as_str).map(|s| s == "pong"))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::normalize_server_url;

    #[test]
    fn normalizes_api_path_without_changing_authority() {
        for (input, expected) in [
            ("https://api", "https://api"),
            ("https://api/", "https://api"),
            ("https://host/api", "https://host"),
            ("https://host/api/", "https://host"),
            (
                "https://immich.example.com/api",
                "https://immich.example.com",
            ),
        ] {
            assert_eq!(normalize_server_url(input), expected);
        }
    }

    #[test]
    fn trims_trailing_slash() {
        assert_eq!(
            normalize_server_url("https://immich.example.com/"),
            "https://immich.example.com"
        );
    }

    #[test]
    fn share_link_uses_primary_server_url() {
        use super::share_link_url;

        assert_eq!(
            share_link_url("https://wan.example.com/api", "share-key").unwrap(),
            "https://wan.example.com/share/share-key"
        );
    }

    #[test]
    fn composes_api_and_share_paths_from_query_bearing_port_base() {
        use super::{api_endpoint_urls, share_link_url};

        let base = normalize_server_url("https://host:2283/api/?next=/");
        assert_eq!(base, "https://host:2283");

        for (path, api_url, bare_url) in [
            (
                "/albums",
                "https://host:2283/api/albums",
                "https://host:2283/albums",
            ),
            (
                "/shared-links",
                "https://host:2283/api/shared-links",
                "https://host:2283/shared-links",
            ),
            (
                "/server/ping",
                "https://host:2283/api/server/ping",
                "https://host:2283/server/ping",
            ),
        ] {
            let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
            let urls = api_endpoint_urls(&base, &segments)
                .unwrap()
                .into_iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>();
            assert_eq!(urls, [api_url, bare_url]);
        }
        assert_eq!(
            share_link_url("https://host:2283/api/?next=/", "share-key").unwrap(),
            "https://host:2283/share/share-key"
        );
    }

    #[test]
    fn only_duplicate_rejects_count_as_present() {
        use super::duplicates_from_results;
        use serde_json::json;
        let results = [
            json!({ "id": "a", "action": "reject", "reason": "duplicate" }),
            json!({ "id": "b", "action": "accept" }),
            json!({ "id": "c", "action": "reject", "reason": "unsupported" }),
            json!({ "id": "d", "action": "reject" }),
        ];
        // Only the duplicate-reason reject is treated as present on the server.
        assert_eq!(duplicates_from_results(&results), vec!["a".to_string()]);
    }

    #[test]
    fn trashed_duplicate_is_not_treated_as_present() {
        use super::duplicates_from_results;
        use serde_json::json;
        // A duplicate whose only server copy is trashed must NOT count as
        // present, or verify-before-wipe would delete the last live original.
        let results = [
            json!({ "id": "live", "action": "reject", "reason": "duplicate", "isTrashed": false }),
            json!({ "id": "trashed", "action": "reject", "reason": "duplicate", "isTrashed": true }),
        ];
        assert_eq!(duplicates_from_results(&results), vec!["live".to_string()]);
    }

    #[test]
    fn share_link_defaults_to_private_metadata() {
        use super::share_link_payload;
        let payload = share_link_payload("album-123");
        assert_eq!(payload["type"], "ALBUM");
        assert_eq!(payload["albumId"], "album-123");
        assert_eq!(payload["allowUpload"], false);
        // Public links must not expose capture/location metadata by default.
        assert_eq!(payload["showMetadata"], false);
    }
}
