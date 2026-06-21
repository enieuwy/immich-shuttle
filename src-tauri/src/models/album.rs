use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumUser {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
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
