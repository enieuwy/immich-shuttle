use reqwest::Url;
use uuid::Uuid;

use crate::{
    models::profile::{Profile, ProfileInput, ServerInfo},
    services::{
        immich_client::{normalize_server_url, server_compatibility, ImmichClient},
        keychain, logs, profile_store,
    },
};

#[tauri::command]
pub async fn profiles_list() -> Result<Vec<Profile>, String> {
    Ok(profile_store::list_profiles()?
        .into_iter()
        .map(normalize_loaded_profile)
        .collect())
}

/// Scan the local network for reachable Immich servers, returning confirmed
/// base URLs the user can one-click into a profile. Read-only; probes only the
/// unauthenticated ping endpoint.
#[tauri::command]
pub async fn discover_immich_servers() -> Result<Vec<String>, String> {
    Ok(crate::services::discovery::discover_immich_servers().await)
}

#[tauri::command]
pub async fn profile_upsert(input: ProfileInput) -> Result<Profile, String> {
    if input.server_url.trim().is_empty() {
        return Err("Server URL is required".to_string());
    }

    let is_new_profile = input.id.is_none();
    let id = input
        .id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let profile = profile_from_input(&input, id.clone())?;

    let api_key = input
        .api_key
        .filter(|api_key| !api_key.trim().is_empty())
        .map(|api_key| api_key.trim().to_string());
    let _guard = profile_store::lock_config();
    // A blank key means "keep the stored one", which only exists to keep. A new
    // profile mints its own id, so nothing can be stored under it; committing
    // one anyway produces a selectable profile whose every authenticated
    // command fails through `require_api_key`, with no way back but delete.
    let previous_api_key = if is_new_profile {
        None
    } else {
        keychain::get_api_key(&id)?
    };
    if api_key.is_none() && previous_api_key.is_none() {
        // Rejected before any write: the config must not gain a profile the
        // user cannot use.
        return Err("API key is required".to_string());
    }

    let stored_key = if let Some(api_key) = api_key {
        keychain::store_api_key(&id, &api_key)?;
        true
    } else {
        false
    };

    match profile_store::upsert_profile_locked(profile) {
        Ok(saved) => Ok(saved),
        Err(err) if stored_key => {
            let rollback = match previous_api_key {
                Some(api_key) => keychain::store_api_key(&id, &api_key),
                None => keychain::delete_api_key(&id),
            };
            match rollback {
                Ok(()) => Err(err),
                Err(rollback_err) => Err(format!(
                    "{err}; additionally failed to roll back the API key change: {rollback_err}"
                )),
            }
        }
        Err(err) => Err(err),
    }
}

fn normalized_server_url(value: &str) -> Result<String, String> {
    let mut url = Url::parse(value.trim()).map_err(|_| "Invalid server URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err("Invalid server URL".to_string());
    }

    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok(normalize_server_url(url.as_str()))
}

fn normalized_optional_server_url(value: Option<&str>) -> Result<Option<String>, String> {
    value
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(normalized_server_url)
        .transpose()
}

/// Normalize a stored profile for display, degrading rather than failing.
///
/// An older build let the editor save a value this normalizer now rejects — a
/// schemeless `192.168.1.10:2283`, for example. Failing here failed the whole
/// list, so one unparseable stored URL hid every profile and left the user an
/// empty picker with no route back to the editor that could repair it. The
/// profile therefore survives with a display-only URL, while every path that
/// would USE that URL — `url_resolver::resolve_server_url`, `profile_upsert`,
/// `profile_validate` — normalizes it again for itself and still refuses it.
/// The label degrades; the action keeps failing closed.
fn normalize_loaded_profile(mut profile: Profile) -> Profile {
    let mut degraded: Vec<&'static str> = Vec::new();
    profile.server_url = normalized_loaded_url(&profile.server_url, "server_url", &mut degraded);
    profile.lan_server_url = loaded_optional_url(
        profile.lan_server_url.as_deref(),
        "lan_server_url",
        &mut degraded,
    );
    profile.wan_server_url = loaded_optional_url(
        profile.wan_server_url.as_deref(),
        "wan_server_url",
        &mut degraded,
    );
    if !degraded.is_empty() {
        // Names the profile and the fields, never the stored text: an old value
        // can still carry the credentials the URL used to be allowed to hold,
        // and app.log is user-visible and shipped in support reports. A failed
        // append must not cost the user their profile list, so it is ignored.
        let _ = logs::append_log(
            "app.log",
            &format!(
                "profile_url_not_normalizable profile_id={} fields={}",
                profile.id,
                degraded.join(",")
            ),
        );
    }
    profile
}

