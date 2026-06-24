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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub stack_raw_jpeg: bool,
    pub stack_burst: bool,
    pub date_range: Option<String>,
    pub concurrent_tasks: Option<u32>,
    /// When set, import only these files (staged into a temp dir); forces keep_files.
    #[serde(default)]
    pub select_files: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileError {
    /// The file immich-go reported as failed (its full name as logged, e.g. "<fs>:<name>").
    pub file: String,
    /// Why it failed (the `error=` reason, or the event message when no reason was logged).
    pub reason: String,
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
    /// Per-file failures parsed from immich-go's run log, for actionable reporting.
    pub file_errors: Vec<FileError>,
}
