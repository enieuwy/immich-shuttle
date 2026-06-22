use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::models::album::{Album, AlbumShareLink, AlbumUser};

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
            http: Client::new(),
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
                        last_err =
                            format!("API {method} {path} failed at {url} ({status}): {text}");
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
                    last_err = format!("API {method} {path} failed at {url}: {e}");
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
    ) -> Result<(), String> {
        self.request_json(
            Method::PUT,
            &format!("/albums/{album_id}/users"),
            Some(json!({
                "albumUsers": user_ids.iter().map(|id| json!({"userId": id, "role": "editor"})).collect::<Vec<_>>()
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

    pub async fn add_assets_to_album(
        &self,
        album_id: &str,
        asset_ids: &[String],
    ) -> Result<(), String> {
        if asset_ids.is_empty() {
            return Ok(());
        }
        self.request_json(
            Method::PUT,
            &format!("/albums/{album_id}/assets"),
            Some(json!({ "ids": asset_ids })),
        )
        .await?;
        Ok(())
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
            for result in results {
                let id = result.get("id").and_then(|v| v.as_str());
                let action = result.get("action").and_then(|v| v.as_str());
                if let (Some(id), Some("reject")) = (id, action) {
                    // action=reject (reason=duplicate) => the server already has it.
                    present.insert(id.to_string());
                }
            }
        }
        Ok(present)
    }
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
}
