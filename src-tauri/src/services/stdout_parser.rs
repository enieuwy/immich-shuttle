use regex::Regex;

use std::collections::HashSet;

use crate::models::job::{FileError, JobProgress};

/// Parse immich-go's `--no-ui` aggregate progress line from stdout.
///
/// In `--no-ui` mode immich-go (v0.31.0, `app/upload/noui.go`) writes ONLY an
/// aggregate progress line to stdout, refreshed in place with a leading `\r`
/// and no trailing newline until the final flush:
///
/// ```text
/// Immich read 100%, Assets found: 15, Upload errors: 2, Uploaded 13 .
/// ```
///
/// Because the line is rewritten with `\r` (not `\n`), a single read may contain
/// several `\r`-separated copies; the counts are monotonic, so the last match in
/// the chunk holds the freshest totals. Per-file events (paths, duplicates,
/// errors) are NOT on stdout — they go to the `--log-file` and are read by
/// [`parse_completed_assets`] / [`parse_error_log`].
pub fn parse_line(line: &str, mut current: JobProgress) -> JobProgress {
    let progress_re =
        Regex::new(r"Assets found:\s*(\d+),\s*Upload errors:\s*\d+,\s*Uploaded\s+(\d+)")
            .expect("valid regex");
    if let Some(c) = progress_re.captures_iter(line).last() {
        if let Some(total) = c.get(1).and_then(|m| m.as_str().parse::<u32>().ok()) {
            current.total = total;
        }
        if let Some(uploaded) = c.get(2).and_then(|m| m.as_str().parse::<u32>().ok()) {
            current.uploaded = uploaded;
        }
    }
    current
}

/// Maximum number of per-file errors retained from a single run, to keep the job
/// payload and the UI bounded on imports that fail en masse.
const MAX_FILE_ERRORS: usize = 100;

/// Assets immich-go confirmed it placed on the server during a run, recovered
/// from the run log.
#[derive(Debug, Clone, Default)]
pub struct CompletedAssets {
    /// Local filesystem paths of files uploaded successfully (safe-to-wipe
    /// candidates, re-verified against the server by SHA-1 before deletion).
    pub paths: Vec<String>,
    /// Count of files the server already held (`server has duplicate`).
    pub duplicates: u32,
}

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

/// Convert immich-go's logged `file=` value into a real local filesystem path.
///
/// immich-go (v0.31.0) logs files as `<fsRoot>:<name>` — e.g.
/// `/Volumes/CANON:DCIM/IMG_0001.JPG` — where `fsRoot` is the source path passed
/// to it and `name` is the path within. POSIX roots contain no colon, so the
/// first colon is the separator; on Windows the root carries a drive colon
/// (`C:\DCIM`), so the search skips a leading `<drive>:` first.
fn fs_path_from_file_attr(file: &str) -> String {
    let search_from = if is_windows_drive_prefix(file) { 2 } else { 0 };
    if let Some(rel) = file.get(search_from..).and_then(|s| s.find(':')) {
        let idx = search_from + rel;
        let root = &file[..idx];
        let name = &file[idx + 1..];
        if !root.is_empty() && !name.is_empty() {
            return format!("{root}/{name}");
        }
    }
    file.to_string()
}

/// Whether `s` begins with a Windows drive prefix like `C:\` or `C:/`.
fn is_windows_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// console-slog (NoColor, `TimeFormat: time.DateTime`) line prefix:
/// `YYYY-MM-DD HH:MM:SS <LVL> `.
fn info_line_message(line: &str) -> Option<&str> {
    // console-slog NoColor renders "YYYY-MM-DD HH:MM:SS INF <message...>"; the
    // 19-char timestamp is fixed-width, so the level token sits at column 19.
    const MARKER: &str = " INF ";
    if line.len() < 19 + MARKER.len() || !line.is_char_boundary(19) {
        return None;
    }
    if &line[19..19 + MARKER.len()] != MARKER {
        return None;
    }
    Some(&line[19 + MARKER.len()..])
}

