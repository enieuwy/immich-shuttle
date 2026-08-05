use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    /// Full hierarchical value (e.g. "Trip/Iceland"); this is what maps to
    /// immich-go's `--tag` flag, which uses `/` for hierarchy.
    pub value: String,
}
