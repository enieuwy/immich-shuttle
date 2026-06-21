use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub display_name: String,
    pub server_url: String,
    pub lan_server_url: Option<String>,
    pub wan_server_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInput {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub server_url: String,
    pub lan_server_url: Option<String>,
    pub wan_server_url: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub user_name: String,
    pub server_version: String,
    pub is_compatible: bool,
    pub warning: Option<String>,
}
