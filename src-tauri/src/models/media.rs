use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub path: String,
    pub name: String,
    pub extension: String,
    pub size_bytes: u64,
    pub is_video: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub files: Vec<MediaFile>,
    pub total_size_bytes: u64,
    pub photo_count: usize,
    pub video_count: usize,
    pub skipped_unreadable: usize,
}
