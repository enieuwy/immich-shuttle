use std::collections::HashSet;

use crate::models::job::{FileError, JobProgress};

/// Maximum number of per-file errors retained from a single run, to keep the job
/// payload and the UI bounded on imports that fail en masse.
const MAX_FILE_ERRORS: usize = 100;
/// Maximum number of distinct failed paths tracked for progress. Progress error
/// counts saturate at this limit so a pathological run cannot retain unbounded
/// path strings.
const MAX_TRACKED_ERROR_PATHS: usize = 10_000;

/// Progress recovered from an immich-go run log.
///
/// In `--no-ui` mode immich-go (v0.32.0) writes per-file events ONLY to its
/// `--log-file` as console-slog; stdout carries just a `\r`-refreshed aggregate
/// line that cannot be read reliably through a pipe (no newline until the very
/// end). The run log is therefore the single source of truth for progress.
#[derive(Debug, Clone, Default)]
pub struct RunProgress {
    /// total / uploaded / duplicates / errors derived from the log.
    pub progress: JobProgress,
    /// Local filesystem paths of files uploaded successfully (safe-to-wipe
    /// candidates, re-verified against the server by SHA-1 before deletion).
    pub completed_paths: Vec<String>,
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
/// immich-go (v0.32.0) logs files as `<fsRoot>:<name>` — e.g.
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

/// The message of a console-slog INFO line (`YYYY-MM-DD HH:MM:SS INF <msg...>`),
/// or `None` if the line is not an INFO event. The timestamp is fixed-width, so
/// the level token sits at column 19. Indented summary-report lines (which begin
/// with extra spaces after `INF `) are intentionally returned with their leading
/// whitespace so callers' `starts_with` checks skip them.
fn info_line_message(line: &str) -> Option<&str> {
    const MARKER: &str = " INF ";
    let marker_end = 19 + MARKER.len();
    if line.len() < marker_end || !line.is_char_boundary(19) || !line.is_char_boundary(marker_end) {
        return None;
    }
    if &line[19..marker_end] != MARKER {
        return None;
    }
    Some(&line[marker_end..])
}

/// console-slog (NoColor) error line prefix: `YYYY-MM-DD HH:MM:SS ERR `.
fn is_error_line(line: &str) -> bool {
    const MARKER: &str = " ERR ";
    let marker_end = 19 + MARKER.len();
    line.len() >= marker_end
        && line.is_char_boundary(19)
        && line.is_char_boundary(marker_end)
        && &line[19..marker_end] == MARKER
}

/// Incremental accumulator for run-log progress.
///
/// immich-go's run log is append-only, so progress can be folded from only the
/// newly-appended bytes each tick instead of re-reading and re-parsing the whole
/// (multi-MB on large imports) log every time — keeping per-tick work
/// proportional to new output rather than total log size.
#[derive(Default)]
pub struct ProgressAccumulator {
    total: u32,
    uploaded: u32,
    duplicates: u32,
    // Both freshly uploaded files AND files the server already holds are safe to
    // wipe from the local source (a server copy exists either way), so both feed
    // completed_paths; `uploaded` stays a separate tally for the progress
    // counter. Deletion is still gated by a SHA-1 existence check downstream.
    completed_paths: Vec<String>,
    seen_paths: HashSet<String>,
    /// Distinct per-file failures, bounded to prevent a pathological run from
    /// retaining an unbounded number of path strings.
    failed_paths: HashSet<String>,
    /// Number of distinct failed paths tracked, capped at
    /// `MAX_TRACKED_ERROR_PATHS`.
    errors: u32,
    /// A trailing line not yet terminated by '\n'; reparsed once its rest arrives.
    pending: String,
}

impl ProgressAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a chunk of newly-read log text. Only complete lines (terminated by
    /// '\n') are parsed; a trailing partial line is buffered for the next call.
    pub fn push_chunk(&mut self, chunk: &str) {
        self.pending.push_str(chunk);
        while let Some(nl) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=nl).collect();
            self.apply_line(line.trim_end_matches(['\r', '\n']));
        }
    }

    /// Flush a buffered final line that never received a trailing newline. Call
    /// once the run has ended.
    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.apply_line(line.trim_end_matches(['\r', '\n']));
        }
    }

    /// Lightweight view for a live progress tick. It avoids cloning the full
    /// completed path list, which is only needed after the run for wipe
    /// accounting.
    pub fn progress_view(&self) -> (JobProgress, Option<&str>) {
        (
            JobProgress {
                total: self.total,
                uploaded: self.uploaded,
                duplicates: self.duplicates,
                errors: self.errors,
            },
            self.completed_paths.last().map(String::as_str),
        )
    }

    pub fn snapshot(&self) -> RunProgress {
        let (progress, _) = self.progress_view();
        RunProgress {
            progress,
            completed_paths: self.completed_paths.clone(),
        }
    }

    fn apply_line(&mut self, line: &str) {
        if is_error_line(line) {
            if let Some(file) = attr_value(line, "file", &["error", "reason", "discovered_at"])
                .filter(|f| !f.is_empty())
            {
                if self.failed_paths.len() < MAX_TRACKED_ERROR_PATHS
                    && self.failed_paths.insert(file)
                {
                    self.errors = self.errors.saturating_add(1);
                }
            }
            return;
        }
        let Some(message) = info_line_message(line) else {
            return;
        };
        if message.starts_with("uploaded successfully") {
            if let Some(file) = attr_value(line, "file", &[]).filter(|f| !f.is_empty()) {
                let path = fs_path_from_file_attr(&file);
                if self.seen_paths.insert(path.clone()) {
                    self.completed_paths.push(path);
                    self.uploaded = self.uploaded.saturating_add(1);
                }
            }
        } else if message.starts_with("server has duplicate") {
            self.duplicates = self.duplicates.saturating_add(1);
            if let Some(file) = attr_value(line, "file", &[]).filter(|f| !f.is_empty()) {
                let path = fs_path_from_file_attr(&file);
                if self.seen_paths.insert(path.clone()) {
                    self.completed_paths.push(path);
                }
            }
        } else if message.starts_with("discovered image") || message.starts_with("discovered video")
        {
            self.total = self.total.saturating_add(1);
        }
    }
}

