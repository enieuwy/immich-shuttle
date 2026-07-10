use std::{sync::LazyLock, time::Duration};

use reqwest::{Client, Method};
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

/// Percent-encode a value for use as a single URL path segment. Everything
/// outside RFC 3986 unreserved characters is escaped — crucially `/`, so an id
/// can never introduce additional path segments or `../` traversal into an
/// authenticated request path.
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let root = self.server_url.trim_end_matches('/');
        let candidates = if root.ends_with("/api") {
            vec![format!("{root}{path}")]
        } else {
            vec![format!("{root}{path}"), format!("{root}/api{path}")]
        };

        let mut last_err = String::from("Unknown request error");
        for url in candidates {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("x-api-key", &self.api_key)
                .header("accept", "application/json");

            if let Some(v) = &body {
                req = req.json(v);
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| format!("Failed reading API response: {e}"))?;
                    if !status.is_success() {
                        let err = format!("API {method} {path} failed at {url} ({status}): {text}");
                        // Only a 404 justifies trying the alternate `/api` path
                        // variant. Any other status (401/403/5xx) is authoritative
                        // for this endpoint, so surface it instead of letting a
                        // later candidate's 404 mask e.g. an expired API key.
                        if status.as_u16() != 404 {
                            return Err(err);
                        }
                        last_err = err;
                        continue;
                    }
                    if text.trim().is_empty() {
                        return Ok(json!({}));
                    }
                    match serde_json::from_str::<Value>(&text) {
                        Ok(v) => return Ok(v),
                        Err(_) => {
                            last_err =
                                format!("API {method} {path} returned non-JSON response at {url}");
                        }
                    }
                }
                Err(e) => {
                    let mut detail = e.to_string();
                    let mut src = std::error::Error::source(&e);
                    while let Some(s) = src {
                        detail.push_str(" -> ");
                        detail.push_str(&s.to_string());
                        src = s.source();
                    }
                    last_err = format!("API {method} {path} failed at {url}: {detail}");
                }
            }
        }

        Err(last_err)
    }

    pub async fn ping(&self) -> Result<(), String> {
        self.request_json(Method::GET, "/server/ping", None).await?;
        Ok(())
    }

    pub async fn get_server_version(&self) -> Result<ServerVersion, String> {
        let value = self
            .request_json(Method::GET, "/server/version", None)
            .await?;
        serde_json::from_value(value).map_err(|e| format!("Failed parsing server version: {e}"))
    }

    pub async fn get_my_user(&self) -> Result<MeUser, String> {
        let value = self.request_json(Method::GET, "/users/me", None).await?;
        serde_json::from_value(value).map_err(|e| format!("Failed parsing /users/me: {e}"))
    }

    pub async fn list_users(&self) -> Result<Vec<AlbumUser>, String> {
        let value = self.request_json(Method::GET, "/users", None).await?;
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
        let value = self.request_json(Method::GET, "/albums", None).await?;
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
            .request_json(Method::POST, "/albums", Some(json!({ "albumName": name })))
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
            // Percent-encode the id so it can't smuggle extra path segments
            // (e.g. `../`) into the authenticated request path.
            &format!("/albums/{}/users", encode_path_segment(album_id)),
            Some(json!({
                "albumUsers": user_ids.iter().map(|id| json!({"userId": id, "role": role})).collect::<Vec<_>>()
            })),
        )
        .await?;
        Ok(())
    }

    pub async fn create_share_link(&self, album_id: &str) -> Result<AlbumShareLink, String> {
        let value = self
            .request_json(
                Method::POST,
                "/shared-links",
                Some(json!({
                    "type": "ALBUM",
                    "albumId": album_id,
                    "allowUpload": false,
                    "showMetadata": true
                })),
            )
            .await?;

        let key = value
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| "Share link response missing key".to_string())?;
        Ok(AlbumShareLink {
            url: format!("{}/share/{key}", self.server_url.trim_end_matches('/')),
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
                    "/assets/bulk-upload-check",
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
/// as confirmed-on-server ONLY when action=="reject" AND reason=="duplicate";
/// any other reject reason is treated as NOT present. This guards
/// verify-before-wipe (wipe::verify_uploaded) so a local original is never
/// deleted unless the server actually holds an identical copy.
fn duplicates_from_results(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .filter_map(|result| {
            let id = result.get("id").and_then(Value::as_str)?;
            let action = result.get("action").and_then(Value::as_str)?;
            let reason = result.get("reason").and_then(Value::as_str);
            if action == "reject" && reason == Some("duplicate") {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect()
}

pub fn normalize_server_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if let Some(root) = trimmed.strip_suffix("/api") {
        root.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_server_url;

    #[test]
    fn strips_trailing_api_segment() {
        assert_eq!(
            normalize_server_url("https://immich.example.com/api"),
            "https://immich.example.com"
        );
    }

    #[test]
    fn trims_trailing_slash() {
        assert_eq!(
            normalize_server_url("https://immich.example.com/"),
            "https://immich.example.com"
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
    fn encode_path_segment_escapes_separators_and_traversal() {
        use super::encode_path_segment;
        // A normal UUID is untouched.
        assert_eq!(
            encode_path_segment("6b2f1c4e-0000-4a1b-9c3d-abcdef012345"),
            "6b2f1c4e-0000-4a1b-9c3d-abcdef012345"
        );
        // Slashes (segment separators) are escaped, so no extra path can be added.
        assert_eq!(encode_path_segment("../admin"), "..%2Fadmin");
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert!(!encode_path_segment("x/../y").contains('/'));
    }
}
