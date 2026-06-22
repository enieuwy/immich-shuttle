use regex::Regex;

use std::collections::HashSet;

use crate::models::job::{FileError, JobProgress};

#[derive(Debug, Clone)]
pub struct ParsedLine {
    pub progress: JobProgress,
    pub has_error: bool,
    pub completed_asset_path: Option<String>,
    pub completed_asset_id: Option<String>,
}

pub fn parse_line(line: &str, mut current: JobProgress) -> ParsedLine {
    let total_re = Regex::new(r"Total Assets:\s*(\d+)").expect("valid regex");
    let dup_re = Regex::new(r"server has duplicate\s*:\s*(\d+)").expect("valid regex");
    let up_re = Regex::new(r"uploaded\s*:\s*(\d+)").expect("valid regex");
    let archived_re = Regex::new(r"FileArchived.*path=([^\s]+)").expect("valid regex");
    let asset_id_re =
        Regex::new(r"(?:assetId|asset_id|id)=([A-Fa-f0-9-]{8,})").expect("valid regex");

    if let Some(c) = total_re.captures(line) {
        current.total = c
            .get(1)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(current.total);
    }
    if let Some(c) = dup_re.captures(line) {
        current.duplicates = c
            .get(1)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(current.duplicates);
    }
    if let Some(c) = up_re.captures(line) {
        current.uploaded = c
            .get(1)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(current.uploaded);
    }

    let lower = line.to_lowercase();
    let has_error = (lower.contains("error") || lower.contains("failed") || line.contains("ERR "))
        && !lower.contains("upload errors");
    if has_error {
        current.errors = current.errors.saturating_add(1);
    }

    let completed_asset_path = archived_re
        .captures(line)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
    let completed_asset_id = asset_id_re
        .captures(line)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));

    ParsedLine {
        progress: current,
        has_error,
        completed_asset_path,
        completed_asset_id,
    }
}

/// Maximum number of per-file errors retained from a single run, to keep the job
/// payload and the UI bounded on imports that fail en masse.
const MAX_FILE_ERRORS: usize = 100;

/// Extract the value of a space-delimited `key=` attribute from a console-slog
/// line. immich-go's console handler writes attribute values UNQUOTED (spaces
/// and all), so a value runs from after `key=` until the next known attribute
/// key or end of line.
fn attr_value(line: &str, key: &str, next_keys: &[&str]) -> Option<String> {
    let needle = format!(" {key}=");
    let pos = line.find(&needle)?;
    let rest = &line[pos + needle.len()..];
    let mut end = rest.len();
    for next in next_keys {
        let marker = format!(" {next}=");
        if let Some(p) = rest.find(&marker) {
            if p < end {
                end = p;
            }
        }
    }
    Some(rest[..end].trim().to_string())
}

/// The event message of a console-slog line (between the `ERR ` level token and
/// the first attribute), e.g. "server error".
fn error_message(line: &str) -> Option<String> {
    let start = line.find(" ERR ")? + " ERR ".len();
    let rest = &line[start..];
    let end = rest
        .find(" file=")
        .or_else(|| rest.find(" error="))
        .unwrap_or(rest.len());
    let msg = rest[..end].trim();
    if msg.is_empty() {
        None
    } else {
        Some(msg.to_string())
    }
}

