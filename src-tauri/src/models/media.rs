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

#[derive(Debug, Clone, Serialize)]
pub struct ScanProgress {
    pub files: Vec<MediaFile>,
    pub photo_count: usize,
    pub video_count: usize,
    pub total_size_bytes: u64,
    pub skipped_unreadable: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSummary {
    pub status: String,
    pub photo_count: usize,
    pub video_count: usize,
    pub total_size_bytes: u64,
    pub skipped_unreadable: usize,
}
