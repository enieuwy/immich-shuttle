use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgress {
    pub total: u32,
    pub uploaded: u32,
    pub duplicates: u32,
    pub errors: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportInput {
    pub profile_id: String,
    pub source_paths: Vec<String>,
    pub album_ids: Vec<String>,
    pub keep_files: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportJob {
    pub id: String,
    pub status: JobStatus,
    pub progress: JobProgress,
    pub error: Option<String>,
    pub summary: Option<String>,
    pub awaiting_wipe_confirmation: bool,
    pub pending_wipe_count: u32,
}