/// Recover assets immich-go confirmed during a run from its `--log-file`.
///
/// Scans console-slog INFO lines for `uploaded successfully` (collecting the
/// local path, de-duplicated) and counts `server has duplicate`. The log is
/// opened `O_APPEND` by immich-go, so a single run file may span several
/// invocations — call this ONCE on the full file after the run completes.
pub fn parse_completed_assets(contents: &str) -> CompletedAssets {
    let mut out = CompletedAssets::default();
    let mut seen: HashSet<String> = HashSet::new();

    for line in contents.lines() {
        let Some(message) = info_line_message(line) else {
            continue;
        };
        if message.starts_with("uploaded successfully") {
            if let Some(file) = attr_value(line, "file", &[]).filter(|f| !f.is_empty()) {
                let path = fs_path_from_file_attr(&file);
                if seen.insert(path.clone()) {
                    out.paths.push(path);
                }
            }
        } else if message.starts_with("server has duplicate") {
            out.duplicates = out.duplicates.saturating_add(1);
        }
    }

    out
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
    use super::{parse_completed_assets, parse_error_log, parse_line};
    use crate::models::job::JobProgress;

    fn zero() -> JobProgress {
        JobProgress {
            total: 0,
            uploaded: 0,
            duplicates: 0,
            errors: 0,
        }
    }

    // ---- stdout aggregate progress line (app/upload/noui.go) ----

    #[test]
    fn parses_real_noui_progress_line() {
        let line = "Immich read 100%, Assets found: 15, Upload errors: 2, Uploaded 13 .";
        let p = parse_line(line, zero());
        assert_eq!(p.total, 15);
        assert_eq!(p.uploaded, 13);
    }

    #[test]
    fn takes_latest_of_carriage_return_refreshed_copies() {
        // A single stdout read often holds several '\r'-separated refreshes.
        let chunk = "\rImmich read 40%, Assets found: 8, Upload errors: 0, Uploaded 5  \
                     \rImmich read 100%, Assets found: 15, Upload errors: 0, Uploaded 15 .";
        let p = parse_line(chunk, zero());
        assert_eq!(p.total, 15);
        assert_eq!(p.uploaded, 15);
    }

    #[test]
    fn leaves_progress_untouched_on_banner_and_noise() {
        let initial = JobProgress {
            total: 9,
            uploaded: 4,
            duplicates: 1,
            errors: 0,
        };
        for line in [
            "immich-go v0.31.0",
            "Running environment: architecture=arm64 os=darwin",
            "",
        ] {
            let p = parse_line(line, initial.clone());
            assert_eq!(p.total, initial.total);
            assert_eq!(p.uploaded, initial.uploaded);
        }
    }

    // ---- run-log completed assets (console-slog, app/log.go + fileevents.go) ----

    #[test]
    fn collects_uploaded_paths_and_converts_fsroot_colon() {
        let log =
            "2026-06-22 14:30:00 INF uploaded successfully file=/Volumes/CANON:DCIM/IMG_0001.JPG";
        let c = parse_completed_assets(log);
        assert_eq!(c.paths, vec!["/Volumes/CANON/DCIM/IMG_0001.JPG"]);
        assert_eq!(c.duplicates, 0);
    }

    #[test]
    fn handles_paths_with_spaces() {
        let log =
            "2026-06-22 14:30:05 INF uploaded successfully file=/Volumes/My Card:DCIM/VID 7.MP4";
        let c = parse_completed_assets(log);
        assert_eq!(c.paths, vec!["/Volumes/My Card/DCIM/VID 7.MP4"]);
    }

    #[test]
    fn converts_windows_drive_paths() {
        let log = "2026-06-22 14:30:00 INF uploaded successfully file=C:\\DCIM:IMG_0001.JPG";
        let c = parse_completed_assets(log);
        assert_eq!(c.paths, vec!["C:\\DCIM/IMG_0001.JPG"]);
    }

    #[test]
    fn counts_server_duplicates_without_treating_them_as_uploaded() {
        let log =
            "2026-06-22 14:30:01 INF server has duplicate file=/Volumes/CANON:DCIM/IMG_0002.JPG";
        let c = parse_completed_assets(log);
        assert!(c.paths.is_empty());
        assert_eq!(c.duplicates, 1);
    }

    #[test]
    fn deduplicates_repeated_uploaded_paths() {
        let log =
            "2026-06-22 14:30:00 INF uploaded successfully file=/Volumes/CANON:DCIM/IMG_0001.JPG\n\
2026-06-22 14:30:09 INF uploaded successfully file=/Volumes/CANON:DCIM/IMG_0001.JPG";
        let c = parse_completed_assets(log);
        assert_eq!(c.paths.len(), 1);
    }

    #[test]
    fn ignores_informational_noise_lines() {
        // Startup banner/flags/report lines are also INF but are not file events.
        let log = "2026-06-22 14:29:59 INF Assets on the server: 42\n\
2026-06-22 14:30:00 INF got album from the server album=Trip assets=3\n\
2026-06-22 14:30:00 INF Running environment: architecture=arm64 os=darwin";
        let c = parse_completed_assets(log);
        assert!(c.paths.is_empty());
        assert_eq!(c.duplicates, 0);
    }

    #[test]
    fn parses_mixed_run_log_for_completed_and_errors() {
        let log = "2026-06-22 14:30:00 INF uploaded successfully file=/Volumes/CANON:DCIM/IMG_0001.JPG\n\
2026-06-22 14:30:01 INF server has duplicate file=/Volumes/CANON:DCIM/IMG_0002.JPG\n\
2026-06-22 14:30:02 INF uploaded successfully file=/Volumes/CANON:DCIM/IMG_0003.JPG\n\
2026-06-22 14:30:03 ERR server error file=/Volumes/CANON:DCIM/IMG_0004.JPG error=Internal Server Error (500)";
        let c = parse_completed_assets(log);
        assert_eq!(
            c.paths,
            vec![
                "/Volumes/CANON/DCIM/IMG_0001.JPG",
                "/Volumes/CANON/DCIM/IMG_0003.JPG",
            ]
        );
        assert_eq!(c.duplicates, 1);

        let errors = parse_error_log(log);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file, "/Volumes/CANON:DCIM/IMG_0004.JPG");
        assert_eq!(errors[0].reason, "Internal Server Error (500)");
    }

    // ---- run-log per-file errors (unchanged behaviour, kept for regression) ----

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
