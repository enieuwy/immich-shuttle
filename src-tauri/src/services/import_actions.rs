use crate::{models::job::ImportInput, services::immich_client::ImmichClient};
use serde_json::{json, Value};

pub async fn apply(client: &ImmichClient, input: &ImportInput, assets: &[Value], album_id: Option<&str>, server_url: &str) -> Result<Vec<String>, String> {
    if input.extended.dry_run { return Ok(Vec::new()); }
    let ids: Vec<String> = assets.iter().filter_map(|a| a.get("id").and_then(Value::as_str).map(str::to_string)).collect();
    let mut albums = input.album_ids.clone();
    if let Some(id) = album_id {
        if !albums.iter().any(|v| v == id) { albums.push(id.to_string()); }
    }
    for album in &albums { client.add_assets_to_album(album, &ids).await?; }
    if input.extended.clock_offset_minutes != 0 {
        for asset in assets {
            let id = asset.get("id").and_then(Value::as_str).ok_or("Asset has no id")?;
            let date = asset.pointer("/exifInfo/dateTimeOriginal").and_then(Value::as_str)
                .or_else(|| asset.get("fileCreatedAt").and_then(Value::as_str))
                .ok_or("Asset has no capture date")?;
            let date = chrono::DateTime::parse_from_rfc3339(date).map_err(|_| "Asset capture date is invalid")?;
            let corrected = date.checked_add_signed(chrono::Duration::minutes(i64::from(input.extended.clock_offset_minutes)))
                .ok_or("Capture date correction is outside the supported range")?;
            client.correct_capture_date(id, &corrected.to_rfc3339()).await?;
        }
    }
    let mut links = Vec::new();
    if !input.extended.share_user_ids.is_empty() || input.extended.public_link {
        if albums.is_empty() { return Err("Sharing requires a confirmed destination album.".into()); }
        for album in albums {
            if !input.extended.share_user_ids.is_empty() {
                client.share_album_users(&album, &input.extended.share_user_ids, input.extended.share_role.as_deref().unwrap_or("viewer")).await?;
            }
            if input.extended.public_link {
                links.push(client.create_share_link(&album, server_url).await?.url);
            }
        }
    }
    Ok(links)
}

pub async fn callback(url: &str, job: &crate::models::job::ImportJob, input: &ImportInput) -> Result<(), String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string())?;
    let response = client.post(url).json(&json!({
        "job_id": job.id, "status": job.status, "uploaded": job.progress.uploaded,
        "duplicates": job.progress.duplicates, "errors": job.progress.errors,
        "album_ids": input.album_ids, "profile_id": input.profile_id,
        "source_paths": input.source_paths, "dry_run": input.extended.dry_run
    })).send().await.map_err(|_| "Completion callback could not reach its destination.".to_string())?;
    if !response.status().is_success() { return Err(format!("Completion callback returned HTTP {}.", response.status())); }
    Ok(())
}
