use serde::{Deserialize, Serialize};

use crate::models::job::ImportInput;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRecord {
    pub id: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub profile_id: String,
    pub source_paths: Vec<String>,
    pub album_ids: Vec<String>,
    pub status: String,
    pub total: u32,
    pub uploaded: u32,
    pub duplicates: u32,
    pub errors: u32,
    /// The full import request that produced this run, persisted so History can
    /// replay it. Optional: records written before this field existed (and runs
    /// where the request was unavailable) deserialize as `None`.
    #[serde(default)]
    pub request: Option<ImportInput>,
}
