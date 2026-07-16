use std::{fs, io::Write, path::PathBuf};

pub fn logs_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| "Could not resolve local data directory".to_string())?;
    let dir = base.join("immich-shuttle").join("logs");
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create log directory: {e}"))?;
    // Run logs can contain an immich-go x-api-key header at higher verbosity;
    // keep the directory owner-only so other local users cannot read them.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

fn tail_lines(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

pub fn read_recent(file_name: &str, max_lines: usize) -> Result<String, String> {
    let path = logs_dir()?.join(file_name);
    if !path.exists() {
        return Ok(String::new());
    }
    let content = fs::read_to_string(&path).map_err(|e| format!("Could not read log file: {e}"))?;
    Ok(tail_lines(&content, max_lines))
}

pub fn append_log(file_name: &str, line: &str) -> Result<(), String> {
    let path = logs_dir()?.join(file_name);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Could not open log file: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("Could not write log line: {e}"))?;

    // app.log is the single durable log and is deliberately never rotated away,
    // so cap its unbounded growth here: once it crosses a size threshold, trim
    // it to the newest APP_LOG_KEEP_LINES. The metadata stat runs on every
    // append (cheap); the rewrite is rare and preserves recent history, which is
    // all `read_recent` (last 500 lines) ever needs.
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > APP_LOG_MAX_BYTES {
        let _ = trim_to_trailing_lines(&path, APP_LOG_KEEP_LINES);
    }
    Ok(())
}

/// Size threshold past which a durable log is trimmed to its trailing window.
const APP_LOG_MAX_BYTES: u64 = 1_000_000;
/// Number of most-recent lines retained when a durable log is trimmed.
const APP_LOG_KEEP_LINES: usize = 5_000;

/// Rewrite `path` in place keeping only its last `keep` lines.
fn trim_to_trailing_lines(path: &std::path::Path, keep: usize) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| format!("Could not read log file: {e}"))?;
    let trimmed = tail_lines(&content, keep);
    // Preserve owner-only perms established on the logs dir; a plain write keeps
    // the existing file mode.
    fs::write(path, format!("{trimmed}\n")).map_err(|e| format!("Could not trim log file: {e}"))
}

pub fn rotate_recent_logs(max_files: usize) -> Result<(), String> {
    let dir = logs_dir()?;
    let mut entries = fs::read_dir(&dir)
        .map_err(|e| format!("Could not list logs directory: {e}"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        // Never rotate away the persistent app log. Rotation is meant to cap the
        // per-run `run-<job>.log` files; app.log is the single durable log the
        // UI reads and must survive regardless of its relative age.
        .filter(|entry| entry.file_name().to_string_lossy() != "app.log")
        .collect::<Vec<_>>();

    entries.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
    if entries.len() <= max_files {
        return Ok(());
    }

    let to_delete = entries.len() - max_files;
    for entry in entries.into_iter().take(to_delete) {
        let _ = fs::remove_file(entry.path());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{tail_lines, trim_to_trailing_lines};
    use std::io::Write;

    #[test]
    fn tail_lines_returns_recent_lines() {
        let content = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10";

        assert_eq!(tail_lines(content, 3), "8\n9\n10");
    }

    #[test]
    fn tail_lines_returns_all_when_under_limit() {
        assert_eq!(tail_lines("a\nb", 10), "a\nb");
    }

    #[test]
    fn trim_to_trailing_lines_caps_file_and_keeps_newest() {
        let dir = std::env::temp_dir().join(format!("logs-trim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..1_000 {
            writeln!(f, "line {i}").unwrap();
        }
        drop(f);

        trim_to_trailing_lines(&path, 100).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 100);
        assert_eq!(lines.first().copied(), Some("line 900"));
        assert_eq!(lines.last().copied(), Some("line 999"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
