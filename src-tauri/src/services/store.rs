use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::models::history::ImportRecord;

static STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Lock the store mutex, recovering the guard if a previous holder panicked.
/// Poisoning only signals that some earlier operation aborted mid-flight; the
/// store data itself lives on disk, so a single panic must not permanently
/// brick every future history/metadata operation for the session.
fn lock_store() -> std::sync::MutexGuard<'static, ()> {
    STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    history: Vec<ImportRecord>,
    #[serde(default)]
    sources: HashMap<String, SourceMeta>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SourceMeta {
    last_imported_at: i64,
    last_total: u32,
}

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not resolve app data directory: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create app data directory: {e}"))?;
    Ok(dir.join("store.json"))
}

fn load(app: &tauri::AppHandle) -> Result<StoreData, String> {
    let path = store_path(app)?;
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str::<StoreData>(&raw)
            .map_err(|e| format!("Could not parse store at {}: {e}", path.display())),
        // A missing file is the first-run case, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreData::default()),
        // File exists but is locked/unreadable — refuse to fall back to an empty
        // store, or the next save would overwrite the user's real history.
        Err(e) => Err(format!("Could not read store at {}: {e}", path.display())),
    }
}

fn save(app: &tauri::AppHandle, data: &StoreData) -> Result<(), String> {
    let path = store_path(app)?;
    let tmp = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(data)
        .map_err(|e| format!("Could not serialize store: {e}"))?;
    fs::write(&tmp, &content).map_err(|e| format!("Could not write temp store: {e}"))?;
    if fs::rename(&tmp, &path).is_err() {
        let fallback =
            fs::write(&path, content).map_err(|e| format!("Could not persist store: {e}"));
        let _ = fs::remove_file(&tmp);
        fallback?;
    }
    Ok(())
}

pub fn append_history(app: &tauri::AppHandle, record: ImportRecord) -> Result<(), String> {
    let _guard = lock_store();

    let mut data = load(app)?;
    let key = source_key(&record.source_paths);
    data.sources.insert(
        key,
        SourceMeta {
            last_imported_at: record.finished_at,
            last_total: record.total,
        },
    );
    data.history.insert(0, record);
    data.history.truncate(100);

    save(app, &data)
}

pub fn list_history(app: &tauri::AppHandle) -> Result<Vec<ImportRecord>, String> {
    let _guard = lock_store();

    let mut history = load(app)?.history;
    history.sort_by_key(|record| std::cmp::Reverse(record.finished_at));
    Ok(history)
}

pub fn clear_history(app: &tauri::AppHandle) -> Result<(), String> {
    let _guard = lock_store();
    // A corrupt/unparseable store must still be resettable — fall back to an
    // empty store so "Clear history" can overwrite and repair it instead of
    // propagating the parse error and leaving the user permanently stuck.
    let mut data = load(app).unwrap_or_default();
    clear_store_data(&mut data);
    save(app, &data)
}

fn clear_store_data(data: &mut StoreData) {
    data.history.clear();
    data.sources.clear();
}

pub fn last_import_for(app: &AppHandle, source_paths: &[String]) -> Option<i64> {
    let _guard = lock_store();

    load(app)
        .ok()?
        .sources
        .get(&source_key(source_paths))
        .map(|source| source.last_imported_at)
}

// Changing this normalization changes persisted keys and resets existing `last_import` associations.
fn source_key(paths: &[String]) -> String {
    let mut normalized: Vec<String> = paths
        .iter()
        .map(|path| normalize_source_path(path))
        .collect();
    normalized.sort();
    normalized.join("\n")
}

fn normalize_source_path(path: &str) -> String {
    #[cfg(windows)]
    let path = PathBuf::from(path.replace('\\', "/"));
    #[cfg(not(windows))]
    let path = PathBuf::from(path);

    let normalized = fs::canonicalize(&path).unwrap_or(path);
    let normalized = normalized.to_string_lossy();
    #[cfg(windows)]
    let normalized = {
        let normalized = normalized.replace('\\', "/");
        normalized
            .strip_prefix("//?/UNC/")
            .map(|path| format!("//{path}"))
            .or_else(|| normalized.strip_prefix("//?/").map(str::to_owned))
            .unwrap_or(normalized)
    };
    #[cfg(not(windows))]
    let normalized = normalized.as_ref();
    let trimmed = normalized.trim_end_matches('/');

    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{clear_store_data, source_key, SourceMeta, StoreData};

    #[test]
    fn clear_history_resets_source_metadata() {
        let mut data = StoreData {
            history: Vec::new(),
            sources: HashMap::from([(
                "source".to_string(),
                SourceMeta {
                    last_imported_at: 1,
                    last_total: 1,
                },
            )]),
        };

        clear_store_data(&mut data);

        assert!(data.history.is_empty());
        assert!(data.sources.is_empty());
    }

    #[test]
    fn source_key_normalizes_trailing_slashes_and_order() {
        let canonical_form = vec![
            "__store_key_test__/second".to_string(),
            "__store_key_test__/first".to_string(),
        ];
        let alternate_form = vec![
            "__store_key_test__/first/".to_string(),
            "__store_key_test__/second/".to_string(),
        ];

        assert_eq!(source_key(&canonical_form), source_key(&alternate_form));
    }

    #[cfg(windows)]
    #[test]
    fn source_key_normalizes_windows_separators() {
        assert_eq!(
            source_key(&["__store_key_test__/first".to_string()]),
            source_key(&["__store_key_test__\\first\\".to_string()])
        );
    }

    #[cfg(windows)]
    #[test]
    fn source_key_normalizes_windows_verbatim_prefix() {
        assert_eq!(
            source_key(&["//?/C:/Path".to_string()]),
            source_key(&["C:/Path".to_string()])
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn source_key_preserves_backslashes_in_unix_filenames() {
        assert_ne!(
            source_key(&["__store_key_test__/first".to_string()]),
            source_key(&["__store_key_test__\\first".to_string()])
        );
    }
}
