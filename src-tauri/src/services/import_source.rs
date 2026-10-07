use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    #[default]
    Folder,
    GooglePhotos,
    Icloud,
    Immich,
}

impl ImportSource {
    pub fn command(self) -> &'static str {
        match self {
            Self::Folder => "from-folder",
            Self::GooglePhotos => "from-google-photos",
            Self::Icloud => "from-icloud",
            Self::Immich => "from-immich",
        }
    }
    pub fn folder_options(self) -> bool {
        matches!(self, Self::Folder | Self::Icloud)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImportExtensions {
    pub source: ImportSource,
    pub source_profile_id: Option<String>,
    pub date_from_name: Option<bool>,
    pub time_zone: Option<String>,
    pub clock_offset_minutes: i32,
    pub dry_run: bool,
    pub completion_webhook_url: Option<String>,
    pub share_user_ids: Vec<String>,
    pub share_role: Option<String>,
    pub public_link: bool,
}

impl ImportExtensions {
    pub fn validate(&self) -> Result<(), String> {
        if self.clock_offset_minutes.unsigned_abs() > 525600 {
            return Err("Clock correction must be within one year.".into());
        }
        if let Some(role) = &self.share_role {
            if role != "viewer" && role != "editor" {
                return Err("Share role must be viewer or editor.".into());
            }
        }
        if let Some(zone) = &self.time_zone {
            if zone.len() > 128 || zone.contains(['\n', '\r']) {
                return Err("Invalid time zone.".into());
            }
        }
        if let Some(value) = &self.completion_webhook_url {
            let url = reqwest::Url::parse(value).map_err(|_| "Invalid callback URL.")?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(
                    "Callback requires an HTTP(S) URL without embedded credentials.".into(),
                );
            }
        }
        Ok(())
    }
}