/// Derive progress + completed-upload paths from a full immich-go run log.
///
/// Counts per-asset event lines (not the indented end-of-run summary, whose
/// messages start with extra whitespace):
/// - `discovered image` / `discovered video` -> total assets found
/// - `uploaded successfully` -> uploaded (de-duplicated, with real fs path)
/// - `server has duplicate` -> duplicates already on the server
/// - ERROR lines carrying a `file=` -> per-file upload error events
pub fn parse_run_progress(contents: &str) -> RunProgress {
    let mut acc = ProgressAccumulator::new();
    acc.push_chunk(contents);
    acc.finish();
    acc.snapshot()
}

/// Parse per-file upload failures out of immich-go's run log (written via
/// `--log-file`). Only ERROR-level lines that carry a `file=` attribute are
/// reported; aggregate/directory-scan errors (no `file=`) are surfaced as a
/// count only. Failures are deduplicated by file, preserving the first reason.
pub fn parse_error_log(contents: &str) -> Vec<FileError> {
    let mut out: Vec<FileError> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for line in contents.lines() {
        if !is_error_line(line) {
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
    use super::{
        info_line_message, is_error_line, parse_error_log, parse_run_progress, ProgressAccumulator,
    };

    // A realistic console-slog run-log slice captured from immich-go v0.32.0
    // `--no-ui --log-level DEBUG` output: per-asset events plus the directory
    // scan ERR noise and the indented end-of-run summary report.
    const LOG: &str = "2026-06-24 16:09:14 INF immich-go version:0.32.0\n\
2026-06-24 16:09:14 ERR @tmp: file does not exist\n\
2026-06-24 16:09:14 ERR PRIVATE/AVCHD/BDMV/STREAM: file does not exist\n\
2026-06-24 16:09:30 INF discovered image file=Untitled:DCIM/100MSDCF/DSC09008.ARW\n\
2026-06-24 16:09:30 INF discovered image file=Untitled:DCIM/100MSDCF/DSC09009.ARW\n\
2026-06-24 16:09:30 INF discovered video file=Untitled:PRIVATE/M4ROOT/CLIP/C0045.MP4\n\
2026-06-24 16:09:37 INF server has duplicate file=Untitled:DCIM/100MSDCF/DSC08936.ARW\n\
2026-06-24 16:10:21 INF uploaded successfully file=Untitled:DCIM/100MSDCF/DSC09008.ARW\n\
2026-06-24 16:10:22 INF uploaded successfully file=Untitled:DCIM/100MSDCF/DSC09009.ARW\n\
2026-06-24 16:10:30 ERR server error file=Untitled:DCIM/100MSDCF/DSC09010.ARW error=Internal Server Error (500)\n\
2026-06-24 16:10:32 INF   discovered image                   :     193  (3.8 GB)\n\
2026-06-24 16:10:32 INF   uploaded successfully              :      50  (3.3 GB)\n\
2026-06-24 16:10:32 INF   server has duplicate               :     144  (2.9 GB)";

    #[test]
    fn counts_events_not_summary_report() {
        let p = parse_run_progress(LOG).progress;
        // 2 discovered image + 1 discovered video event lines (NOT the indented
        // "discovered image : 193" summary line).
        assert_eq!(p.total, 3);
        assert_eq!(p.uploaded, 2);
        assert_eq!(p.duplicates, 1);
        // One ERR carries file= (real per-file error); the two "file does not
        // exist" directory-scan ERRs do not and are excluded.
        assert_eq!(p.errors, 1);
    }

    #[test]
    fn completed_paths_include_uploads_and_server_duplicates() {
        // Duplicates already on the server are safe-to-wipe candidates too, so
        // they join the completed paths (in log order: the duplicate line comes
        // before the two uploads) while `uploaded` stays a separate count.
        let run = parse_run_progress(LOG);
        assert_eq!(
            run.completed_paths,
            vec![
                "Untitled/DCIM/100MSDCF/DSC08936.ARW",
                "Untitled/DCIM/100MSDCF/DSC09008.ARW",
                "Untitled/DCIM/100MSDCF/DSC09009.ARW",
            ]
        );
        assert_eq!(run.progress.uploaded, 2);
        assert_eq!(run.progress.duplicates, 1);
    }

    #[test]
    fn empty_log_is_all_zero() {
        let p = parse_run_progress("").progress;
        assert_eq!(p.total, 0);
        assert_eq!(p.uploaded, 0);
        assert_eq!(p.duplicates, 0);
        assert_eq!(p.errors, 0);
    }

    #[test]
    fn converts_fsroot_colon_and_windows_drive() {
        let log =
            "2026-06-24 16:10:00 INF uploaded successfully file=/Volumes/CANON:DCIM/IMG_0001.JPG\n\
2026-06-24 16:10:01 INF uploaded successfully file=C:\\DCIM:IMG_0002.JPG";
        let run = parse_run_progress(log);
        assert_eq!(
            run.completed_paths,
            vec!["/Volumes/CANON/DCIM/IMG_0001.JPG", "C:\\DCIM/IMG_0002.JPG"]
        );
    }

    #[test]
    fn handles_paths_with_spaces() {
        let log =
            "2026-06-24 16:10:05 INF uploaded successfully file=/Volumes/My Card:DCIM/VID 7.MP4";
        let run = parse_run_progress(log);
        assert_eq!(run.completed_paths, vec!["/Volumes/My Card/DCIM/VID 7.MP4"]);
    }

    #[test]
    fn deduplicates_repeated_uploaded_paths() {
        let log = "2026-06-24 16:10:00 INF uploaded successfully file=Card:IMG_0001.JPG\n\
2026-06-24 16:10:09 INF uploaded successfully file=Card:IMG_0001.JPG";
        let run = parse_run_progress(log);
        assert_eq!(run.completed_paths.len(), 1);
        assert_eq!(run.progress.uploaded, 1);
    }

    // ---- per-file error detail list (parse_error_log) ----

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
    fn excludes_directory_scan_errors_without_file() {
        let log = "2026-06-24 16:09:14 ERR @tmp: file does not exist\n\
2026-06-24 16:09:14 ERR PRIVATE/AVCHD/BDMV/STREAM: file does not exist";
        assert!(parse_error_log(log).is_empty());
        assert_eq!(parse_run_progress(log).progress.errors, 0);
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
    fn progress_deduplicates_repeated_error_paths() {
        let log = "2026-06-22 14:30:01 ERR server error file=/x/IMG_0003.JPG error=first\n\
2026-06-22 14:30:09 ERR incomplete processing file=/x/IMG_0003.JPG error=second";
        assert_eq!(parse_run_progress(log).progress.errors, 1);
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

    // ---- incremental progress accumulator (ProgressAccumulator) ----

    #[test]
    fn incremental_chunks_match_full_parse() {
        let full = parse_run_progress(LOG);
        // Feed the log one byte at a time (each chunk splits lines arbitrarily);
        // the running snapshot must equal a whole-log parse.
        let mut acc = ProgressAccumulator::new();
        let mut buf = [0u8; 4];
        for ch in LOG.chars() {
            acc.push_chunk(ch.encode_utf8(&mut buf));
        }
        acc.finish();
        let inc = acc.snapshot();
        assert_eq!(inc.progress.total, full.progress.total);
        assert_eq!(inc.progress.uploaded, full.progress.uploaded);
        assert_eq!(inc.progress.duplicates, full.progress.duplicates);
        assert_eq!(inc.progress.errors, full.progress.errors);
        assert_eq!(inc.completed_paths, full.completed_paths);
    }

    #[test]
    fn finish_flushes_trailing_line_without_newline() {
        // The final "uploaded successfully" line has no trailing '\n'; it must
        // only be counted after finish(), not while still buffered.
        let mut acc = ProgressAccumulator::new();
        acc.push_chunk("2026-06-24 16:10:21 INF uploaded successfully file=Card:IMG_0001.JPG");
        assert_eq!(acc.snapshot().progress.uploaded, 0);
        acc.finish();
        assert_eq!(acc.snapshot().progress.uploaded, 1);
    }

    #[test]
    fn malformed_non_ascii_level_boundary_is_not_an_event() {
        // The byte offset after a console-slog level token lands inside `é`.
        // Parsing this must reject the line rather than slicing at a non-boundary.
        let line = "1234567890123456789abcdé ERR file=Card:IMG_0001.JPG";
        assert!(!is_error_line(line));
        assert_eq!(info_line_message(line), None);
        assert_eq!(parse_run_progress(line).progress.errors, 0);
    }
}