fn loaded_optional_url(
    value: Option<&str>,
    field: &'static str,
    degraded: &mut Vec<&'static str>,
) -> Option<String> {
    value
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| normalized_loaded_url(url, field, degraded))
}

/// One stored URL: normalized when it can be, kept as display-only text when it
/// cannot, recording the field so the caller can log it.
///
/// Credentials are stripped from the degraded text whenever the value still
/// parses as a URL, because the editor renders this text back to the user. A
/// value that does not parse at all has no authority we could identify, and no
/// consumer ever sends this text anywhere: each one normalizes first.
fn normalized_loaded_url(
    value: &str,
    field: &'static str,
    degraded: &mut Vec<&'static str>,
) -> String {
    if let Ok(url) = normalized_server_url(value) {
        return url;
    }
    degraded.push(field);
    let trimmed = value.trim();
    match Url::parse(trimmed) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            String::from(url)
        }
        Err(_) => trimmed.to_string(),
    }
}

fn profile_from_input(input: &ProfileInput, id: String) -> Result<Profile, String> {
    Ok(Profile {
        id,
        display_name: input
            .display_name
            .clone()
            .unwrap_or_else(|| "Immich User".to_string()),
        server_url: normalized_server_url(&input.server_url)?,
        lan_server_url: normalized_optional_server_url(input.lan_server_url.as_deref())?,
        wan_server_url: normalized_optional_server_url(input.wan_server_url.as_deref())?,
    })
}

