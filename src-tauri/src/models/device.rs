use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovableDevice {
    pub name: String,
    pub mount_path: String,
    pub total_space: u64,
    pub available_space: u64,
    pub has_dcim: bool,
}
