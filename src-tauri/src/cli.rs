use crate::{commands::import, models::job::{ImportInput, JobStatus}, services::{import_source::ImportSource, store}};
use fs4::fs_std::FileExt;
use std::{fs::{self, File, OpenOptions}, io::Read};

pub struct InstanceLease { _file: File }
pub fn acquire_instance() -> Result<InstanceLease, String> {
    let dir = store::data_dir()?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let file = options.open(dir.join("import-owner.lock")).map_err(|e| e.to_string())?;
    if !file.try_lock_exclusive().map_err(|e| e.to_string())? {
        return Err("Another GUI or headless instance owns this data directory.".into());
    }
    Ok(InstanceLease { _file: file })
}

const HELP: &str = "Immich Shuttle headless imports\n\nUsage:\n  immich-shuttle --headless import --request request.json [--confirm-trash]\n  immich-shuttle --headless import --profile ID --source PATH [--album NAME] [--dry-run]\n\nOptions: --source-kind folder|google_photos|icloud|immich, --source-profile ID, --keep\n\nRequests use the same ImportInput JSON as the desktop. Progress and the final result are NDJSON.\nExit 0 means a complete run; 1 means failure or an incomplete result; 2 means invalid arguments.\nCredentials use the OS keychain, or an explicit owner-only JSON map in IMMICH_SHUTTLE_API_KEYS_FILE.\nIMMICH_SHUTTLE_CONFIG_DIR and IMMICH_SHUTTLE_DATA_DIR isolate all configuration, history and logs.\nA dry run never offers deletion. --confirm-trash is required before a delete-enabled request can start.\n";

pub fn try_run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--headless") { return None; }
    if args.len() == 1 || args.iter().any(|a| a == "--help" || a == "-h") { println!("{HELP}"); return Some(0); }
    let (input, confirm) = match parse(&args[1..]) {
        Ok(value) => value,
        Err(error) => { eprintln!("{}", serde_json::json!({"error": error})); return Some(2); }
    };
    let result = tauri::async_runtime::block_on(async move {
        let _lease = acquire_instance()?;
        let id = import::start_import(None, input).await?;
        let cancel_id = id.clone();
        let signal = tauri::async_runtime::spawn(async move {
            #[cfg(unix)] {
                if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
                } else { let _ = tokio::signal::ctrl_c().await; }
            }
            #[cfg(not(unix))] { let _ = tokio::signal::ctrl_c().await; }
            let _ = import::import_cancel(cancel_id).await;
        });
        loop {
            match import::import_await_terminal(id.clone(), 600_000).await {
                Ok(mut job) => {
                    signal.abort();
                    if job.awaiting_wipe_confirmation && confirm {
                        job = import::import_confirm_wipe(id, true).await?;
                    }
                    let success = matches!(job.status, JobStatus::Completed) && job.error.is_none() && job.progress.errors == 0;
                    println!("{}", serde_json::json!({"event": "result", "job": job}));
                    return Ok::<_, String>(if success { 0 } else { 1 });
                }
                Err(error) if error.starts_with("Timed out waiting for import ") => continue,
                Err(error) => { signal.abort(); return Err(error); }
            }
        }
    });
    Some(match result { Ok(code) => code, Err(error) => { eprintln!("{}", serde_json::json!({"event":"error", "error":error})); 1 } })
}

fn parse(args: &[String]) -> Result<(ImportInput, bool), String> {
    if args.first().map(String::as_str) != Some("import") { return Err(HELP.into()); }
    let mut value = serde_json::json!({"profile_id":"", "source_paths":[], "album_ids":[], "keep_files":true,
        "stack_raw_jpeg":false, "stack_burst":false, "date_range":null, "concurrent_tasks":2});
    let mut request = None;
    let mut confirm = false;
    let mut extended = crate::services::import_source::ImportExtensions::default();
    let mut i = 1;
    let mut overrides = false;
    while i < args.len() {
        let flag = &args[i]; i += 1;
        match flag.as_str() {
            "--confirm-trash" => confirm = true,
            "--keep" => { value["keep_files"] = true.into(); overrides = true; }
            "--dry-run" => { extended.dry_run = true; overrides = true; }
            "--request" | "--profile" | "--source" | "--album" | "--source-kind" | "--source-profile" => {
                let v = args.get(i).ok_or_else(|| format!("{flag} needs a value"))?; i += 1;
                if flag != "--request" { overrides = true; }
                match flag.as_str() {
                    "--request" => request = Some(v.clone()),
                    "--profile" => value["profile_id"] = v.clone().into(),
                    "--source" => value["source_paths"].as_array_mut().unwrap().push(v.clone().into()),
                    "--album" => value["into_album"] = v.clone().into(),
                    "--source-profile" => extended.source_profile_id = Some(v.clone()),
                    "--source-kind" => extended.source = match v.as_str() {
                        "folder" => ImportSource::Folder, "google_photos" => ImportSource::GooglePhotos,
                        "icloud" => ImportSource::Icloud, "immich" => ImportSource::Immich,
                        _ => return Err("Unknown source kind".into()),
                    },
                    _ => unreachable!(),
                }
            }
            _ => return Err(format!("Unknown option {flag}")),
        }
    }
    let input: ImportInput = if let Some(path) = request {
        if overrides { return Err("Use either --request or individual import options, not both.".into()); }
        let mut raw = String::new();
        File::open(path).map_err(|e| e.to_string())?.take(16 * 1024 * 1024 + 1).read_to_string(&mut raw).map_err(|e| e.to_string())?;
        if raw.len() > 16 * 1024 * 1024 { return Err("Request exceeds 16 MiB".into()); }
        serde_json::from_str(&raw).map_err(|e| format!("Invalid import request: {e}"))?
    } else {
        value["extended"] = serde_json::to_value(extended).map_err(|e| e.to_string())?;
        serde_json::from_value(value).map_err(|e| e.to_string())?
    };
    if input.profile_id.is_empty() { return Err("A destination profile is required.".into()); }
    if !input.keep_files && !confirm { return Err("A delete-enabled request requires --confirm-trash.".into()); }
    Ok((input, confirm))
}
