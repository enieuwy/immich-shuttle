mod commands;
mod models;
mod services;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            crate::services::device_detector::start_polling(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::profiles::profiles_list,
            commands::profiles::profile_upsert,
            commands::profiles::profile_delete,
            commands::profiles::profile_validate,
            commands::albums::albums_list,
            commands::albums::album_create,
            commands::albums::album_share_users,
            commands::albums::album_share_link,
            commands::import::import_start,
            commands::import::import_confirm_wipe,
            commands::import::import_cancel,
            commands::import::import_list_jobs,
            commands::import::import_retry,
            commands::import::import_dismiss,
            commands::import::import_clear_finished,
            commands::history::history_list,
            commands::history::history_clear,
            commands::history::history_source_last_import,
            commands::import::scan_source,
            commands::import::scan_sources,
            commands::devices::devices_list_removable,
            commands::users::users_list,
            commands::settings::get_server_info,
            commands::settings::get_logs_dir,
            commands::settings::get_recent_logs,
            commands::settings::open_logs_dir
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
