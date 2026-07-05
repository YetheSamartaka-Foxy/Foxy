use super::enums::{RepoState, RepositorySpaceBulkMode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct BulkActionEntry {
    pub repo_index: usize,
    pub repo_name: String,
    pub current_state: RepoState,
    pub selected: bool,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct RepositorySpaceBulkAction {
    pub space_id: String,
    pub space_name: String,
    pub mode: RepositorySpaceBulkMode,
    pub entries: Vec<BulkActionEntry>,
}

#[derive(Debug, Clone)]
pub struct RepositorySpaceBulkProgress {
    pub space_id: String,
    pub mode: RepositorySpaceBulkMode,
    pub total_count: usize,
    pub completed_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    pub updates_available_count: usize,
    pub up_to_date_count: usize,
    pub current_repo_name: Option<String>,
    pub target_repo_urls: HashSet<String>,
    pub completed_repo_urls: HashSet<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct RepositorySpaceEntry {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct RepositorySpace {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub local_name_override: Option<String>,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub source_address: String,
    #[serde(default)]
    pub source_base_url: String,
    #[serde(default)]
    pub shared_path: String,
    #[serde(default)]
    pub icon_image_path: String,
    #[serde(default)]
    pub icon_image_checksum: String,
    #[serde(default)]
    pub repo_image_path: String,
    #[serde(default)]
    pub repo_image_checksum: String,
    #[serde(default)]
    pub app_update_url: String,
    #[serde(default)]
    pub entries: Vec<RepositorySpaceEntry>,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct RepositoryVisualFolder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub repository_space_id: Option<String>,
    #[serde(default = "default_repository_visual_folder_color")]
    pub color_rgb: [u8; 3],
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub repository_keys: Vec<String>,
}

pub const fn default_repository_visual_folder_color() -> [u8; 3] {
    [86, 132, 214]
}
