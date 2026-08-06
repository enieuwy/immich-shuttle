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

/// Client for authenticated raw-byte fetches (profile images). Redirects are
/// refused outright: reqwest's cross-host filter strips only Authorization/
/// Cookie-class headers, so a 3xx would otherwise resend the custom
/// `x-api-key` header to whatever origin the server names. The image endpoint
/// never legitimately redirects, so a 3xx is treated as "no image".
static HTTP_NO_REDIRECT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| Client::new())
});

use crate::models::album::{Album, AlbumShareLink, AlbumUser};
use crate::models::tag::Tag;

/// Bound JSON reads so a malicious or misconfigured endpoint cannot make the app
/// buffer an unbounded response. The ceiling has to clear the largest legitimate
/// list this client fetches: `GET /albums` and `GET /users` return one object per
/// row including a nested owner, roughly a kilobyte each, so a 1 MiB cap failed
/// outright somewhere past a thousand albums. 16 MiB keeps the guard meaningful
/// while putting the limit far beyond any real library.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

async fn response_bytes_limited(
    mut response: Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if let Some(content_length) = response.content_length() {
        if content_length > max_bytes as u64 {
            return Err(format!("response exceeds the {max_bytes} byte limit"));
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
        if total > max_bytes {
            return Err(format!("response exceeds the {max_bytes} byte limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn response_text_limited(response: Response) -> Result<String, String> {
    let body = response_bytes_limited(response, MAX_RESPONSE_BYTES).await?;
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

/// Parse an Immich `UserResponseDto` into an `AlbumUser`. Shared by the user
/// list and the album `albumUsers[].user` entries so avatar metadata stays
/// consistent everywhere a person badge renders.
fn parse_album_user(user: &Value) -> Option<AlbumUser> {
    let id = user.get("id")?.as_str()?.to_string();
    let name = user
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| user.get("email").and_then(Value::as_str))
        .unwrap_or("Immich User")
        .to_string();
    let email = user
        .get("email")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let avatar_color = user
        .get("avatarColor")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let has_profile_image = user
        .get("profileImagePath")
        .and_then(Value::as_str)
        .is_some_and(|p| !p.is_empty());
    Some(AlbumUser {
        id,
        name,
        email,
        avatar_color,
        has_profile_image,
    })
}
/// Identify an image format from its magic bytes. Covers the formats Immich
/// stores as profile images; anything unrecognized is rejected rather than
/// guessed, because the result is embedded verbatim in a `data:` URL.
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
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

        let users = raw.iter().filter_map(parse_album_user).collect();
        Ok(users)
    }

    /// Fetch a user's profile image (avatar). `Ok(None)` means the user has no
    /// usable image: a 404, a redirect (refused — see HTTP_NO_REDIRECT), or a
    /// response that is not verifiably an image. Bytes are bounded so a
    /// misbehaving server cannot force an unbounded buffer; avatars are small
    /// crops, so 8 MiB is generous.
    pub async fn get_profile_image(
        &self,
        user_id: &str,
    ) -> Result<Option<(Vec<u8>, String)>, String> {
        const MAX_AVATAR_BYTES: usize = 8 * 1024 * 1024;
        let display_path = format!("/users/{user_id}/profile-image");
        let candidates = api_endpoint_urls(&self.server_url, &["users", user_id, "profile-image"])?;

        for (index, url) in candidates.iter().enumerate() {
            let has_alternate = index + 1 < candidates.len();
            match HTTP_NO_REDIRECT
                .get(url.clone())
                .header("x-api-key", &self.api_key)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    if status.as_u16() == 404 {
                        if has_alternate {
                            continue;
                        }
                        return Ok(None);
                    }
                    // A redirect here is a proxy quirk at best and a key-
                    // exfiltration attempt at worst; never follow it.
                    if status.is_redirection() {
                        return Ok(None);
                    }
                    if !status.is_success() {
                        return Err(format!("API GET {display_path} failed at {url} ({status})"));
                    }
                    let declared_mime = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.split(';').next())
                        .map(|v| v.trim().to_ascii_lowercase());
                    // An explicit non-image Content-Type (HTML error page,
                    // JSON from a mis-routed prefix) is not an avatar.
                    if declared_mime
                        .as_deref()
                        .is_some_and(|v| !v.starts_with("image/"))
                    {
                        return Ok(None);
                    }
                    let bytes = response_bytes_limited(resp, MAX_AVATAR_BYTES)
                        .await
                        .map_err(|e| format!("Failed reading profile image: {e}"))?;
                    // Only bytes that verifiably look like an image may flow
                    // into the data: URL the frontend renders; a declared
                    // image/* type without a matching signature is refused too.
                    let Some(mime) = sniff_image_mime(&bytes) else {
                        return Ok(None);
                    };
                    return Ok(Some((bytes, mime.to_string())));
                }
                Err(e) => {
                    if has_alternate {
                        continue;
                    }
                    return Err(format!("API GET {display_path} failed at {url}: {e}"));
                }
            }
        }
        Ok(None)
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
                        .filter_map(|entry| parse_album_user(entry.get("user")?))
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

    pub async fn list_tags(&self) -> Result<Vec<Tag>, String> {
        let value = self.request_json(Method::GET, &["tags"], None).await?;
        let raw = serde_json::from_value::<Vec<Value>>(value)
            .map_err(|e| format!("Failed parsing /tags list: {e}"))?;

        let mut tags = Vec::new();
        for item in raw {
            let id = match item.get("id").and_then(Value::as_str) {
                Some(v) => v.to_string(),
                None => continue,
            };
            // `value` is the full hierarchical path ("Parent/Child"); fall back
            // to `name` (leaf) if a server omits it.
            let value = item
                .get("value")
                .and_then(Value::as_str)
                .or_else(|| item.get("name").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            if value.is_empty() {
                continue;
            }
            tags.push(Tag { id, value });
        }
        Ok(tags)
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

    /// Checks which of `checksums` the server already holds, returning the
    /// POSITIONS within the slice that it reports as duplicates.
    ///
    /// The wire `id` is only a correlation handle — the server echoes it back so
    /// each result can be paired with the row that produced it, and nothing else
    /// reads it. It is therefore the request index, never the file's path. A path
    /// would ship the user's account name, volume labels, and directory layout to
    /// whatever endpoint the active profile resolves to (including the WAN
    /// failover target), which is strictly more than uploading the photo itself
    /// discloses: immich-go identifies an asset as `<filename>-<size>` and sends
    /// no directory structure at all.
    pub async fn bulk_upload_check(
        &self,
        checksums: &[String],
    ) -> Result<std::collections::HashSet<usize>, String> {
        let mut present: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (chunk_index, chunk) in checksums.chunks(BULK_UPLOAD_CHECK_CHUNK).enumerate() {
            let offset = chunk_index * BULK_UPLOAD_CHECK_CHUNK;
            let value = self
                .request_json(
                    Method::POST,
                    &["assets", "bulk-upload-check"],
                    Some(bulk_upload_check_payload(offset, chunk)),
                )
                .await?;
            let results = value
                .get("results")
                .and_then(|r| r.as_array())
                .ok_or_else(|| "bulk-upload-check returned no results".to_string())?;
            // An id we cannot parse is one we never issued, so ignore it. Failing
            // to recognise a duplicate only means a file is treated as NOT on the
            // server, which keeps the original — the safe direction.
            present.extend(
                duplicates_from_results(results)
                    .iter()
                    .filter_map(|id| id.parse::<usize>().ok()),
            );
        }
        Ok(present)
    }
}

/// Immich rejects very large batches, so checks are chunked.
const BULK_UPLOAD_CHECK_CHUNK: usize = 500;

/// Builds one bulk-upload-check request body. Split out so the wire shape can be
/// asserted without a live server — specifically that it carries nothing but an
/// index and a checksum.
fn bulk_upload_check_payload(offset: usize, chunk: &[String]) -> Value {
    let assets: Vec<Value> = chunk
        .iter()
        .enumerate()
        .map(|(i, checksum)| json!({ "id": (offset + i).to_string(), "checksum": checksum }))
        .collect();
    json!({ "assets": assets })
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
    fn sniffs_only_known_image_signatures() {
        use super::sniff_image_mime;
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_image_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00]),
            Some("image/png")
        );
        assert_eq!(sniff_image_mime(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            sniff_image_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        // HTML/JSON error bodies and truncated headers must be refused — they
        // would otherwise be embedded into a data: URL as a fake JPEG.
        assert_eq!(sniff_image_mime(b"<!doctype html>"), None);
        assert_eq!(sniff_image_mime(b"{\"error\":\"x\"}"), None);
        assert_eq!(sniff_image_mime(b"RIFF\x00\x00\x00\x00WAVE"), None);
        assert_eq!(sniff_image_mime(&[]), None);
    }

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

    /// The bulk-upload-check id is a correlation handle, not an identifier the
    /// server is entitled to. Uploading a photo tells the server its filename and
    /// size; nothing about this duplicate check requires also handing over the
    /// account name, the volume label, or the folder tree the photo came from.
    #[test]
    fn bulk_upload_check_body_carries_no_local_path() {
        use super::bulk_upload_check_payload;

        let checksums = vec![
            "da39a3ee5e6b4b0d3255bfef95601890afd80709".to_string(),
            "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".to_string(),
        ];
        let body = bulk_upload_check_payload(0, &checksums).to_string();

        for leak in [
            "/Users/",
            "/Volumes/",
            "C:\\",
            "DCIM",
            "Pictures",
            ".JPG",
            "IMG_",
        ] {
            assert!(!body.contains(leak), "request body leaked {leak}: {body}");
        }
        assert_eq!(
            body,
            r#"{"assets":[{"checksum":"da39a3ee5e6b4b0d3255bfef95601890afd80709","id":"0"},{"checksum":"aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d","id":"1"}]}"#
        );
    }

    /// Chunk boundaries must keep producing request-wide positions, or the second
    /// batch's results would be paired back to the first batch's files — on the
    /// wipe path, deleting an original whose upload was never confirmed.
    #[test]
    fn bulk_upload_check_ids_are_request_wide_positions() {
        use super::bulk_upload_check_payload;

        let body = bulk_upload_check_payload(500, &["sum".to_string()]);
        assert_eq!(body["assets"][0]["id"], "500");
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
