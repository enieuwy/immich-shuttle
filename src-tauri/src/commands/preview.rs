use crate::services::thumbnailer::{thumbnail, ThumbResult, MAX_PX};

/// Generate (or fetch cached) thumbnails for a batch of files. The frontend grid
/// calls this lazily for the tiles entering the viewport. Work runs on blocking
/// threads, bounded to a small pool so a large card can't saturate the runtime.
#[tauri::command]
pub async fn preview_thumbnails(paths: Vec<String>) -> Result<Vec<ThumbResult>, String> {
    let mut results = Vec::with_capacity(paths.len());

    for chunk in paths.chunks(8) {
        let handles: Vec<_> = chunk
            .iter()
            .cloned()
            .map(|path| tauri::async_runtime::spawn_blocking(move || thumbnail(&path, MAX_PX)))
            .collect();

        for handle in handles {
            if let Ok(result) = handle.await {
                results.push(result);
            }
        }
    }

    Ok(results)
}
