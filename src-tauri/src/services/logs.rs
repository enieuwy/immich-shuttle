use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

/// Serializes append+trim so a concurrent append can't be clobbered by another
/// thread's in-place trim rewrite (read-old, write-truncated) race.
static LOG_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
    // Lossy on purpose: a single mangled byte in a log line must not hide the
    // whole log from the viewer.
    let content = fs::read(&path).map_err(|e| format!("Could not read log file: {e}"))?;
    Ok(tail_lines(&String::from_utf8_lossy(&content), max_lines))
}

pub fn append_log(file_name: &str, line: &str) -> Result<(), String> {
    let _guard = LOG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    // all `read_recent` (last 500 lines) ever needs. A trim error here is an I/O
    // failure on the log volume itself, which no log line could report, so the
    // line that was already written stands and the next append retries.
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > APP_LOG_MAX_BYTES {
        let _ = trim_to_trailing_lines(&path, APP_LOG_KEEP_LINES);
    }
    Ok(())
}

/// Size threshold past which a durable log is trimmed to its trailing window.
const APP_LOG_MAX_BYTES: u64 = 1_000_000;
/// Number of most-recent lines retained when a durable log is trimmed.
const APP_LOG_KEEP_LINES: usize = 5_000;

/// The last `keep` lines of `content`, as raw bytes.
///
/// Byte-oriented on purpose: one non-UTF-8 byte in the log - a mangled path, a
/// truncated write - must not make the file untrimmable, because the trim is the
/// only thing bounding its growth.
fn tail_bytes(content: &[u8], keep: usize) -> &[u8] {
    // A trailing newline terminates the last line rather than starting another.
    let end = content.strip_suffix(b"\n").unwrap_or(content).len();
    let mut seen = 0;
    for (index, byte) in content[..end].iter().enumerate().rev() {
        if *byte != b'\n' {
            continue;
        }
        seen += 1;
        if seen == keep {
            return &content[index + 1..];
        }
    }
    content
}

/// Rewrite `path` in place keeping only its last `keep` lines.
fn trim_to_trailing_lines(path: &std::path::Path, keep: usize) -> Result<(), String> {
    let content = fs::read(path).map_err(|e| format!("Could not read log file: {e}"))?;
    let mut trimmed = tail_bytes(&content, keep).to_vec();
    if !trimmed.ends_with(b"\n") {
        trimmed.push(b'\n');
    }
    // Preserve owner-only perms established on the logs dir; a plain write keeps
    // the existing file mode.
    fs::write(path, trimmed).map_err(|e| format!("Could not trim log file: {e}"))
}

/// Exactly the per-run log files this app writes: `run-<canonical UUID>.log`.
///
/// The logs directory is user-visible — the UI opens it in the file manager —
/// so deletion must be provably scoped to our own artifacts. Startup pruning
/// and rotation both delete by this one predicate: anything else in that
/// directory belongs to the user, and never enters a retention budget either.
pub(crate) fn is_run_log_name(name: &str) -> bool {
    name.strip_prefix("run-")
        .and_then(|name| name.strip_suffix(".log"))
        .is_some_and(crate::is_canonical_uuid)
}

// Keep directory traversal separate from `logs_dir()` so tests can rotate an isolated directory.
fn rotate_dir(dir: &Path, max_files: usize) -> Result<(), String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|e| format!("Could not list logs directory: {e}"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        // Rotation caps the per-run logs only. app.log is the single durable log
        // the UI reads and must survive regardless of its relative age, and an
        // unrelated file is not ours to delete at all.
        .filter(|entry| is_run_log_name(&entry.file_name().to_string_lossy()))
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

pub fn rotate_recent_logs(max_files: usize) -> Result<(), String> {
    rotate_dir(&logs_dir()?, max_files)
}

#[cfg(test)]
mod tests {
    use super::{rotate_dir, tail_lines, trim_to_trailing_lines};
    use std::io::Write;
    use std::{
        fs,
        path::Path,
        time::{Duration, SystemTime},
    };
    use uuid::Uuid;

    /// Pins a fixture's modification time. Rotation orders by `modified()`, so
    /// the age order a test asserts on must be stated outright: writing the
    /// files in sequence only orders them when the filesystem's timestamp
    /// resolution is finer than the gap between the writes, and tied keys leave
    /// the unspecified `read_dir` order deciding which files rotation deletes.
    fn set_mtime(path: &Path, seconds_from_epoch: u64) {
        fs::File::open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds_from_epoch))
            .unwrap();
    }

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

    #[test]
    fn a_mangled_byte_does_not_make_the_log_untrimmable() {
        // The trim is the only bound on app.log's growth, and it used to require
        // the whole file to be valid UTF-8. One bad byte - a mangled path from a
        // removable card, a torn write - would then fail every later trim, so the
        // log grew without limit while each append re-read all of it.
        let dir = std::env::temp_dir().join(format!("logs-invalid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        let mut raw = Vec::new();
        for i in 0..500 {
            raw.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        raw.extend_from_slice(b"copy failed: /Volumes/CARD/\xFF\xFE.jpg\n");
        for i in 500..1_000 {
            raw.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        std::fs::write(&path, &raw).unwrap();
        assert!(
            std::fs::read_to_string(&path).is_err(),
            "fixture must not be valid UTF-8"
        );

        trim_to_trailing_lines(&path, 100).unwrap();

        let content = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 100);
        assert_eq!(lines.first().copied(), Some("line 900"));
        assert_eq!(lines.last().copied(), Some("line 999"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Rotation deletes only this app's own oldest run logs. The logs directory is
    // user-visible, so app.log, a user's own files, and anything whose name we
    // cannot prove we wrote must survive - and must not consume the budget that
    // decides how many real run logs are kept.
    #[test]
    fn log_rotation_only_removes_this_apps_oldest_run_logs() {
        let dir = std::env::temp_dir().join(format!("logs-rotate-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        // Written first, so an age-ordered sweep would reach these before any
        // run log: none of them is ours to delete.
        let bystanders = ["app.log", "notes.txt", "crash.log", ".DS_Store"];
        for (index, name) in bystanders.iter().enumerate() {
            fs::write(dir.join(name), format!("{name} content\n")).unwrap();
            set_mtime(&dir.join(name), 1_700_000_000 + index as u64);
        }
        // Malformed: the run-log shape without a canonical uuid, so not ours.
        fs::write(dir.join("run-upload.log"), "hand-written\n").unwrap();
        set_mtime(&dir.join("run-upload.log"), 1_700_000_100);

        let run_logs: Vec<String> = (0..5)
            .map(|index| {
                let name = format!("run-{}.log", Uuid::new_v4());
                fs::write(dir.join(&name), format!("run {index}\n")).unwrap();
                set_mtime(&dir.join(&name), 1_700_000_200 + index as u64 * 60);
                name
            })
            .collect();

        rotate_dir(&dir, 2).unwrap();

        for name in bystanders {
            assert!(dir.join(name).exists(), "{name} must survive rotation");
        }
        assert!(dir.join("run-upload.log").exists());
        for name in &run_logs[..3] {
            assert!(!dir.join(name).exists(), "{name} is the oldest and must go");
        }
        for name in &run_logs[3..] {
            assert!(dir.join(name).exists(), "{name} is newest and must stay");
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
