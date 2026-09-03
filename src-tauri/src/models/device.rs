use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovableDevice {
    pub name: String,
    pub mount_path: String,
    pub total_space: u64,
    pub available_space: u64,
    pub has_dcim: bool,
    /// Stable per-volume identity (macOS volume UUID, Linux filesystem UUID, Windows
    /// volume GUID path). `None` when the platform cannot prove which physical medium
    /// this is: label and mount path are reused across cards, so a caller that keys a
    /// destination or a delete-after-verify policy by card MUST refuse to act without
    /// this value rather than fall back to something ambiguous.
    pub volume_id: Option<String>,
}
