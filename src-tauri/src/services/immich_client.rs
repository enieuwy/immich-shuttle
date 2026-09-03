use std::{collections::HashSet, ops::Range, sync::LazyLock, time::Duration};

use reqwest::{Client, Method, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Prefix for a transport-level failure — DNS, TCP, or TLS — as opposed to an
/// HTTP error from a server that did answer.
///
/// It is both the user-facing opening of the message and the marker the frontend
/// matches to decide whether retrying is worthwhile (`albums.ts`). Before this
/// existed the frontend pattern-matched reqwest's own prose ("error sending
/// request", "tcp connect"), which any dependency bump could reword. Only
/// `request_json` marks its errors this way; that is the path the album loader
/// retries on.
pub const UNREACHABLE_ERROR: &str = "Could not reach the server";

/// Follow redirects only when the scheme, host, and effective port stay fixed.
/// Reqwest does not strip the custom `x-api-key` header when an origin changes.
fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    let limited = reqwest::redirect::Policy::limited(10);
    reqwest::redirect::Policy::custom(move |attempt| {
        let same_origin = attempt.previous().last().is_some_and(|previous| {
            previous.scheme() == attempt.url().scheme()
                && previous.host_str() == attempt.url().host_str()
                && previous.port_or_known_default() == attempt.url().port_or_known_default()
        });
        if same_origin {
            limited.redirect(attempt)
        } else {
            attempt.stop()
        }
    })
}

/// One shared HTTP client (connection pool + TLS config) reused across every
/// request. Building a fresh `Client` per call is wasteful and was a likely
/// source of flaky "error sending request" failures during the startup burst.
/// Same-origin redirects remain supported. An origin change returns its 3xx
/// response without sending the authenticated follow-up request.
///
/// Keep the build error instead of falling back to reqwest's default
/// constructor: that fallback can panic for the same TLS or resolver failure
/// that rejected the configured client, poisoning this process-wide lazy value.
static HTTP: LazyLock<Result<Client, String>> = LazyLock::new(|| {
    Client::builder()
        .redirect(same_origin_redirect_policy())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("Failed to build shared Immich HTTP client: {error}"))
});

