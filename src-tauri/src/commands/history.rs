use crate::models::history::ImportRecord;
use crate::services::store;

#[tauri::command]
pub async fn history_list(app: tauri::AppHandle) -> Result<Vec<ImportRecord>, String> {
    store::list_history(&app)
}

#[tauri::command]
pub async fn history_clear(app: tauri::AppHandle) -> Result<(), String> {
    store::clear_history(&app)
}

#[tauri::command]
pub async fn history_source_last_import(
    app: tauri::AppHandle,
    profile_id: String,
    source_paths: Vec<String>,
) -> Result<Option<i64>, String> {
    Ok(store::last_import_for(&app, &profile_id, &source_paths))
}
