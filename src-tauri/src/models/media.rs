use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub path: String,
    pub name: String,
    pub extension: String,
    pub size_bytes: u64,
    pub is_video: bool,
}

// Retained as a test helper for the non-streaming scanner used in unit tests;
// production scanning streams ScanProgress batches and returns ScanSummary.
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub files: Vec<MediaFile>,
    pub total_size_bytes: u64,
    pub photo_count: usize,
    pub video_count: usize,
    pub skipped_unreadable: usize,
}

/// A batch of freshly-scanned files, streamed to the frontend as the
/// `scan-progress` event fires repeatedly during one `scan_sources_stream`
/// call.
///
/// `scan_id` identifies which scan produced this batch. The event channel
/// (`app.emit("scan-progress", …)`) is shared across every scan in the
/// window's lifetime, and cancelling a scan is not an emit barrier: a batch
/// already in flight when `scan_sources_stream` is called again for a new
/// selection can still land after the new listener is installed. Without a
/// per-scan id the frontend has no way to tell that batch apart from one
/// belonging to the scan it actually asked for, so a stale batch from a
/// source the user has since deselected would silently contaminate the new
/// scan's file list and counts. The id is supplied by the frontend at call
/// time (not generated here), so there is no handshake race between
/// receiving a scan id and installing the listener that filters on it.
#[derive(Debug, Clone, Serialize)]
pub struct ScanProgress {
    pub scan_id: String,
    pub files: Vec<MediaFile>,
    pub photo_count: usize,
    pub video_count: usize,
    pub total_size_bytes: u64,
    pub skipped_unreadable: usize,
}

/// Why a streamed scan stopped. `Complete` is the only outcome that means the
/// file list is the whole selection; the other two mean it is a prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    Complete,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSummary {
    pub status: ScanStatus,
    pub photo_count: usize,
    pub video_count: usize,
    pub total_size_bytes: u64,
    pub skipped_unreadable: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_status_serializes_to_the_frontend_union() {
        // These three strings are the `ScanSummary.status` union in types.ts.
        // Renaming a variant without updating that union breaks the frontend
        // silently, so pin the wire values here.
        let wire = |status| serde_json::to_string(&status).expect("serialize scan status");
        assert_eq!(wire(ScanStatus::Complete), "\"complete\"");
        assert_eq!(wire(ScanStatus::Cancelled), "\"cancelled\"");
        assert_eq!(wire(ScanStatus::TimedOut), "\"timed_out\"");
    }
}