#[tauri::command]
pub async fn profile_delete(id: String) -> Result<(), String> {
    let _guard = profile_store::lock_config();
    let previous_api_key = keychain::get_api_key(&id)?;
    keychain::delete_api_key(&id)?;

    if let Err(err) = profile_store::delete_profile_locked(&id) {
        if let Some(api_key) = previous_api_key {
            if let Err(rollback_err) = keychain::store_api_key(&id, &api_key) {
                return Err(format!(
                    "{err}; additionally failed to restore the API key after the profile delete failed: {rollback_err}"
                ));
            }
        }
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn profile_validate(url: String, api_key: String) -> Result<ServerInfo, String> {
    if url.trim().is_empty() {
        return Err("Server URL is required".to_string());
    }
    let normalized_url = normalized_server_url(&url)?;
    if api_key.trim().is_empty() {
        return Err("API key is required".to_string());
    }

    let client = ImmichClient::new(&normalized_url, api_key.trim());
    client.ping().await?;
    let version = client.get_server_version().await?;
    let me = client.get_my_user().await?;

    let (is_compatible, warning) = server_compatibility(&version);

    Ok(ServerInfo {
        user_name: me
            .name
            .or(me.email)
            .unwrap_or_else(|| "Immich User".to_string()),
        server_version: format!("{}.{}.{}", version.major, version.minor, version.patch),
        is_compatible,
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::{profile_from_input, profile_upsert, profile_validate, profiles_list};
    use crate::models::profile::{Profile, ProfileInput};
    use crate::services::{keychain, profile_store, url_resolver};

    /// Both process-wide test seams, always taken in this order so the
    /// keychain tests (credential lock only) cannot deadlock against these.
    fn isolate(
        suffix: &str,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
        std::path::PathBuf,
    ) {
        let config_guard = profile_store::test_config::lock();
        let credential_guard = keychain::test_store::exclusive();
        keychain::test_store::reset();
        let dir = profile_store::test_config::use_temp_config_home(suffix);
        (config_guard, credential_guard, dir)
    }

    fn input(id: Option<&str>, server_url: &str, api_key: Option<&str>) -> ProfileInput {
        ProfileInput {
            id: id.map(str::to_string),
            display_name: None,
            server_url: server_url.to_string(),
            lan_server_url: None,
            wan_server_url: None,
            api_key: api_key.map(str::to_string),
        }
    }

    #[test]
    fn profile_builder_normalizes_optional_lan_and_wan_urls() {
        let profile = profile_from_input(
            &ProfileInput {
                id: None,
                display_name: None,
                server_url: "https://immich.example.com".to_string(),
                lan_server_url: Some(" https://lan.example.com/ ".to_string()),
                wan_server_url: Some("https://wan.example.com/api".to_string()),
                api_key: None,
            },
            "profile-id".to_string(),
        )
        .expect("valid profile URLs");

        assert_eq!(
            profile.lan_server_url.as_deref(),
            Some("https://lan.example.com")
        );
        assert_eq!(
            profile.wan_server_url.as_deref(),
            Some("https://wan.example.com")
        );
    }
    // Profile editing keeps the default name and ignores empty optional endpoints.
    #[test]
    fn profile_builder_defaults_name_and_discards_empty_optional_urls() {
        let profile = profile_from_input(
            &ProfileInput {
                id: None,
                display_name: None,
                server_url: "https://immich.example.com".to_string(),
                lan_server_url: Some("   ".to_string()),
                wan_server_url: None,
                api_key: None,
            },
            "profile-id".to_string(),
        )
        .expect("valid profile URLs");

        assert_eq!(profile.display_name, "Immich User");
        assert_eq!(profile.lan_server_url, None);
        assert_eq!(profile.wan_server_url, None);

        let empty_url_profile = profile_from_input(
            &ProfileInput {
                id: None,
                display_name: None,
                server_url: "https://immich.example.com".to_string(),
                lan_server_url: Some(String::new()),
                wan_server_url: Some("  https://wan.example.com/api/  ".to_string()),
                api_key: None,
            },
            "profile-id".to_string(),
        )
        .expect("valid profile URLs");

        assert_eq!(empty_url_profile.lan_server_url, None);
        assert_eq!(
            empty_url_profile.wan_server_url.as_deref(),
            Some("https://wan.example.com")
        );
    }

    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain test seams.
    #[tokio::test]
    async fn profile_upsert_persists_urls_without_userinfo() {
        let (_config, _credentials, dir) = isolate("upsert-userinfo");
        let saved = profile_upsert(ProfileInput {
            id: None,
            display_name: None,
            server_url: "https://user:password@immich.example.com/api/".to_string(),
            lan_server_url: Some("http://lan-user:lan-password@lan.example.com:2283/".to_string()),
            wan_server_url: Some("https://wan-user:wan-password@wan.example.com/api".to_string()),
            api_key: Some("api-key".to_string()),
        })
        .await
        .expect("save profile");
        let stored = profile_store::get_profile(&saved.id).expect("load saved profile");

        assert_eq!(stored.server_url, "https://immich.example.com");
        assert_eq!(
            stored.lan_server_url.as_deref(),
            Some("http://lan.example.com:2283")
        );
        assert_eq!(
            stored.wan_server_url.as_deref(),
            Some("https://wan.example.com")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain test seams.
    #[tokio::test]
    async fn profiles_list_sanitizes_legacy_profile_urls() {
        let (_config, _credentials, dir) = isolate("list-legacy-userinfo");
        profile_store::upsert_profile(Profile {
            id: "legacy".to_string(),
            display_name: "Legacy".to_string(),
            server_url: "https://user:password@immich.example.com".to_string(),
            lan_server_url: Some("http://lan-user:lan-password@lan.example.com:2283".to_string()),
            wan_server_url: Some("https://wan-user:wan-password@wan.example.com".to_string()),
        })
        .expect("write legacy profile");

        let profiles = profiles_list().await.expect("list legacy profile");
        let profile = profiles.first().expect("legacy profile");
        assert_eq!(profile.server_url, "https://immich.example.com");
        assert_eq!(
            profile.lan_server_url.as_deref(),
            Some("http://lan.example.com:2283")
        );
        assert_eq!(
            profile.wan_server_url.as_deref(),
            Some("https://wan.example.com")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    // A stored URL an older build allowed must not hide the profiles beside
    // it: the list is the only route to the editor that can repair it. The bad
    // one still resolves to nothing, which every caller reports as no
    // reachable server, so the display degrades and the action fails closed.
    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain test seams.
    #[tokio::test]
    async fn profiles_list_keeps_a_profile_whose_stored_url_cannot_be_normalized() {
        let (_config, _credentials, dir) = isolate("list-unnormalizable-url");
        profile_store::upsert_profile(Profile {
            id: "schemeless".to_string(),
            display_name: "Schemeless".to_string(),
            server_url: "192.168.1.10:2283".to_string(),
            lan_server_url: Some("ftp://lan-user:lan-password@lan.example.com".to_string()),
            wan_server_url: None,
        })
        .expect("write schemeless profile");
        profile_store::upsert_profile(Profile {
            id: "valid".to_string(),
            display_name: "Valid".to_string(),
            server_url: "https://immich.example.com".to_string(),
            lan_server_url: None,
            wan_server_url: None,
        })
        .expect("write valid profile");

        let profiles = profiles_list().await.expect("list both profiles");

        let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["schemeless", "valid"]);
        let schemeless = &profiles[0];
        let valid = &profiles[1];
        assert_eq!(schemeless.server_url, "192.168.1.10:2283");
        assert_eq!(valid.server_url, "https://immich.example.com");
        // The degraded text is rendered back into the editor, so a credential an
        // older URL was allowed to carry is still stripped where it is visible.
        let lan = schemeless
            .lan_server_url
            .as_deref()
            .expect("the degraded LAN URL is kept for display");
        assert!(!lan.contains("lan-password"), "leaked credential: {lan}");
        assert!(!lan.contains("lan-user"), "leaked credential: {lan}");

        assert!(
            url_resolver::resolve_server_url(schemeless)
                .await
                .is_empty(),
            "an unnormalizable stored URL must resolve to no server"
        );
        assert_eq!(
            url_resolver::resolve_server_url(valid).await,
            "https://immich.example.com"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain test seams.
    #[tokio::test]
    async fn invalid_server_urls_are_rejected_without_credential_echoes() {
        let (_config, _credentials, dir) = isolate("reject-invalid-userinfo");
        let error = profile_upsert(input(None, "https://user:password@[", Some("api-key")))
            .await
            .expect_err("invalid URL must not be persisted");

        assert_eq!(error, "Invalid server URL");
        assert!(!error.contains("user"));
        assert!(!error.contains("password"));
        assert!(profile_store::list_profiles()
            .expect("list profiles")
            .is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn invalid_url_validation_does_not_echo_userinfo() {
        let error = profile_validate("https://user:password@[".to_string(), "api-key".to_string())
            .await
            .expect_err("invalid URL must be rejected before connecting");

        assert_eq!(error, "Invalid server URL");
        assert!(!error.contains("user"));
        assert!(!error.contains("password"));
    }

    // A profile with no credential is selectable but unusable, so creating one
    // must fail before anything reaches the config file.
    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain test seams.
    #[tokio::test]
    async fn creating_a_profile_without_an_api_key_writes_nothing() {
        let (_config, _credentials, dir) = isolate("upsert-missing-key");

        for blank in [None, Some(""), Some("   ")] {
            let error = profile_upsert(input(None, "https://immich.example.com", blank))
                .await
                .expect_err("a new profile without an API key must be rejected");
            assert_eq!(error, "API key is required");
        }

        assert!(profile_store::list_profiles()
            .expect("list profiles")
            .is_empty());
        assert!(!dir.join("immich-shuttle/config.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    // Editing keeps blank-means-keep: the dialog never repopulates the key
    // field, so saving an unrelated URL change must not clear the credential.
    #[allow(clippy::await_holding_lock)] // Serializes process-global config and fake-keychain test seams.
    #[tokio::test]
    async fn editing_with_a_blank_api_key_keeps_the_stored_credential() {
        let (_config, _credentials, dir) = isolate("upsert-blank-key");
        let created = profile_upsert(input(
            None,
            "https://immich.example.com",
            Some("original-key"),
        ))
        .await
        .expect("create profile with an API key");

        let updated = profile_upsert(input(Some(&created.id), "https://moved.example.com", None))
            .await
            .expect("editing an existing profile with a blank key must succeed");

        assert_eq!(updated.id, created.id);
        assert_eq!(updated.server_url, "https://moved.example.com");
        assert_eq!(
            keychain::test_store::peek(&created.id).as_deref(),
            Some("original-key")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
