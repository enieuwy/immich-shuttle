use regex::Regex;

use crate::models::job::JobProgress;

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

#[cfg(test)]
mod tests {
    use super::parse_line;
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
}
