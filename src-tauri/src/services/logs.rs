use std::{fs, io::Write, path::PathBuf};

pub fn logs_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| "Could not resolve local data directory".to_string())?;
    let dir = base.join("immich-shuttle").join("logs");
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create log directory: {e}"))?;
    Ok(dir)
}

pub fn append_log(file_name: &str, line: &str) -> Result<(), String> {
    let path = logs_dir()?.join(file_name);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Could not open log file: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("Could not write log line: {e}"))
}

pub fn rotate_recent_logs(max_files: usize) -> Result<(), String> {
    let dir = logs_dir()?;
    let mut entries = fs::read_dir(&dir)
        .map_err(|e| format!("Could not list logs directory: {e}"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
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
