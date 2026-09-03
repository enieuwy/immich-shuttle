use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::models::job::ImportInput;

/// Terminal outcome of a recorded run.
///
/// Deserialization is deliberately lenient. History is JSON on disk that
/// outlives any one build, so a value this build does not know must not fail
/// the whole read and drop every row in the file.
///
/// `Unknown` keeps the value it was read from disk and writes it back verbatim.
/// `append_history` reloads every record and rewrites the whole store, so a
/// variant that serialized as a literal `"unknown"` would let this build erase a
/// newer build's outcome from the user's history on the next import, with no way
/// to recover it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordStatus {
    Completed,
    Failed,
    Cancelled,
    Unknown(String),
}

impl RecordStatus {
    /// The wire value, which for `Unknown` is whatever was read from disk.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown(raw) => raw,
        }
    }
}

impl Serialize for RecordStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for RecordStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `#[serde(other)]` is rejected on an externally tagged enum, and it
        // would discard the original value anyway, so this is spelled out.
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Unknown(raw),
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
    /// Set when the run ended with an aggregate fault: work it was asked to do
    /// that it never proved it did — a non-zero or unobserved sidecar exit, a
    /// source it could not enumerate, or a selection it could not stage.
    ///
    /// Separate from `errors`, which counts per-FILE failures only. A run that
    /// uploaded one photo and then aborted while enumerating the rest has
    /// `errors == 0`, so without this flag the receipt claims a clean run and
    /// the user may reformat the card on the strength of it.
    ///
    /// `#[serde(default)]`: records written before this field existed read as
    /// `false`, which is the honest answer — nothing recorded a fault for them.
    #[serde(default)]
    pub incomplete: bool,
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
    }

    #[test]
    fn unknown_persisted_status_loads_instead_of_failing_the_history_read() {
        // A record written by a future build must not make this build drop every
        // row in the store, so an unrecognized value degrades to Unknown.
        let parsed: RecordStatus =
            serde_json::from_str("\"partially_uploaded\"").expect("unknown status must parse");
        assert_eq!(parsed, RecordStatus::Unknown("partially_uploaded".into()));

        for known in ["completed", "failed", "cancelled"] {
            let parsed: RecordStatus = serde_json::from_str(&format!("\"{known}\""))
                .expect("a known status must round-trip");
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                format!("\"{known}\"")
            );
        }
    }

    #[test]
    fn old_history_records_default_to_clean_but_new_records_persist_incomplete() {
        // History is a JSON file that outlives a binary. A record written before
        // aggregate terminal faults became explicit has no `incomplete` field,
        // so it must keep loading rather than making every stored receipt fail.
        let old: ImportRecord = serde_json::from_str(
            r#"{
                "id":"old",
                "started_at":1,
                "finished_at":2,
                "profile_id":"p1",
                "source_paths":[],
                "album_ids":[],
                "status":"completed",
                "total":1,
                "uploaded":1,
                "duplicates":0,
                "errors":0
            }"#,
        )
        .expect("a record written before incomplete existed still loads");
        assert!(!old.incomplete);

        let mut unclean = old;
        unclean.incomplete = true;
        let wire = serde_json::to_value(&unclean).expect("a new record serializes");
        assert_eq!(wire["incomplete"], true);
    }
    #[test]
    fn a_newer_builds_status_survives_a_load_and_rewrite() {
        // `append_history` reloads every record and rewrites the whole store, so
        // a status this build cannot name still has to come back out verbatim.
        // Writing a literal "unknown" here would erase the newer outcome from the
        // user's history on their next import, unrecoverably.
        let parsed: RecordStatus =
            serde_json::from_str("\"partially_uploaded\"").expect("unknown status must parse");
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            "\"partially_uploaded\""
        );
        assert_eq!(parsed.as_wire(), "partially_uploaded");
    }
}
