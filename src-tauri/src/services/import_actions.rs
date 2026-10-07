use crate::{
    models::job::{ImportInput, Organization},
    services::immich_client::ImmichClient,
};
use serde_json::{json, Value};
use std::time::Duration;

const CAPTURE_METADATA_WAIT: Duration = Duration::from_secs(30);
const CAPTURE_METADATA_POLL: Duration = Duration::from_millis(250);

fn capture_date(asset: &Value) -> Option<&str> {
    asset
        .pointer("/exifInfo/dateTimeOriginal")
        .and_then(Value::as_str)
        .filter(|date| !date.is_empty())
}

async fn extracted_capture_date(
    client: &ImmichClient,
    asset: &Value,
    id: &str,
    is_cancelled: &(impl Fn() -> bool + Send + Sync),
) -> Result<chrono::DateTime<chrono::FixedOffset>, String> {
    let parse = |date: &str| {
        chrono::DateTime::parse_from_rfc3339(date)
            .map_err(|_| "Asset capture date is invalid".to_string())
    };
    if let Some(date) = capture_date(asset) {
        return parse(date);
    }

    // Search can return an uploaded asset before Immich extracts its EXIF.
    // Never use the provisional filesystem date as the clock correction basis.
    tokio::time::timeout(CAPTURE_METADATA_WAIT, async {
        loop {
            if is_cancelled() {
                return Err("Import cancelled.".into());
            }
            let metadata = client.asset(id).await?;
            if is_cancelled() {
                return Err("Import cancelled.".into());
            }
            if let Some(date) = capture_date(&metadata) {
                return parse(date);
            }
            tokio::time::sleep(CAPTURE_METADATA_POLL).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(format!(
            "Capture date correction is incomplete: asset {id} has no extracted capture date after 30 seconds."
        ))
    })
}

pub async fn apply(
    client: &ImmichClient,
    input: &ImportInput,
    assets: &[Value],
    album_id: Option<&str>,
    server_url: &str,
    is_cancelled: impl Fn() -> bool + Send + Sync,
) -> Result<Vec<String>, String> {
    if input.extended.dry_run {
        return Ok(Vec::new());
    }
    if is_cancelled() {
        return Err("Import cancelled.".into());
    }
    // The uploader can recreate a deleted picker album. Only the confirmed
    // destination is safe here; folder organization never uses a picker album.
    let album_id = album_id.filter(|_| input.organization == Organization::SingleAlbum);
    if let Some(album) = album_id {
        let ids: Vec<String> = assets
            .iter()
            .filter_map(|a| a.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        // Each client call must contain at most one server mutation.
        for chunk in ids.chunks(500) {
            if is_cancelled() {
                return Err("Import cancelled.".into());
            }
            client.add_assets_to_album(album, chunk).await?;
        }
    }
    if input.extended.clock_offset_minutes != 0 {
        for asset in assets {
            let id = asset
                .get("id")
                .and_then(Value::as_str)
                .ok_or("Asset has no id")?;
            let date = extracted_capture_date(client, asset, id, &is_cancelled).await?;
            let corrected = date
                .checked_add_signed(chrono::Duration::minutes(i64::from(
                    input.extended.clock_offset_minutes,
                )))
                .ok_or("Capture date correction is outside the supported range")?;
            if is_cancelled() {
                return Err("Import cancelled.".into());
            }
            client
                .correct_capture_date(id, &corrected.to_rfc3339())
                .await?;
        }
    }
    let mut links = Vec::new();
    if !input.extended.share_user_ids.is_empty() || input.extended.public_link {
        let album = album_id.ok_or("Sharing requires a confirmed destination album.")?;
        if !input.extended.share_user_ids.is_empty() {
            if is_cancelled() {
                return Err("Import cancelled.".into());
            }
            client
                .share_album_users(
                    album,
                    &input.extended.share_user_ids,
                    input.extended.share_role.as_deref().unwrap_or("viewer"),
                )
                .await?;
        }
        if input.extended.public_link {
            if is_cancelled() {
                return Err("Import cancelled.".into());
            }
            links.push(client.create_share_link(album, server_url).await?.url);
        }
    }
    Ok(links)
}

pub async fn callback(
    url: &str,
    job: &crate::models::job::ImportJob,
    input: &ImportInput,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(url)
        .json(&json!({
            "job_id": job.id, "status": job.status, "uploaded": job.progress.uploaded,
            "duplicates": job.progress.duplicates, "errors": job.progress.errors,
            "error": job.error,
            "success": matches!(job.status, crate::models::job::JobStatus::Completed)
                && job.error.is_none() && job.progress.errors == 0,
            "album_ids": input.album_ids, "profile_id": input.profile_id,
            "source_paths": input.source_paths, "dry_run": input.extended.dry_run
        }))
        .send()
        .await
        .map_err(|_| "Completion callback could not reach its destination.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Completion callback returned HTTP {}.",
            response.status()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::mpsc,
    };

    use super::*;

    struct HttpStub {
        url: String,
        requests: mpsc::UnboundedReceiver<String>,
        cancelled: Arc<AtomicBool>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for HttpStub {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    async fn spawn_http_stub(cancel_after: Option<usize>) -> HttpStub {
        spawn_http_stub_with_metadata(cancel_after, Vec::new()).await
    }

    async fn spawn_http_stub_with_metadata(
        cancel_after: Option<usize>,
        metadata: Vec<Value>,
    ) -> HttpStub {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (requests_tx, requests) = mpsc::unbounded_channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let server_cancelled = Arc::clone(&cancelled);
        let handle = tokio::spawn(async move {
            let mut count = 0;
            let mut metadata_index = 0;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while let Ok(read) = socket.read(&mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    let Some(head_len) = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|offset| offset + 4)
                    else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&request[..head_len]);
                    let body_len = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= head_len + body_len {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let payload: Value = request
                    .split_once("\r\n\r\n")
                    .filter(|(_, body)| !body.is_empty())
                    .map(|(_, body)| serde_json::from_str(body).unwrap())
                    .unwrap_or(Value::Null);
                let body = if request.starts_with("GET ") {
                    let row = metadata
                        .get(metadata_index)
                        .or_else(|| metadata.last())
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    metadata_index += 1;
                    row.to_string()
                } else if path.ends_with("/assets") {
                    let rows: Vec<Value> = payload["ids"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| json!({"id": id, "success": true}))
                        .collect();
                    serde_json::to_string(&rows).unwrap()
                } else if path.ends_with("/shared-links") {
                    r#"{"key":"public-key"}"#.into()
                } else {
                    "{}".into()
                };
                count += 1;
                if cancel_after == Some(count) {
                    // Change the live state before responding. The next write
                    // must observe it without sleeps or a stale snapshot.
                    server_cancelled.store(true, Ordering::SeqCst);
                }
                requests_tx.send(request).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        HttpStub {
            url: format!("http://127.0.0.1:{}", addr.port()),
            requests,
            cancelled,
            handle,
        }
    }

    fn input() -> ImportInput {
        serde_json::from_value(json!({
            "profile_id": "p", "source_paths": ["/source"],
            "album_ids": ["deleted-picker-id"], "keep_files": true,
            "stack_raw_jpeg": false, "stack_burst": false,
            "date_range": null, "concurrent_tasks": null
        }))
        .unwrap()
    }

    fn assets() -> Vec<Value> {
        vec![
            json!({"id": "asset-1", "exifInfo": {"dateTimeOriginal": "2026-03-15T12:00:00Z"}}),
            json!({"id": "asset-2", "exifInfo": {"dateTimeOriginal": "2026-03-15T13:00:00Z"}}),
        ]
    }

    fn request_paths(stub: &mut HttpStub) -> Vec<String> {
        let mut paths = Vec::new();
        while let Ok(request) = stub.requests.try_recv() {
            paths.push(
                request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .into(),
            );
        }
        paths
    }

    #[tokio::test]
    async fn clock_correction_waits_for_exif_instead_of_using_filesystem_date() {
        let mut stub = spawn_http_stub_with_metadata(
            None,
            vec![
                json!({"id": "asset-1", "fileCreatedAt": "2023-11-14T22:13:20Z",
                    "exifInfo": {"dateTimeOriginal": null}}),
                json!({"id": "asset-1", "fileCreatedAt": "2023-11-14T22:13:20Z",
                    "exifInfo": {"dateTimeOriginal": "2020-01-02T03:04:05Z"}}),
            ],
        )
        .await;
        let client = ImmichClient::new(&stub.url, "test-key");
        let mut input = input();
        input.extended.clock_offset_minutes = 90;
        let assets = [json!({"id": "asset-1", "fileCreatedAt": "2023-11-14T22:13:20Z"})];
        apply(&client, &input, &assets, None, &stub.url, || false)
            .await
            .unwrap();

        let first = stub.requests.try_recv().unwrap();
        let second = stub.requests.try_recv().unwrap();
        let correction = stub.requests.try_recv().unwrap();
        assert!(first.starts_with("GET /api/assets/asset-1 "));
        assert!(second.starts_with("GET /api/assets/asset-1 "));
        assert!(correction.starts_with("PUT /api/assets/asset-1 "));
        let payload: Value =
            serde_json::from_str(correction.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload["dateTimeOriginal"], "2020-01-02T04:34:05+00:00");
        assert!(stub.requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn unavailable_capture_metadata_reports_incomplete_without_correction() {
        let mut stub = spawn_http_stub_with_metadata(
            None,
            vec![json!({"id": "asset-1", "fileCreatedAt": "2023-11-14T22:13:20Z"})],
        )
        .await;
        let client = ImmichClient::new(&stub.url, "test-key");
        let mut input = input();
        input.extended.clock_offset_minutes = 90;
        let server_url = stub.url.clone();
        let action = tokio::spawn(async move {
            apply(
                &client,
                &input,
                &[json!({"id": "asset-1", "fileCreatedAt": "2023-11-14T22:13:20Z"})],
                None,
                &server_url,
                || false,
            )
            .await
        });
        let request = stub.requests.recv().await.unwrap();
        assert!(request.starts_with("GET /api/assets/asset-1 "));
        // Establish real HTTP I/O before advancing virtual time; otherwise an
        // idle paused runtime can expire the request before the stub accepts it.
        tokio::time::pause();
        tokio::time::advance(CAPTURE_METADATA_WAIT).await;
        assert_eq!(
            action.await.unwrap().unwrap_err(),
            "Capture date correction is incomplete: asset asset-1 has no extracted capture date after 30 seconds."
        );
        while let Ok(request) = stub.requests.try_recv() {
            assert!(request.starts_with("GET /api/assets/asset-1 "));
        }
    }

    #[tokio::test]
    async fn cancellation_during_capture_metadata_wait_prevents_correction() {
        let mut stub = spawn_http_stub_with_metadata(Some(1), vec![json!({})]).await;
        let client = ImmichClient::new(&stub.url, "test-key");
        let mut input = input();
        input.extended.clock_offset_minutes = 90;
        let cancelled = Arc::clone(&stub.cancelled);
        assert_eq!(
            apply(
                &client,
                &input,
                &[json!({"id": "asset-1", "fileCreatedAt": "2023-11-14T22:13:20Z"})],
                None,
                &stub.url,
                || cancelled.load(Ordering::SeqCst),
            )
            .await
            .unwrap_err(),
            "Import cancelled."
        );
        assert_eq!(request_paths(&mut stub), ["/api/assets/asset-1"]);
    }

    #[tokio::test]
    async fn resolved_destination_replaces_stale_picker_for_membership_and_sharing() {
        let mut stub = spawn_http_stub(None).await;
        let client = ImmichClient::new(&stub.url, "test-key");
        let mut input = input();
        input.extended.share_user_ids = vec!["recipient".into()];
        input.extended.public_link = true;
        let links = apply(
            &client,
            &input,
            &assets(),
            Some("resolved-id"),
            &stub.url,
            || false,
        )
        .await
        .unwrap();
        assert_eq!(links.len(), 1);
        let mut requests = Vec::new();
        while let Ok(request) = stub.requests.try_recv() {
            requests.push(request);
        }
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("PUT /api/albums/resolved-id/assets "));
        assert!(requests[1].starts_with("PUT /api/albums/resolved-id/users "));
        assert!(requests[2].starts_with("POST /api/shared-links "));
        let payload: Value =
            serde_json::from_str(requests[2].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload["albumId"], "resolved-id");
        assert!(requests
            .iter()
            .all(|request| !request.contains("deleted-picker-id")));
    }

    #[tokio::test]
    async fn folder_organization_ignores_picker_and_single_destination_ids() {
        let mut stub = spawn_http_stub(None).await;
        let client = ImmichClient::new(&stub.url, "test-key");
        for organization in [
            Organization::FolderName,
            Organization::FolderPath,
            Organization::FolderTags,
        ] {
            let mut input = input();
            input.organization = organization;
            for destination in [None, Some("resolved-picker-id")] {
                assert!(
                    apply(&client, &input, &assets(), destination, &stub.url, || false)
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
        assert!(request_paths(&mut stub).is_empty());
    }

    #[tokio::test]
    async fn sharing_and_links_require_a_confirmed_single_destination() {
        let mut stub = spawn_http_stub(None).await;
        let client = ImmichClient::new(&stub.url, "test-key");
        for organization in [
            Organization::SingleAlbum,
            Organization::FolderName,
            Organization::FolderPath,
            Organization::FolderTags,
        ] {
            for public_link in [false, true] {
                let mut input = input();
                input.organization = organization;
                input.extended.public_link = public_link;
                if !public_link {
                    input.extended.share_user_ids = vec!["recipient".into()];
                }
                let destination =
                    (organization != Organization::SingleAlbum).then_some("picker-id");
                assert_eq!(
                    apply(&client, &input, &assets(), destination, &stub.url, || false)
                        .await
                        .unwrap_err(),
                    "Sharing requires a confirmed destination album."
                );
            }
        }
        assert!(request_paths(&mut stub).is_empty());
    }

    #[tokio::test]
    async fn cancellation_before_actions_prevents_every_mutation() {
        let mut stub = spawn_http_stub(None).await;
        let client = ImmichClient::new(&stub.url, "test-key");
        assert_eq!(
            apply(
                &client,
                &input(),
                &assets(),
                Some("resolved-id"),
                &stub.url,
                || true
            )
            .await
            .unwrap_err(),
            "Import cancelled."
        );
        assert!(request_paths(&mut stub).is_empty());
    }

    #[tokio::test]
    async fn live_cancellation_stops_between_each_separate_completion_mutation() {
        let expected = [
            "/api/albums/resolved-id/assets",
            "/api/assets/asset-1",
            "/api/assets/asset-2",
            "/api/albums/resolved-id/users",
            "/api/shared-links",
        ];
        for cancel_after in 1..expected.len() {
            let mut stub = spawn_http_stub(Some(cancel_after)).await;
            let client = ImmichClient::new(&stub.url, "test-key");
            let mut input = input();
            input.extended.clock_offset_minutes = 30;
            input.extended.share_user_ids = vec!["recipient".into()];
            input.extended.public_link = true;
            let cancelled = Arc::clone(&stub.cancelled);
            assert_eq!(
                apply(
                    &client,
                    &input,
                    &assets(),
                    Some("resolved-id"),
                    &stub.url,
                    || cancelled.load(Ordering::SeqCst)
                )
                .await
                .unwrap_err(),
                "Import cancelled."
            );
            assert_eq!(request_paths(&mut stub), expected[..cancel_after]);
        }
    }

    #[tokio::test]
    async fn live_cancellation_stops_between_album_membership_batches() {
        let mut stub = spawn_http_stub(Some(1)).await;
        let client = ImmichClient::new(&stub.url, "test-key");
        let assets: Vec<Value> = (0..501)
            .map(|id| json!({"id": format!("asset-{id}")}))
            .collect();
        let cancelled = Arc::clone(&stub.cancelled);
        assert_eq!(
            apply(
                &client,
                &input(),
                &assets,
                Some("resolved-id"),
                &stub.url,
                || cancelled.load(Ordering::SeqCst)
            )
            .await
            .unwrap_err(),
            "Import cancelled."
        );
        let request = stub.requests.try_recv().unwrap();
        let payload: Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload["ids"].as_array().unwrap().len(), 500);
        assert!(stub.requests.try_recv().is_err());
    }
}
