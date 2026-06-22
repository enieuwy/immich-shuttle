use serde::{Deserialize, Serialize};

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
}
