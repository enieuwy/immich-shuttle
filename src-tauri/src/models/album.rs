use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumUser {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    /// Immich avatar color name (e.g. "blue", "pink"); None when the server
    /// omits it (older versions).
    pub avatar_color: Option<String>,
    /// True when the user has an uploaded profile image, fetchable through the
    /// `user_profile_image` command.
    pub has_profile_image: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub album_name: String,
    pub shared_with: Vec<AlbumUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumShareLink {
    pub url: String,
}
