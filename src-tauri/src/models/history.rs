use serde::{Deserialize, Deserializer, Serialize};

use crate::models::job::ImportInput;

/// Terminal outcome of a recorded run.
///
/// Deserialization is deliberately lenient. History is JSON on disk that
/// outlives any one build, so a value this build does not know must not fail
/// the whole read and drop every row in the file. Unrecognized values load as
/// `Unknown` and are rendered as such, which is why the frontend union carries
/// `"unknown"` too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

impl<'de> Deserialize<'de> for RecordStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `#[serde(other)]` is rejected on an externally tagged enum, so the
        // fallback is spelled out here rather than derived.
        Ok(match String::deserialize(deserializer)?.as_str() {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Unknown,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRecord {
    pub id: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub profile_id: String,
    pub source_paths: Vec<String>,
    pub album_ids: Vec<String>,
    pub status: RecordStatus,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_status_serializes_to_the_frontend_union() {
        let wire = |status| serde_json::to_string(&status).expect("serialize record status");
        assert_eq!(wire(RecordStatus::Completed), "\"completed\"");
        assert_eq!(wire(RecordStatus::Failed), "\"failed\"");
        assert_eq!(wire(RecordStatus::Cancelled), "\"cancelled\"");
        assert_eq!(wire(RecordStatus::Unknown), "\"unknown\"");
    }

    #[test]
    fn unknown_persisted_status_loads_instead_of_failing_the_history_read() {
        // A record written by a future build must not make this build drop every
        // row in history.json, so an unrecognized value degrades to Unknown.
        let parsed: RecordStatus =
            serde_json::from_str("\"partially_uploaded\"").expect("unknown status must parse");
        assert_eq!(parsed, RecordStatus::Unknown);

        for known in ["completed", "failed", "cancelled"] {
            let parsed: RecordStatus = serde_json::from_str(&format!("\"{known}\""))
                .expect("a known status must round-trip");
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                format!("\"{known}\"")
            );
        }
    }
}
