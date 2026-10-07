//! Metadata-only backups. This format deliberately has no credential field.
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::Path,
};

use crate::{
    models::profile::Profile,
    services::{keychain, profile_store},
};
use serde::{Deserialize, Serialize};

const MAX_BACKUP_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupProfile {
    pub id: String,
    pub display_name: String,
    pub server_url: String,
    pub lan_server_url: Option<String>,
    pub wan_server_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupDefaults {
    pub keep_files_on_disk: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSettings {
    pub stack_raw_jpeg: bool,
    pub stack_burst: bool,
    pub concurrent_tasks: Option<u32>,
    pub keep_going_on_errors: bool,
    pub session_tag: bool,
    pub exclude_extensions: Vec<String>,
    pub theme: String,
    pub palette: String,
    pub avatar_display: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataBackup {
    pub format: String,
    pub version: u32,
    pub profiles: Vec<BackupProfile>,
    pub defaults: BackupDefaults,
    pub ui_settings: UiSettings,
}

#[derive(Debug, Serialize)]
pub struct BackupImportResult {
    pub added: usize,
    pub merged: usize,
    pub needs_api_key: Vec<String>,
    pub ui_settings: Option<UiSettings>,
}

fn checked_url(raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| "Backup has an invalid server URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Backup server URLs must use HTTP(S) without credentials, queries, or fragments".into(),
        );
    }
    Ok(crate::services::immich_client::normalize_server_url(
        url.as_str(),
    ))
}

fn export_url(raw: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(raw.trim())
        .map_err(|_| "Repair invalid profile URLs before exporting")?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    checked_url(url.as_str())
}

fn validate_settings(settings: &UiSettings) -> Result<(), String> {
    if settings
        .concurrent_tasks
        .is_some_and(|n| !(1..=20).contains(&n))
        || !["system", "light", "dark"].contains(&settings.theme.as_str())
        || !["darkroom", "indigo", "ember"].contains(&settings.palette.as_str())
        || !["initials", "photos"].contains(&settings.avatar_display.as_str())
        || settings.exclude_extensions.len() > 100
        || settings.exclude_extensions.iter().any(|ext| {
            !ext.starts_with('.')
                || ext.len() < 2
                || ext.len() > 32
                || !ext[1..].bytes().all(|b| b.is_ascii_alphanumeric())
        })
    {
        return Err("Backup contains invalid import or display settings".into());
    }
    Ok(())
}

fn validate_backup(mut backup: MetadataBackup) -> Result<MetadataBackup, String> {
    if backup.format != "immich-shuttle-metadata" || backup.version != 1 {
        return Err("Unsupported backup format or version".into());
    }
    validate_settings(&backup.ui_settings)?;
    let mut ids = HashSet::new();
    for profile in &mut backup.profiles {
        if profile.id.is_empty()
            || profile.id.len() > 200
            || !profile
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || profile.display_name.trim().is_empty()
            || profile.display_name.len() > 500
        {
            return Err("Backup contains an invalid profile identity or name".into());
        }
        if !ids.insert(profile.id.clone()) {
            return Err(format!(
                "Backup contains duplicate profile ID: {}",
                profile.id
            ));
        }
        profile.server_url = checked_url(&profile.server_url)?;
        profile.lan_server_url = profile
            .lan_server_url
            .as_deref()
            .map(checked_url)
            .transpose()?;
        profile.wan_server_url = profile
            .wan_server_url
            .as_deref()
            .map(checked_url)
            .transpose()?;
    }
    Ok(backup)
}

fn parse_backup(content: &str) -> Result<MetadataBackup, String> {
    if content.len() as u64 > MAX_BACKUP_BYTES {
        return Err("Backup exceeds the 1 MiB size limit".into());
    }
    let backup =
        serde_json::from_str(content).map_err(|e| format!("Invalid metadata backup: {e}"))?;
    validate_backup(backup)
}

struct SizeLimit(u64);

impl Write for SizeLimit {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > MAX_BACKUP_BYTES - self.0 {
            return Err(std::io::Error::other("Backup exceeds the 1 MiB size limit"));
        }
        self.0 += bytes.len() as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Publish a complete owner-only file without replacing any existing file.
/// Unlike config writes, this must never chmod the user's chosen directory.
fn write_new_private(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp = parent.join(format!(
        ".immich-shuttle-backup-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&temp)
            .map_err(|e| format!("Could not create backup: {e}"))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("Could not write backup: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("Could not sync backup: {e}"))?;
        drop(file);
        fs::hard_link(&temp, path)
            .map_err(|e| format!("Could not save backup; choose a new filename: {e}"))?;
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result
}

#[tauri::command]
pub async fn profiles_export(path: String, ui_settings: UiSettings) -> Result<(), String> {
    validate_settings(&ui_settings)?;
    let _guard = profile_store::lock_config();
    let config = profile_store::load_config()?;
    let mut profiles = Vec::with_capacity(config.profiles.len());
    for profile in config.profiles {
        profiles.push(BackupProfile {
            id: profile.id,
            display_name: profile.display_name,
            server_url: export_url(&profile.server_url)?,
            lan_server_url: profile
                .lan_server_url
                .as_deref()
                .map(export_url)
                .transpose()?,
            wan_server_url: profile
                .wan_server_url
                .as_deref()
                .map(export_url)
                .transpose()?,
        });
    }
    let backup = validate_backup(MetadataBackup {
        format: "immich-shuttle-metadata".into(),
        version: 1,
        profiles,
        defaults: BackupDefaults {
            keep_files_on_disk: config.defaults.keep_files_on_disk,
        },
        ui_settings,
    })?;
    let content = serde_json::to_string_pretty(&backup)
        .map_err(|e| format!("Could not encode backup: {e}"))?;
    if content.len() as u64 > MAX_BACKUP_BYTES {
        return Err("Backup exceeds the 1 MiB size limit".into());
    }
    write_new_private(Path::new(&path), &content)
}

#[tauri::command]
pub async fn profiles_backup_read(path: String) -> Result<MetadataBackup, String> {
    let file = fs::File::open(path).map_err(|e| format!("Could not open backup: {e}"))?;
    if !file
        .metadata()
        .map_err(|e| format!("Could not inspect backup: {e}"))?
        .is_file()
    {
        return Err("Choose a regular backup file".into());
    }
    let mut content = String::new();
    file.take(MAX_BACKUP_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|e| format!("Could not read backup: {e}"))?;
    parse_backup(&content)
}

#[tauri::command]
pub async fn profiles_import(
    backup: MetadataBackup,
    restore_settings: bool,
) -> Result<BackupImportResult, String> {
    // Validate again at the mutation boundary; a renderer preview is not authority.
    serde_json::to_writer(SizeLimit(0), &backup)
        .map_err(|e| format!("Invalid metadata backup: {e}"))?;
    let backup = validate_backup(backup)?;
    let _guard = profile_store::lock_config();
    let mut config = profile_store::load_config()?;
    let mut existing_ids = HashSet::new();
    if config
        .profiles
        .iter()
        .any(|p| !existing_ids.insert(p.id.as_str()))
    {
        return Err(
            "Existing config contains duplicate profile IDs; repair it before importing".into(),
        );
    }
    let mut result = BackupImportResult {
        added: 0,
        merged: 0,
        needs_api_key: Vec::new(),
        ui_settings: None,
    };
    for incoming in backup.profiles {
        let existing = config.profiles.iter_mut().find(|p| p.id == incoming.id);
        let key = keychain::get_api_key(&incoming.id)?;
        if let Some(existing) = existing {
            if checked_url(&existing.server_url)? != incoming.server_url
                || existing
                    .lan_server_url
                    .as_deref()
                    .map(checked_url)
                    .transpose()?
                    != incoming.lan_server_url
                || existing
                    .wan_server_url
                    .as_deref()
                    .map(checked_url)
                    .transpose()?
                    != incoming.wan_server_url
            {
                return Err(format!("Profile ID {} has different server endpoints; import refuses this identity conflict", incoming.id));
            }
            existing.display_name = incoming.display_name;
            result.merged += 1;
        } else {
            if key.is_some() {
                return Err(format!(
                    "Profile ID {} already has an orphaned credential; import refuses to reuse it",
                    incoming.id
                ));
            }
            config.profiles.push(Profile {
                id: incoming.id.clone(),
                display_name: incoming.display_name,
                server_url: incoming.server_url,
                lan_server_url: incoming.lan_server_url,
                wan_server_url: incoming.wan_server_url,
            });
            result.added += 1;
        }
        if key.is_none() {
            result.needs_api_key.push(incoming.id);
        }
    }
    if restore_settings {
        config.defaults.keep_files_on_disk = backup.defaults.keep_files_on_disk;
        result.ui_settings = Some(backup.ui_settings);
    }
    profile_store::save_config(&config)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> UiSettings {
        UiSettings {
            stack_raw_jpeg: false,
            stack_burst: true,
            concurrent_tasks: Some(4),
            keep_going_on_errors: true,
            session_tag: true,
            exclude_extensions: vec![".aae".into()],
            theme: "dark".into(),
            palette: "ember".into(),
            avatar_display: "initials".into(),
        }
    }

    fn backup() -> MetadataBackup {
        MetadataBackup {
            format: "immich-shuttle-metadata".into(),
            version: 1,
            profiles: vec![BackupProfile {
                id: "p1".into(),
                display_name: "Camera".into(),
                server_url: "http://127.0.0.1:2283".into(),
                lan_server_url: None,
                wan_server_url: None,
            }],
            defaults: BackupDefaults {
                keep_files_on_disk: false,
            },
            ui_settings: settings(),
        }
    }

    #[test]
    fn backup_rejects_duplicate_ids_credentials_and_unsupported_versions() {
        let mut duplicate = backup();
        duplicate.profiles.push(duplicate.profiles[0].clone());
        assert!(validate_backup(duplicate).is_err());
        let mut future = backup();
        future.version = 2;
        assert!(validate_backup(future).is_err());
        let mut secret = serde_json::to_value(backup()).unwrap();
        secret["profiles"][0]["api_key"] = serde_json::json!("must-not-import");
        assert!(parse_backup(&secret.to_string()).is_err());
        for url in [
            "http://user:secret@127.0.0.1:2283",
            "http://127.0.0.1:2283?api_key=secret",
            "file:///tmp/photos",
        ] {
            let mut invalid = backup();
            invalid.profiles[0].server_url = url.into();
            assert!(validate_backup(invalid).is_err());
        }
    }

    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain seams.
    #[tokio::test]
    async fn metadata_roundtrip_preserves_keys_and_requires_reentry_on_fresh_config() {
        let _config = profile_store::test_config::lock();
        let _credentials = keychain::test_store::exclusive();
        keychain::test_store::reset();
        let dir = profile_store::test_config::use_temp_config_home("metadata-backup");
        let mut config = profile_store::AppConfig::default();
        config.profiles.push(Profile {
            id: "p1".into(),
            display_name: "Camera".into(),
            server_url: "http://127.0.0.1:2283".into(),
            lan_server_url: None,
            wan_server_url: None,
        });
        config.defaults.keep_files_on_disk = false;
        profile_store::save_config(&config).unwrap();
        keychain::store_api_key("p1", "synthetic-secret").unwrap();
        let path = dir.join("backup.json");
        profiles_export(path.to_string_lossy().into_owned(), settings())
            .await
            .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("synthetic-secret"));
        let exported = profiles_backup_read(path.to_string_lossy().into_owned())
            .await
            .unwrap();
        let merged = profiles_import(exported.clone(), true).await.unwrap();
        assert_eq!(merged.merged, 1);
        assert_eq!(
            keychain::get_api_key("p1").unwrap().as_deref(),
            Some("synthetic-secret")
        );
        assert!(merged.needs_api_key.is_empty());

        profile_store::save_config(&profile_store::AppConfig::default()).unwrap();
        keychain::test_store::reset();
        let fresh = profiles_import(exported, true).await.unwrap();
        assert_eq!(fresh.needs_api_key, vec!["p1"]);
        let restored = profile_store::load_config().unwrap();
        assert_eq!(restored.profiles[0].server_url, "http://127.0.0.1:2283");
        assert!(!restored.defaults.keep_files_on_disk);
        assert_eq!(fresh.ui_settings.unwrap().concurrent_tasks, Some(4));
        assert!(keychain::get_api_key("p1").unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain seams.
    #[tokio::test]
    async fn conflicting_identity_leaves_config_and_secret_unchanged() {
        let _config = profile_store::test_config::lock();
        let _credentials = keychain::test_store::exclusive();
        keychain::test_store::reset();
        let dir = profile_store::test_config::use_temp_config_home("backup-conflict");
        let first = backup();
        profiles_import(first.clone(), false).await.unwrap();
        keychain::store_api_key("p1", "synthetic-existing-key").unwrap();
        let before = fs::read(dir.join("immich-shuttle/config.json")).unwrap();
        let mut conflict = first;
        conflict.profiles[0].server_url = "http://127.0.0.1:9999".into();
        assert!(profiles_import(conflict, true).await.is_err());
        assert_eq!(
            fs::read(dir.join("immich-shuttle/config.json")).unwrap(),
            before
        );
        assert_eq!(
            keychain::get_api_key("p1").unwrap().as_deref(),
            Some("synthetic-existing-key")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_never_replaces_a_file_or_changes_parent_permissions() {
        let dir =
            std::env::temp_dir().join(format!("shuttle-backup-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = dir.join("backup.json");
        write_new_private(&path, "first").unwrap();
        assert!(write_new_private(&path, "second").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