/// Client for authenticated raw-byte fetches (profile images). Redirects are
/// refused outright: reqwest's cross-host filter strips only Authorization/
/// Cookie-class headers, so a 3xx would otherwise resend the custom
/// `x-api-key` header to whatever origin the server names. The image endpoint
/// never legitimately redirects, so a 3xx is treated as "no image".
static HTTP_NO_REDIRECT: LazyLock<Result<Client, String>> = LazyLock::new(|| {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("Failed to build shared no-redirect Immich HTTP client: {error}"))
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

/// Longest server-supplied response body quoted back in an HTTP-failure
/// message. `MAX_RESPONSE_BYTES` (16 MiB) bounds what this client will BUFFER,
/// which a success path legitimately needs, but an error string travels much
/// further: it becomes `job.error`, crosses IPC to the queue card, and is
/// written to app.log as a single line. A body that large is not diagnostic
/// anyway — the first few hundred characters carry the status page or JSON
/// error object, and the rest is padding or an HTML page — so quote only that
/// much, in the same spirit as `MAX_ECHOED_ID_CHARS`.
const MAX_ERROR_BODY_CHARS: usize = 400;

/// Bound a server-controlled response body for inclusion in an error message.
/// The excerpt is cut on a character boundary and names how much was dropped,
/// so a truncated message still says the server answered at length.
fn error_body_excerpt(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(MAX_ERROR_BODY_CHARS) {
        None => trimmed.to_string(),
        Some((cut, _)) => format!(
            "{}… (truncated, {} more bytes)",
            &trimmed[..cut],
            trimmed.len() - cut
        ),
    }
}

/// Immich server bases identify an origin (optionally behind a path-prefix), not
/// a resource. Discard a query and fragment so they cannot be inherited by API
/// requests or public share links.
///
/// Userinfo is discarded for the same reason and one more: `Url`'s `Display`
/// prints `user:pass@host`, and every API error string interpolates the URL
/// before it reaches app.log and the job card. Stripping the credentials here
/// means no downstream formatter can leak them. This client authenticates with
/// the `x-api-key` header, so URL credentials were never a supported mechanism.
/// `set_username`/`set_password` only fail for a cannot-be-a-base URL, which by
/// construction has no authority to carry userinfo, so the error is moot.
fn server_base_url(value: &str) -> Option<Url> {
    let mut url = Url::parse(value.trim()).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);

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

pub fn server_compatibility(version: &ServerVersion) -> (bool, Option<String>) {
    const MIN_SUPPORTED_VERSION: (i64, i64, i64) = (1, 106, 0);
    const WARNING: &str =
        "Immich server version may be below the minimum supported by bundled immich-go.";

    let is_compatible = (version.major, version.minor, version.patch) >= MIN_SUPPORTED_VERSION;
    let warning = (!is_compatible).then(|| WARNING.to_string());
    (is_compatible, warning)
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
    http: Result<Client, String>,
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
        let http = self.http.as_ref().map_err(|error| error.clone())?;

        for (index, url) in candidates.iter().enumerate() {
            let has_alternate = index + 1 < candidates.len();
            let mut req = http
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
                        let body = error_body_excerpt(&text);
                        return Err(format!(
                            "API {method} {display_path} failed at {url} ({status}): {body}"
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
                    let err = format!(
                        "{UNREACHABLE_ERROR}: API {method} {display_path} at {url}: {detail}"
                    );
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
        let http = HTTP_NO_REDIRECT.as_ref().map_err(|error| error.clone())?;

        for (index, url) in candidates.iter().enumerate() {
            let has_alternate = index + 1 < candidates.len();
            match http
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
    /// POSITIONS within the slice it reports as duplicates, split by whether the
    /// server also proved its copy is live (see `BulkUploadCheck`).
    ///
    /// The wire `id` is only a correlation handle — the server echoes it back so
    /// each result can be paired with the row that produced it, and nothing else
    /// reads it. It is therefore the request index, never the file's path. A path
    /// would ship the user's account name, volume labels, and directory layout to
    /// whatever endpoint the active profile resolves to (including the WAN
    /// failover target), which is strictly more than uploading the photo itself
    /// discloses: immich-go identifies an asset as `<filename>-<size>` and sends
    /// no directory structure at all.
    pub async fn bulk_upload_check(&self, checksums: &[String]) -> Result<BulkUploadCheck, String> {
        let mut check = BulkUploadCheck::default();
        for (chunk_index, chunk) in checksums.chunks(BULK_UPLOAD_CHECK_CHUNK).enumerate() {
            // `chunks` yields full-size batches until the last one, so this
            // batch starts at exactly `chunk_index * CHUNK` and covers
            // `chunk.len()` consecutive request-wide indexes from there. That
            // half-open range is the complete set of ids this request issued,
            // and therefore the only ids its response may echo back.
            let start = chunk_index * BULK_UPLOAD_CHECK_CHUNK;
            let requested = start..start + chunk.len();
            let value = self
                .request_json(
                    Method::POST,
                    &["assets", "bulk-upload-check"],
                    Some(bulk_upload_check_payload(start, chunk)),
                )
                .await?;
            let results = value
                .get("results")
                .and_then(|r| r.as_array())
                .ok_or_else(|| "bulk-upload-check returned no results".to_string())?;
            merge_checked_results(results, &requested, &mut check)?;
        }
        Ok(check)
    }
}

/// Immich rejects very large batches, so checks are chunked.
const BULK_UPLOAD_CHECK_CHUNK: usize = 500;

/// Longest server-echoed id repeated back in a protocol-violation message. The
/// text is server-controlled and lands in app.log and the job card, so keep
/// enough to diagnose the mispairing without letting a server flood the log.
const MAX_ECHOED_ID_CHARS: usize = 32;

/// Pair one bulk-upload-check result back to the row that produced it.
///
/// The echoed `id` is the ONLY link between a result and a local file, and a
/// confirmed row is what authorizes deleting that file's original
/// (wipe::verify_uploaded). So an id is accepted only when it is an index
/// inside `requested`, the exact half-open range this one request carried. An
/// index belonging to a different chunk would otherwise graft this response's
/// answer onto an unrelated photo whose checksum the server never confirmed.
///
/// A violation fails the whole sweep rather than skipping the offending row: a
/// server that mispairs one index has disproved its pairing of every other id
/// in the same response, so the remaining answers are not evidence either.
/// Both callers propagate the error, so a failed sweep deletes nothing.
fn checked_result_index(id: &str, requested: &Range<usize>) -> Result<usize, String> {
    match id.parse::<usize>() {
        Ok(index) if requested.contains(&index) => Ok(index),
        _ => {
            let shown: String = id.chars().take(MAX_ECHOED_ID_CHARS).collect();
            Err(format!(
                "bulk-upload-check protocol violation: server echoed id {shown:?} for a request carrying indexes {}..{}",
                requested.start, requested.end
            ))
        }
    }
}

/// Fold one response's duplicate rows into the running sweep, rejecting a
/// response that answers the same request row more than once.
///
/// Set-wise insertion made a repeated index silently resolve to the most
/// permissive of its rows: a response carrying `isTrashed: false` for index 0
/// and, later, `isTrashed: true` (or an absent field) for the same index left 0
/// in `confirmed_live`, so verify-before-wipe (wipe::verify_uploaded) read a
/// self-contradiction as proof that the local original was safely uploaded and
/// could trash it. A repeat is therefore treated exactly like an out-of-range
/// id: the whole sweep fails and nothing is deleted.
///
/// The rejection covers two rows that AGREE as well. A server that duplicates a
/// row has already disproved its one-answer-per-row pairing, and agreement is
/// not evidence that the duplication was harmless — the deliberate choice is to
/// fail closed and keep the user's files.
///
/// Indexes are compared PARSED, never as strings: `"0"` and `"00"` are two
/// different strings naming one request row.
///
/// Nothing is recorded until every row of the response has been validated, so a
/// violating response contributes no partial evidence to the sweep.
fn merge_checked_results(
    results: &[Value],
    requested: &Range<usize>,
    check: &mut BulkUploadCheck,
) -> Result<(), String> {
    // Scan EVERY duplicate-reject row, not only the rows the liveness read
    // keeps. The contradicting row is precisely the trashed or
    // unknown-status one that read drops, so a guard placed after it would
    // never see the conflict. Range-checking those rows too is the same
    // fail-closed policy: a trashed answer for an index this request never
    // issued is a mispairing whatever its liveness says.
    let mut answered: HashSet<usize> = HashSet::with_capacity(results.len());
    for result in results {
        let Some(id) = duplicate_reject_id(result) else {
            continue;
        };
        let index = checked_result_index(id, requested)?;
        if !answered.insert(index) {
            let shown: String = id.chars().take(MAX_ECHOED_ID_CHARS).collect();
            return Err(format!(
                "bulk-upload-check protocol violation: server echoed id {shown:?} for index {index} twice in one response for a request carrying indexes {}..{}",
                requested.start, requested.end
            ));
        }
    }

    // Every id below already passed the range and repeat checks above, so this
    // pass only records evidence.
    for (id, copy) in duplicates_from_results(results) {
        let index = checked_result_index(id, requested)?;
        check.duplicates.insert(index);
        if copy == ServerCopy::Live {
            check.confirmed_live.insert(index);
        }
    }
    Ok(())
}

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

/// One bulk-upload-check sweep, split into the two different questions its
/// callers ask. "Does the server already hold this file?" and "may we delete
/// the local original?" are not the same question, because the server can match
/// a checksum without telling us whether its copy is live.
#[derive(Debug, Default, Clone)]
pub struct BulkUploadCheck {
    /// Indices the server matched AND explicitly reported live. ONLY these may
    /// authorize deleting a local original (wipe::verify_uploaded).
    pub confirmed_live: HashSet<usize>,
    /// Indices the server matched without reporting a trashed copy — live plus
    /// unknown-status. Read-only forecasting uses this: it deletes nothing, so
    /// counting an unprovable match as a duplicate costs only accuracy.
    pub duplicates: HashSet<usize>,
}

/// What a duplicate match proves about the liveness of the server's copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerCopy {
    /// The server stated `isTrashed: false`.
    Live,
    /// The server did not state the copy is live. Never wipe-confirmable.
    Unknown,
}

/// Asset ids the server reports as already-present duplicates, each paired with
/// what the response proves about that copy. An id is reported ONLY when
/// action=="reject" AND reason=="duplicate"; any other reject reason is treated
/// as NOT present. This guards verify-before-wipe (wipe::verify_uploaded) so a
/// local original is never deleted unless the server actually holds a live
/// identical copy.
///
/// `isTrashed` is critical: bulk-upload-check matches a checksum even when the
/// server's only copy is soft-deleted, so treating a trashed match as "present"
/// would let us wipe the last live original and lose it permanently once the
/// server trash is emptied. The field is OPTIONAL in the Immich API
/// (`AssetBulkUploadCheckResult` lists it outside `required`), so an absent,
/// null, or non-boolean value means unknown — never live. Unknown is dropped
/// here rather than defaulting to false, because a default of false would be a
/// fabricated proof of liveness and this codebase's compatibility floor (1.106)
/// predates the field entirely.
fn duplicates_from_results(results: &[Value]) -> Vec<(&str, ServerCopy)> {
    results
        .iter()
        .filter_map(|result| {
            let id = duplicate_reject_id(result)?;
            match result.get("isTrashed").and_then(Value::as_bool) {
                Some(true) => None,
                Some(false) => Some((id, ServerCopy::Live)),
                None => Some((id, ServerCopy::Unknown)),
            }
        })
        .collect()
}

/// The echoed id of a row the server rejected as an already-present duplicate,
/// whatever that row says about liveness.
///
/// Shared by the liveness read above and the repeated-index guard in
/// `merge_checked_results` so both agree on exactly which rows are answers
/// about a local file. The guard has to see the rows the read discards, because
/// a trashed row is both discarded and the row that contradicts a live one.
fn duplicate_reject_id(result: &Value) -> Option<&str> {
    let id = result.get("id").and_then(Value::as_str)?;
    let action = result.get("action").and_then(Value::as_str)?;
    let reason = result.get("reason").and_then(Value::as_str);
    (action == "reject" && reason == Some("duplicate")).then_some(id)
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
    let Ok(http) = HTTP.as_ref() else {
        return false;
    };

    for url in candidates {
        let resp = http
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
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::{normalize_server_url, server_compatibility, ImmichClient, ServerVersion};

    struct HttpStub {
        url: String,
        requests: mpsc::UnboundedReceiver<String>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for HttpStub {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    async fn spawn_http_stub(responder: impl Fn(&str) -> String + Send + 'static) -> HttpStub {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let addr = listener.local_addr().expect("stub address");
        let (requests_tx, requests) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                // Read the headers, then the body the request declares. A stub
                // that answers and closes while a POST body is still unread can
                // reach the client as a connection reset instead of a response,
                // which would make the chunked bulk-upload-check sweep flaky.
                let mut head_len = None;
                while let Ok(read) = socket.read(&mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if head_len.is_none() {
                        head_len = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .map(|offset| offset + 4);
                    }
                    let Some(head_len) = head_len else { continue };
                    let head = String::from_utf8_lossy(&request[..head_len]).into_owned();
                    let body_len = request_header(&head, "content-length")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= head_len + body_len {
                        break;
                    }
                }

                let request = String::from_utf8_lossy(&request).into_owned();
                let response = responder(&request);
                let _ = requests_tx.send(request);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        HttpStub {
            url: format!("http://127.0.0.1:{}", addr.port()),
            requests,
            handle,
        }
    }

    fn http_response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let headers = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect::<String>();
        format!(
            "HTTP/1.1 {status}\r\n{headers}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().skip(1).find_map(|line| {
            let (header_name, value) = line.split_once(':')?;
            header_name.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    async fn next_request(stub: &mut HttpStub) -> String {
        tokio::time::timeout(Duration::from_secs(1), stub.requests.recv())
            .await
            .expect("stub received a request before timeout")
            .expect("stub request channel stayed open")
    }

    #[tokio::test]
    async fn authenticated_json_redirects_follow_only_the_same_origin() {
        const API_KEY: &str = "redirect-test-api-key";

        let mut same_origin = spawn_http_stub(|request| {
            if request.starts_with("GET /api/server/ping ") {
                http_response("302 Found", &[("location", "/redirected")], "")
            } else {
                http_response("200 OK", &[], r#"{"res":"pong"}"#)
            }
        })
        .await;
        let client = ImmichClient::new(&same_origin.url, API_KEY);

        client
            .ping()
            .await
            .expect("same-origin redirect must remain supported");
        for expected_path in ["/api/server/ping", "/redirected"] {
            let request = next_request(&mut same_origin).await;
            assert!(
                request.starts_with(&format!("GET {expected_path} ")),
                "unexpected request: {request}"
            );
            assert_eq!(request_header(&request, "x-api-key"), Some(API_KEY));
        }

        let mut cross_origin_target =
            spawn_http_stub(|_| http_response("200 OK", &[], r#"{"res":"pong"}"#)).await;
        let target_url = cross_origin_target.url.clone();
        let cross_origin_source =
            spawn_http_stub(move |_| http_response("302 Found", &[("location", &target_url)], ""))
                .await;
        let client = ImmichClient::new(&cross_origin_source.url, API_KEY);

        let error = client
            .ping()
            .await
            .expect_err("cross-origin redirect must be refused");
        assert!(error.contains("302"), "unexpected redirect error: {error}");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                cross_origin_target.requests.recv()
            )
            .await
            .is_err(),
            "the client followed the cross-origin redirect"
        );
    }

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
    fn server_compatibility_uses_minimum_version_boundary() {
        let warning = Some(
            "Immich server version may be below the minimum supported by bundled immich-go."
                .to_string(),
        );

        assert_eq!(
            server_compatibility(&ServerVersion {
                major: 1,
                minor: 105,
                patch: 99,
            }),
            (false, warning.clone())
        );
        assert_eq!(
            server_compatibility(&ServerVersion {
                major: 1,
                minor: 106,
                patch: 0,
            }),
            (true, None)
        );
        assert_eq!(
            server_compatibility(&ServerVersion {
                major: 1,
                minor: 106,
                patch: 1,
            }),
            (true, None)
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
        use super::{duplicates_from_results, ServerCopy};
        use serde_json::json;
        let results = [
            json!({ "id": "a", "action": "reject", "reason": "duplicate" }),
            json!({ "id": "b", "action": "accept" }),
            json!({ "id": "c", "action": "reject", "reason": "unsupported" }),
            json!({ "id": "d", "action": "reject" }),
        ];
        // Only the duplicate-reason reject is treated as present on the server;
        // it states no trash status, so its liveness is unknown.
        assert_eq!(
            duplicates_from_results(&results),
            vec![("a", ServerCopy::Unknown)]
        );
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

    /// The echoed id is the only link between a result and a local file, so the
    /// range check must accept exactly the indexes the request issued — no more,
    /// and no fewer. `600` and `499` bracket the second chunk's range, and a
    /// non-numeric id is an id this client never sent at all.
    #[test]
    fn checked_result_index_accepts_only_the_requested_range() {
        use super::checked_result_index;

        assert_eq!(checked_result_index("500", &(500..600)), Ok(500));
        assert_eq!(checked_result_index("599", &(500..600)), Ok(599));
        for id in ["499", "600", "0", "-1", "1e3", "", "not-an-index"] {
            let error = checked_result_index(id, &(500..600))
                .expect_err("an id outside 500..600 was never issued by that request");
            assert!(
                error.contains("protocol violation") && error.contains("500..600"),
                "unexpected error for id {id:?}: {error}"
            );
        }
        // The id is server-controlled text on its way to app.log, so it is
        // bounded before it is quoted back.
        let error = checked_result_index(&"9".repeat(4096), &(0..1))
            .expect_err("an over-long id is not an index");
        assert!(error.len() < 200, "echoed id was not bounded: {error}");
    }

    fn chunked_check_checksums() -> Vec<String> {
        // 600 rows split into chunks of 500, so the second request carries
        // indexes 500..600 and the offset arithmetic is actually exercised.
        (0..600).map(|row| format!("{row:040x}")).collect()
    }

    /// A result carrying an index from a DIFFERENT chunk is a mispairing, and
    /// `confirmed_live` is what authorizes deleting a local original. The whole
    /// sweep fails rather than skipping the row: a server that mispairs one
    /// index has disproved its pairing of the rest of the response too.
    #[tokio::test]
    async fn bulk_upload_check_rejects_an_index_from_another_chunk() {
        let stub = spawn_http_stub(|request| {
            // Only the second chunk's body mentions index 500.
            let echoed = if request.contains(r#""id":"500""#) {
                "0"
            } else {
                "1"
            };
            http_response(
                "200 OK",
                &[],
                &format!(
                    r#"{{"results":[{{"id":"{echoed}","action":"reject","reason":"duplicate","isTrashed":false}}]}}"#
                ),
            )
        })
        .await;

        let error = ImmichClient::new(&stub.url, "range-test-api-key")
            .bulk_upload_check(&chunked_check_checksums())
            .await
            .expect_err("a cross-chunk index must fail the check");
        assert!(
            error.contains("protocol violation") && error.contains("500..600"),
            "unexpected error: {error}"
        );
    }

    /// The mirror of the rejection: the second chunk's own indexes start at 500,
    /// so a range anchored at zero (or one sized to the first chunk) would drop
    /// every later confirmation and silently stop wiping uploaded originals.
    #[tokio::test]
    async fn bulk_upload_check_confirms_in_range_indexes_from_every_chunk() {
        let stub = spawn_http_stub(|request| {
            let echoed: &[&str] = if request.contains(r#""id":"500""#) {
                &["500", "599"]
            } else {
                &["0", "499"]
            };
            let results = echoed
                .iter()
                .map(|id| {
                    format!(
                        r#"{{"id":"{id}","action":"reject","reason":"duplicate","isTrashed":false}}"#
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            http_response("200 OK", &[], &format!(r#"{{"results":[{results}]}}"#))
        })
        .await;

        let check = ImmichClient::new(&stub.url, "range-test-api-key")
            .bulk_upload_check(&chunked_check_checksums())
            .await
            .expect("every echoed index is inside the chunk that requested it");
        let mut confirmed = check.confirmed_live.into_iter().collect::<Vec<_>>();
        confirmed.sort_unstable();
        assert_eq!(confirmed, [0, 499, 500, 599]);
        let mut duplicates = check.duplicates.into_iter().collect::<Vec<_>>();
        duplicates.sort_unstable();
        assert_eq!(duplicates, [0, 499, 500, 599]);
    }

    /// Two rows answering the SAME request index are two contradictory answers
    /// about one local file, and set-wise insertion let the permissive one win:
    /// a live row plus a trashed row for index 0 used to leave 0 in
    /// `confirmed_live`, so verify-before-wipe would trash the local original on
    /// contradictory evidence. `"0"` and `"00"` are different strings naming the
    /// same parsed index, which is why the guard compares parsed indexes.
    #[tokio::test]
    async fn bulk_upload_check_rejects_a_repeated_index_with_conflicting_liveness() {
        const RESULTS: &str = r#"{"results":[{"id":"0","action":"reject","reason":"duplicate","isTrashed":false},{"id":"00","action":"reject","reason":"duplicate","isTrashed":true}]}"#;

        let stub = spawn_http_stub(|_| http_response("200 OK", &[], RESULTS)).await;
        let error = ImmichClient::new(&stub.url, "repeat-test-api-key")
            .bulk_upload_check(&[format!("{:040x}", 0)])
            .await
            .expect_err("a contradicted index must fail the whole check");
        assert!(
            error.contains("protocol violation") && error.contains("twice"),
            "unexpected error: {error}"
        );

        // The sweep must also record nothing from a violating response, so no
        // caller can read a half-merged confirmation out of a failed check.
        let results: serde_json::Value = serde_json::from_str(RESULTS).expect("stub body parses");
        let results = results["results"].as_array().expect("results array");
        let mut check = super::BulkUploadCheck::default();
        super::merge_checked_results(results, &(0..1), &mut check)
            .expect_err("a contradicted index must fail the merge");
        assert!(
            check.confirmed_live.is_empty() && check.duplicates.is_empty(),
            "a violating response contributed evidence: {check:?}"
        );
    }

    /// Repeated rows that AGREE are rejected too. This is the deliberate
    /// fail-closed choice: a server that answers one requested row twice has
    /// disproved its one-answer-per-row pairing, and agreement between the two
    /// copies is not evidence that the duplication was harmless. Keeping the
    /// user's files costs one re-run of the check; trusting a mispairing server
    /// costs the original.
    #[tokio::test]
    async fn bulk_upload_check_rejects_a_repeated_index_even_when_the_rows_agree() {
        let stub = spawn_http_stub(|_| {
            http_response(
                "200 OK",
                &[],
                r#"{"results":[{"id":"0","action":"reject","reason":"duplicate","isTrashed":false},{"id":"0","action":"reject","reason":"duplicate","isTrashed":false}]}"#,
            )
        })
        .await;

        let error = ImmichClient::new(&stub.url, "repeat-test-api-key")
            .bulk_upload_check(&[format!("{:040x}", 0)])
            .await
            .expect_err("a repeated index must fail the check even when it agrees");
        assert!(
            error.contains("protocol violation") && error.contains("twice"),
            "unexpected error: {error}"
        );
    }

    /// The guard must not cost the normal case anything: one row per index still
    /// confirms the live ones, still forecasts the unknown-status ones, and
    /// still keeps the trashed ones out of both sets.
    #[test]
    fn one_row_per_index_is_recorded_unchanged() {
        use serde_json::json;

        let results = [
            json!({ "id": "0", "action": "reject", "reason": "duplicate", "isTrashed": false }),
            json!({ "id": "1", "action": "reject", "reason": "duplicate", "isTrashed": true }),
            json!({ "id": "2", "action": "reject", "reason": "duplicate" }),
            json!({ "id": "3", "action": "accept" }),
        ];
        let mut check = super::BulkUploadCheck::default();
        super::merge_checked_results(&results, &(0..4), &mut check)
            .expect("one answer per index is a well-formed response");

        let mut confirmed = check.confirmed_live.into_iter().collect::<Vec<_>>();
        confirmed.sort_unstable();
        assert_eq!(confirmed, [0]);
        let mut duplicates = check.duplicates.into_iter().collect::<Vec<_>>();
        duplicates.sort_unstable();
        assert_eq!(duplicates, [0, 2]);
    }

    /// An HTTP failure body is server-controlled text that becomes `job.error`,
    /// crosses IPC to the queue card, and is written to app.log as one line. The
    /// read limit is 16 MiB, so quoting the whole body let a server put megabytes
    /// into all three. The excerpt still has to name the failure it saw.
    #[tokio::test]
    async fn api_error_body_is_bounded_before_it_reaches_the_job_error() {
        let body = format!(
            r#"{{"message":"upstream exploded","pad":"{}"}}"#,
            "x".repeat(1024 * 1024)
        );
        let stub =
            spawn_http_stub(move |_| http_response("500 Internal Server Error", &[], &body)).await;

        let error = ImmichClient::new(&stub.url, "error-body-test-api-key")
            .ping()
            .await
            .expect_err("a 500 must surface as an error");
        assert!(
            error.contains("500") && error.contains("upstream exploded"),
            "the excerpt dropped the diagnostic opening: {error}"
        );
        assert!(
            error.len() < 1024,
            "the error string was not bounded ({} bytes)",
            error.len()
        );
        assert!(
            error.contains("truncated"),
            "a truncated body must say so: {error}"
        );
    }

    /// A profile URL may carry `user:pass@`, and `Url`'s Display prints userinfo
    /// verbatim. Normalization drops it so no downstream formatter can leak it.
    #[test]
    fn normalization_drops_url_credentials() {
        for (input, expected) in [
            (
                "https://s3cr3tuser:s3cr3tpass@immich.example.com/api",
                "https://immich.example.com",
            ),
            (
                "https://s3cr3tuser@immich.example.com:2283/",
                "https://immich.example.com:2283",
            ),
        ] {
            assert_eq!(normalize_server_url(input), expected);
        }
    }

    /// API error text reaches app.log and the job card, so a server URL that
    /// carried credentials must not put them there.
    #[tokio::test]
    async fn api_error_text_carries_no_url_credentials() {
        const USERNAME: &str = "s3cr3tuser";
        const PASSWORD: &str = "s3cr3tpass";

        let stub = spawn_http_stub(|_| {
            http_response("500 Internal Server Error", &[], r#"{"error":"boom"}"#)
        })
        .await;
        let with_credentials = stub
            .url
            .replace("http://", &format!("http://{USERNAME}:{PASSWORD}@"));

        let error = ImmichClient::new(&with_credentials, "credential-test-api-key")
            .ping()
            .await
            .expect_err("a 500 must surface as an API error");
        assert!(error.contains("(500"), "unexpected error: {error}");
        for secret in [USERNAME, PASSWORD] {
            assert!(
                !error.contains(secret),
                "API error leaked URL credentials: {error}"
            );
        }
        assert!(!error.contains('@'), "API error leaked userinfo: {error}");
    }

    #[test]
    fn trashed_duplicate_is_not_treated_as_present() {
        use super::{duplicates_from_results, ServerCopy};
        use serde_json::json;
        // A duplicate whose only server copy is trashed must NOT count as
        // present, or verify-before-wipe would delete the last live original.
        let results = [
            json!({ "id": "live", "action": "reject", "reason": "duplicate", "isTrashed": false }),
            json!({ "id": "trashed", "action": "reject", "reason": "duplicate", "isTrashed": true }),
        ];
        assert_eq!(
            duplicates_from_results(&results),
            vec![("live", ServerCopy::Live)]
        );
    }

    /// `isTrashed` is optional in the Immich API, so a response that omits it —
    /// or answers with the wrong type — proves nothing about the server's copy.
    /// Defaulting such a match to "live" would authorize wiping the last live
    /// local original on the word of a field the server never sent.
    #[test]
    fn unstated_trash_status_is_unknown_not_live() {
        use super::{duplicates_from_results, ServerCopy};
        use serde_json::json;
        for result in [
            json!({ "id": "x", "action": "reject", "reason": "duplicate" }),
            json!({ "id": "x", "action": "reject", "reason": "duplicate", "isTrashed": null }),
            json!({ "id": "x", "action": "reject", "reason": "duplicate", "isTrashed": "false" }),
            json!({ "id": "x", "action": "reject", "reason": "duplicate", "isTrashed": 0 }),
        ] {
            let results = [result.clone()];
            assert_eq!(
                duplicates_from_results(&results),
                vec![("x", ServerCopy::Unknown)],
                "unstated trash status must not read as live: {result}"
            );
        }
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
    // Collaborator badges show a useful name, avatar state, and no badge for malformed users.
    #[test]
    fn parses_collaborator_user_display_and_avatar_metadata() {
        use super::parse_album_user;
        use serde_json::json;

        let fully_populated = parse_album_user(&json!({
            "id": "user-1",
            "name": "Ada Lovelace",
            "email": "ada@example.com",
            "avatarColor": "blue",
            "profileImagePath": "avatars/ada.jpg"
        }))
        .expect("an id makes the collaborator user valid");
        assert_eq!(fully_populated.id, "user-1");
        assert_eq!(fully_populated.name, "Ada Lovelace");
        assert_eq!(fully_populated.email.as_deref(), Some("ada@example.com"));
        assert_eq!(fully_populated.avatar_color.as_deref(), Some("blue"));
        assert!(fully_populated.has_profile_image);

        let email_fallback = parse_album_user(&json!({
            "id": "user-2",
            "email": "grace@example.com"
        }))
        .expect("an id makes the collaborator user valid");
        assert_eq!(email_fallback.name, "grace@example.com");

        let null_name_fallback = parse_album_user(&json!({
            "id": "user-3",
            "name": null,
            "email": "linus@example.com"
        }))
        .expect("an id makes the collaborator user valid");
        assert_eq!(null_name_fallback.name, "linus@example.com");

        let generic_fallback = parse_album_user(&json!({ "id": "user-4" }))
            .expect("an id makes the collaborator user valid");
        assert_eq!(generic_fallback.name, "Immich User");

        let empty_image = parse_album_user(&json!({
            "id": "user-5",
            "profileImagePath": ""
        }))
        .expect("an id makes the collaborator user valid");
        assert!(!empty_image.has_profile_image);

        let missing_id = parse_album_user(&json!({ "name": "No Id" }));
        assert!(missing_id.is_none());
    }
}