/// Parse per-file upload failures out of immich-go's run log (written via
/// `--log-file`). Only ERROR-level lines that carry a `file=` attribute are
/// reported; aggregate/album errors (no `file=`) are surfaced elsewhere.
/// Failures are deduplicated by file, preserving the first reason seen.
pub fn parse_error_log(contents: &str) -> Vec<FileError> {
    // console-slog (NoColor) error lines start: "YYYY-MM-DD HH:MM:SS ERR ".
    let err_line =
        Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\s+ERR\s").expect("valid regex");
    let mut out: Vec<FileError> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for line in contents.lines() {
        if !err_line.is_match(line) {
            continue;
        }
        let file = match attr_value(line, "file", &["error", "reason", "discovered_at"]) {
            Some(f) if !f.is_empty() => f,
            _ => continue,
        };
        if !seen.insert(file.clone()) {
            continue;
        }
        let reason = attr_value(line, "error", &["discovered_at"])
            .filter(|r| !r.is_empty())
            .or_else(|| attr_value(line, "reason", &["discovered_at"]).filter(|r| !r.is_empty()))
            .or_else(|| error_message(line))
            .unwrap_or_else(|| "Upload failed".to_string());
        out.push(FileError { file, reason });
        if out.len() >= MAX_FILE_ERRORS {
            break;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{parse_error_log, parse_line};
    use crate::models::job::JobProgress;

    #[test]
    fn parses_total_assets() {
        let initial = JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        };
        let parsed = parse_line("Total Assets: 15", initial);
        assert_eq!(parsed.progress.total, 15);
    }

    #[test]
    fn parses_duplicates() {
        let initial = JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        };
        let parsed = parse_line("server has duplicate : 3", initial);
        assert_eq!(parsed.progress.duplicates, 3);
    }

    #[test]
    fn marks_error_lines() {
        let initial = JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        };
        let parsed = parse_line("ERROR upload failed", initial);
        assert!(parsed.has_error);
        assert_eq!(parsed.progress.errors, 1);
    }

    #[test]
    fn extracts_archived_asset_path() {
        let initial = JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        };
        let parsed = parse_line(
            "FileArchived status=ok path=/tmp/DCIM/100MEDIA/IMG_0001.JPG",
            initial,
        );
        assert_eq!(
            parsed.completed_asset_path.as_deref(),
            Some("/tmp/DCIM/100MEDIA/IMG_0001.JPG")
        );
    }

    #[test]
    fn ignores_incomplete_non_matching_lines() {
        let initial = JobProgress {
            total: 9,
            uploaded: 4,
            duplicates: 1,
            errors: 0,
        };
        let parsed = parse_line("uploaded :", initial.clone());
        assert_eq!(parsed.progress.total, initial.total);
        assert_eq!(parsed.progress.uploaded, initial.uploaded);
        assert_eq!(parsed.progress.duplicates, initial.duplicates);
        assert!(!parsed.has_error);
    }

    #[test]
    fn extracts_asset_id_from_line() {
        let initial = JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        };
        let parsed = parse_line(
            "uploaded : 1 assetId=123e4567-e89b-12d3-a456-426614174000",
            initial,
        );
        assert_eq!(
            parsed.completed_asset_id.as_deref(),
            Some("123e4567-e89b-12d3-a456-426614174000")
        );
    }

    #[test]
    fn parses_per_file_server_error() {
        let log = "2026-06-22 14:30:01 ERR server error file=/Volumes/CANON:DCIM/IMG_0001.JPG error=Internal Server Error (500)";
        let errors = parse_error_log(log);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file, "/Volumes/CANON:DCIM/IMG_0001.JPG");
        assert_eq!(errors[0].reason, "Internal Server Error (500)");
    }

    #[test]
    fn parses_path_with_spaces_and_trailing_attr() {
        let log = "2026-06-22 14:30:05 ERR incomplete processing file=/Volumes/My Card:DCIM/VID 7.MP4 error=asset never reached final state discovered_at=2026-06-22T14:29:00Z";
        let errors = parse_error_log(log);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file, "/Volumes/My Card:DCIM/VID 7.MP4");
        assert_eq!(errors[0].reason, "asset never reached final state");
    }

    #[test]
    fn ignores_non_error_and_aggregate_lines() {
        let log = "2026-06-22 14:30:00 INF uploaded successfully file=/x/IMG_0002.JPG\n\
2026-06-22 14:30:02 ERR failed to add assets to album error=boom album=Trip\n\
2026-06-22 14:30:03 WRN discarded not selected file=/x/note.txt reason=not a media file";
        // INF line, ERR-without-file (album), and WRN line are all excluded.
        assert!(parse_error_log(log).is_empty());
    }

    #[test]
    fn deduplicates_by_file_keeping_first_reason() {
        let log = "2026-06-22 14:30:01 ERR server error file=/x/IMG_0003.JPG error=first\n\
2026-06-22 14:30:09 ERR incomplete processing file=/x/IMG_0003.JPG error=second";
        let errors = parse_error_log(log);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].reason, "first");
    }

    #[test]
    fn caps_at_one_hundred_errors() {
        let mut log = String::new();
        for i in 0..150 {
            log.push_str(&format!(
                "2026-06-22 14:30:01 ERR server error file=/x/IMG_{i:04}.JPG error=boom\n"
            ));
        }
        assert_eq!(parse_error_log(&log).len(), 100);
    }
}
