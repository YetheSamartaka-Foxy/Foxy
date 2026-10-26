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

impl RepositorySpaceBulkProgress {
    /// Count one member's outcome. Returns false when the URL is not a pending
    /// target of this run, so a repeated or foreign completion changes nothing.
    pub fn record(&mut self, repo_url: &str, succeeded: bool, had_updates: bool) -> bool {
        if !self.target_repo_urls.contains(repo_url) || self.completed_repo_urls.contains(repo_url)
        {
            return false;
        }
        self.completed_repo_urls.insert(repo_url.to_string());
        self.completed_count += 1;
        if succeeded {
            self.succeeded_count += 1;
            if self.mode == RepositorySpaceBulkMode::RecheckAll {
                if had_updates {
                    self.updates_available_count += 1;
                } else {
                    self.up_to_date_count += 1;
                }
            }
        } else {
            self.failed_count += 1;
        }
        true
    }

    /// Take a removed member out of the run, whether or not it already finished,
    /// so the remaining members can still complete it.
    pub fn remove_target(&mut self, repo_url: &str) {
        if !self.target_repo_urls.remove(repo_url) {
            return;
        }
        self.total_count = self.total_count.saturating_sub(1);
        if self.completed_repo_urls.remove(repo_url) {
            self.completed_count = self.completed_count.saturating_sub(1);
        }
    }

    pub fn is_finished(&self) -> bool {
        self.completed_count >= self.total_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(urls: &[&str]) -> RepositorySpaceBulkProgress {
        RepositorySpaceBulkProgress {
            space_id: "space".into(),
            mode: RepositorySpaceBulkMode::UpdateAll,
            total_count: urls.len(),
            completed_count: 0,
            succeeded_count: 0,
            failed_count: 0,
            updates_available_count: 0,
            up_to_date_count: 0,
            current_repo_name: None,
            target_repo_urls: urls.iter().map(|url| url.to_string()).collect(),
            completed_repo_urls: HashSet::new(),
        }
    }

    #[test]
    fn removing_a_finished_member_still_lets_the_run_complete() {
        let mut run = progress(&["a/", "b/"]);
        assert!(run.record("a/", true, false));
        run.remove_target("a/");
        assert!(!run.is_finished());
        assert!(run.record("b/", true, false));
        assert!(run.is_finished());
    }

    #[test]
    fn repeated_and_foreign_completions_are_ignored() {
        let mut run = progress(&["a/", "b/"]);
        assert!(run.record("a/", false, false));
        assert!(!run.record("a/", true, false));
        assert!(!run.record("other/", true, false));
        assert_eq!((run.completed_count, run.failed_count), (1, 1));
        assert!(!run.is_finished());
    }
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
    /// Checksum of the manifest these fields were last taken from; a refresh
    /// that fetches the same checksum leaves the space untouched.
    #[serde(default)]
    pub manifest_checksum: String,
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
