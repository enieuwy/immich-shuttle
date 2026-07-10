use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::{models::history::ImportRecord, services::logs};

static STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
        fs::write(&path, content).map_err(|e| format!("Could not persist store: {e}"))?;
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

pub fn append_history(app: &tauri::AppHandle, record: ImportRecord) {
    let Ok(_guard) = STORE_LOCK.lock() else {
        let _ = logs::append_log(
            "app.log",
            "history_append_failed reason=store_lock_poisoned",
        );
        return;
    };

    let mut data = match load(app) {
        Ok(data) => data,
        // Reading the existing store failed — do NOT overwrite it with a fresh
        // empty state, or we'd wipe the user's real history/source metadata.
        Err(err) => {
            let _ = logs::append_log("app.log", &format!("history_append_failed reason={err}"));
            return;
        }
    };
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

    if let Err(err) = save(app, &data) {
        let _ = logs::append_log("app.log", &format!("history_append_failed reason={err}"));
    }
}

pub fn list_history(app: &tauri::AppHandle) -> Vec<ImportRecord> {
    let Ok(_guard) = STORE_LOCK.lock() else {
        return Vec::new();
    };

    let mut history = match load(app) {
        Ok(data) => data.history,
        Err(_) => return Vec::new(),
    };
    history.sort_by_key(|record| std::cmp::Reverse(record.finished_at));
    history
}

pub fn clear_history(app: &tauri::AppHandle) -> Result<(), String> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| "Could not lock import history store".to_string())?;
    let mut data = load(app)?;
    data.history.clear();
    save(app, &data)
}

pub fn last_import_for(app: &AppHandle, source_paths: &[String]) -> Option<i64> {
    let Ok(_guard) = STORE_LOCK.lock() else {
        return None;
    };

    load(app)
        .ok()?
        .sources
        .get(&source_key(source_paths))
        .map(|source| source.last_imported_at)
}

fn source_key(paths: &[String]) -> String {
    let mut sorted = paths.to_vec();
    sorted.sort();
    sorted.join("\n")
}
