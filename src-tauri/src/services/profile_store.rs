use std::{collections::HashMap, fs, path::PathBuf};

use dirs::config_dir;
use serde::{Deserialize, Serialize};

use crate::models::profile::Profile;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    pub keep_files_on_disk: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub profiles: Vec<Profile>,
    pub defaults: Defaults,
    pub recent_album_ids: HashMap<String, Vec<String>>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            defaults: Defaults {
                keep_files_on_disk: true,
            },
            recent_album_ids: HashMap::new(),
        }
    }
}

fn config_path() -> Result<PathBuf, String> {
    let base = if let Ok(override_dir) = std::env::var("IMMICH_SHUTTLE_CONFIG_DIR") {
        PathBuf::from(override_dir)
    } else {
        config_dir().ok_or_else(|| "Could not resolve config directory".to_string())?
    };
    let dir = base.join("immich-shuttle");
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create config directory: {e}"))?;
    Ok(dir.join("config.json"))
}

pub fn load_config() -> Result<AppConfig, String> {
    let path = config_path()?;
    if !path.exists() {
        let cfg = AppConfig::default();
        save_config(&cfg)?;
        return Ok(cfg);
    }

    let raw = fs::read_to_string(&path).map_err(|e| format!("Could not read config: {e}"))?;
    serde_json::from_str::<AppConfig>(&raw).map_err(|e| format!("Could not parse config: {e}"))
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = config_path()?;
    let tmp = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Could not serialize config: {e}"))?;
    fs::write(&tmp, content).map_err(|e| format!("Could not write temp config: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("Could not persist config: {e}"))
}

pub fn list_profiles() -> Result<Vec<Profile>, String> {
    Ok(load_config()?.profiles)
}

pub fn get_profile(profile_id: &str) -> Result<Profile, String> {
    load_config()?
        .profiles
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("Profile not found: {profile_id}"))
}

pub fn upsert_profile(profile: Profile) -> Result<Profile, String> {
    let mut cfg = load_config()?;
    if let Some(existing) = cfg.profiles.iter_mut().find(|p| p.id == profile.id) {
        *existing = profile.clone();
    } else {
        cfg.profiles.push(profile.clone());
    }
    save_config(&cfg)?;
    Ok(profile)
}

pub fn delete_profile(profile_id: &str) -> Result<(), String> {
    let mut cfg = load_config()?;
    cfg.profiles.retain(|p| p.id != profile_id);
    cfg.recent_album_ids.remove(profile_id);
    save_config(&cfg)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{LazyLock, Mutex};

    use crate::models::profile::Profile;

    use super::{delete_profile, get_profile, load_config, upsert_profile};

    static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn use_temp_config_home(suffix: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        dir.push(format!(
            "immich-shuttle-profile-store-test-{}-{}-{}",
            suffix,
            std::process::id(),
            nonce
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp config home");
        std::env::set_var("IMMICH_SHUTTLE_CONFIG_DIR", &dir);
        dir
    }

    #[test]
    fn generates_default_config_when_missing() {
        let _guard = TEST_LOCK.lock().expect("lock test mutex");
        let dir = use_temp_config_home("default");
        let cfg = load_config().expect("load default config");
        assert!(cfg.profiles.is_empty());
        assert!(cfg.defaults.keep_files_on_disk);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_and_get_profile_roundtrip() {
        let _guard = TEST_LOCK.lock().expect("lock test mutex");
        let dir = use_temp_config_home("crud");
        let profile = Profile {
            id: "p1".to_string(),
            display_name: "Ellis".to_string(),
            server_url: "https://immich.example.com".to_string(),
            lan_server_url: Some("https://lan.example.com".to_string()),
            wan_server_url: None,
        };

        upsert_profile(profile.clone()).expect("upsert profile");
        let loaded = get_profile("p1").expect("get profile");
        assert_eq!(loaded.display_name, profile.display_name);
        assert_eq!(loaded.server_url, profile.server_url);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_profile_removes_profile() {
        let _guard = TEST_LOCK.lock().expect("lock test mutex");
        let dir = use_temp_config_home("delete");
        let profile = Profile {
            id: "p1".to_string(),
            display_name: "Ellis".to_string(),
            server_url: "https://immich.example.com".to_string(),
            lan_server_url: None,
            wan_server_url: None,
        };
        upsert_profile(profile).expect("upsert profile");
        delete_profile("p1").expect("delete profile");
        assert!(get_profile("p1").is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
