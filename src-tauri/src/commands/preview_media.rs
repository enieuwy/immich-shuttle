//! Scoped video access. The protocol exposes opaque tickets, never file paths.
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    time::SystemTime,
};
use tauri::http::{Request, Response, StatusCode};

const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TICKETS: usize = 16;

struct VideoTicket {
    path: PathBuf,
    token: u64,
    file: Mutex<File>,
    len: u64,
    modified: Option<SystemTime>,
    mime: &'static str,
}

static TICKETS: LazyLock<Mutex<HashMap<String, Arc<VideoTicket>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn open(path: String, token: u64) -> Result<String, String> {
    if super::preview_session_cancelled(token) {
        return Err("Preview cancelled".to_string());
    }
    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !crate::services::source_guard::is_within_approved(&path.to_string_lossy()) {
        return Err("File is outside the selected sources".to_string());
    }
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        _ => return Err("This file is not a supported video container".to_string()),
    };
    let file = File::open(&path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("Video is empty or unavailable".to_string());
    }
    let ticket = VideoTicket {
        path,
        token,
        file: Mutex::new(file),
        len: metadata.len(),
        modified: metadata.modified().ok(),
        mime,
    };
    let mut tickets = TICKETS.lock().unwrap_or_else(|p| p.into_inner());
    tickets.retain(|_, t| !super::preview_session_cancelled(t.token));
    if tickets.len() >= MAX_TICKETS {
        return Err("Close an existing video preview before opening another".to_string());
    }
    let key = uuid::Uuid::new_v4().to_string();
    tickets.insert(key.clone(), Arc::new(ticket));
    Ok(key)
}

pub(super) fn release(ticket: &str) {
    TICKETS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(ticket);
}

pub(super) fn cancel() {
    TICKETS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|_, t| !super::preview_session_cancelled(t.token));
}

fn response(status: StatusCode) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Cache-Control", "no-store")
        .body(Vec::new())
        .unwrap()
}

/// Every request repeats source authorization and cancellation checks. Reads
/// retain the original file handle and serialize seeking, including on Unix.
pub fn serve(request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != "GET" && request.method() != "HEAD" {
        return response(StatusCode::METHOD_NOT_ALLOWED);
    }
    let key = request.uri().path().trim_start_matches('/');
    let Some(ticket) = TICKETS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(key)
        .cloned()
    else {
        return response(StatusCode::NOT_FOUND);
    };
    if super::preview_session_cancelled(ticket.token)
        || !crate::services::source_guard::is_within_approved(&ticket.path.to_string_lossy())
    {
        return response(StatusCode::FORBIDDEN);
    }
    let mut file = ticket.file.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(metadata) = file.metadata() else {
        return response(StatusCode::NOT_FOUND);
    };
    if metadata.len() != ticket.len || metadata.modified().ok() != ticket.modified {
        return response(StatusCode::CONFLICT);
    }
    if super::preview_session_cancelled(ticket.token)
        || !crate::services::source_guard::is_within_approved(&ticket.path.to_string_lossy())
    {
        return response(StatusCode::FORBIDDEN);
    }
    // Range applies to GET only. HEAD describes the complete representation
    // without allocating the video or returning unsolicited partial content.
    if request.method() == "HEAD" {
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", ticket.mime)
            .header("Content-Length", ticket.len.to_string())
            .header("Accept-Ranges", "bytes")
            .header("Cache-Control", "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(Vec::new())
            .unwrap();
    }
    let range_header = request.headers().get("Range");
    if range_header.is_none() && ticket.len > MAX_RESPONSE_BYTES {
        // This Tauri response is buffered. Refuse an unbounded allocation
        // instead of pretending that the first chunk is a complete video.
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header("Accept-Ranges", "bytes")
            .header("Cache-Control", "no-store")
            .body(Vec::new())
            .unwrap();
    }
    let range = match range_header {
        Some(header) => header
            .to_str()
            .ok()
            .and_then(|s| parse_range(s, ticket.len)),
        None => Some((0, ticket.len - 1)),
    };
    let Some((start, requested_end)) = range else {
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header("Content-Range", format!("bytes */{}", ticket.len))
            .header("Cache-Control", "no-store")
            .body(Vec::new())
            .unwrap();
    };
    let end = requested_end.min(start.saturating_add(MAX_RESPONSE_BYTES - 1));
    let len = end - start + 1;
    let partial = range_header.is_some();
    let mut bytes = vec![0; len as usize];
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut bytes).is_err() {
        return response(StatusCode::NOT_FOUND);
    }
    if super::preview_session_cancelled(ticket.token)
        || !crate::services::source_guard::is_within_approved(&ticket.path.to_string_lossy())
    {
        return response(StatusCode::FORBIDDEN);
    }
    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header("Content-Type", ticket.mime)
        .header("Content-Length", len.to_string())
        .header("Accept-Ranges", "bytes")
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff");
    if partial {
        builder = builder.header(
            "Content-Range",
            format!("bytes {start}-{end}/{}", ticket.len),
        );
    }
    builder.body(bytes).unwrap()
}

fn parse_range(value: &str, len: u64) -> Option<(u64, u64)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    if len == 0 || end.contains(',') {
        return None;
    }
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        return (suffix > 0).then_some((len.saturating_sub(suffix), len - 1));
    }
    let start: u64 = start.parse().ok()?;
    let end = if end.is_empty() {
        len - 1
    } else {
        end.parse::<u64>().ok()?.min(len - 1)
    };
    (start <= end && start < len).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_range_boundaries_support_seeking_and_suffixes() {
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=95-200", 100), Some((95, 99)));
        assert_eq!(parse_range("bytes=80-", 100), Some((80, 99)));
        assert_eq!(parse_range("bytes=-20", 100), Some((80, 99)));
        assert_eq!(parse_range("bytes=-200", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=50-40", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("bytes=-0", 100), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
    }
}
