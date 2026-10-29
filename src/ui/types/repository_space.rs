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
    pub pending_target: Option<crate::ui::types::scheduling::RepoInstanceKey>,
    pub active_target: Option<crate::ui::types::scheduling::RepoInstanceKey>,
    pub target_instances: Vec<crate::ui::types::scheduling::RepoInstanceKey>,
    pub completed_instances: Vec<crate::ui::types::scheduling::RepoInstanceKey>,
}

impl RepositorySpaceBulkProgress {
    pub fn record_target(
        &mut self,
        target: &crate::ui::types::scheduling::RepoInstanceKey,
        succeeded: bool,
        had_updates: bool,
    ) -> bool {
        if !self.target_instances.contains(target) || self.completed_instances.contains(target) {
            return false;
        }
        self.completed_instances.push(target.clone());
        let url = target.remote_url.clone();
        if self.pending_target.as_ref() == Some(target) {
            self.pending_target = None;
        }
        if self.active_target.as_ref() == Some(target) {
            self.active_target = None;
        }
        self.completed_repo_urls.insert(url);
        self.record_outcome(succeeded, had_updates);
        true
    }

    pub fn remove_target_instance(
        &mut self,
        target: &crate::ui::types::scheduling::RepoInstanceKey,
    ) {
        let Some(index) = self.target_instances.iter().position(|item| item == target) else {
            return;
        };
        self.target_instances.remove(index);
        if self.pending_target.as_ref() == Some(target) {
            self.pending_target = None;
        }
        if self.active_target.as_ref() == Some(target) {
            self.active_target = None;
        }
        if !self.completed_instances.contains(target) {
            self.total_count = self.total_count.saturating_sub(1);
            self.failed_count += 1;
        }
        let url = &target.remote_url;
        if !self
            .target_instances
            .iter()
            .any(|item| &item.remote_url == url)
        {
            self.target_repo_urls.remove(url);
        }
    }

    /// Count one member's outcome. Returns false when the URL is not a pending
    /// target of this run, so a repeated or foreign completion changes nothing.
    pub fn record(&mut self, repo_url: &str, succeeded: bool, had_updates: bool) -> bool {
        if !self.target_repo_urls.contains(repo_url) || self.completed_repo_urls.contains(repo_url)
        {
            return false;
        }
        self.completed_repo_urls.insert(repo_url.to_string());
        self.record_outcome(succeeded, had_updates);
        true
    }

    fn record_outcome(&mut self, succeeded: bool, had_updates: bool) {
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
    }

    /// Remove a pending member from this run and count it as a failed skip.
    /// Completed outcomes stay in the run summary if their row is later removed.
    pub fn remove_target(&mut self, repo_url: &str) {
        if !self.target_repo_urls.remove(repo_url) {
            return;
        }
        if !self.completed_repo_urls.contains(repo_url) {
            self.total_count = self.total_count.saturating_sub(1);
            self.failed_count += 1;
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
            pending_target: None,
            active_target: None,
            target_instances: Vec::new(),
            completed_instances: Vec::new(),
        }
    }

    #[test]
    fn removing_a_pending_member_counts_as_failed_and_remaining_can_finish() {
        let mut run = progress(&["a/", "b/"]);
        run.remove_target("a/");
        assert_eq!(run.total_count, 1);
        assert_eq!((run.completed_count, run.failed_count), (0, 1));
        assert!(!run.is_finished());
        assert!(run.target_repo_urls.contains("b/"));
        assert!(!run.target_repo_urls.contains("a/"));

        assert!(run.record("b/", true, false));
        assert!(run.is_finished());
        assert_eq!((run.succeeded_count, run.failed_count), (1, 1));
    }

    #[test]
    fn removing_a_finished_member_preserves_its_counted_outcome() {
        let mut run = progress(&["a/", "b/"]);
        assert!(run.record("a/", true, false));
        run.remove_target("a/");
        assert_eq!(run.total_count, 2);
        assert_eq!((run.completed_count, run.succeeded_count), (1, 1));
        assert!(!run.is_finished());
        assert!(run.record("b/", true, false));
        assert!(run.is_finished());
        assert_eq!((run.completed_count, run.succeeded_count), (2, 2));
    }

    #[test]
    fn removing_the_only_pending_member_finishes_as_failed() {
        let mut run = progress(&["a/"]);
        run.remove_target("a/");

        assert!(run.is_finished());
        assert_eq!(run.total_count, 0);
        assert_eq!((run.succeeded_count, run.failed_count), (0, 1));
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

    #[test]
    fn instance_completion_is_exactly_once_for_same_url_targets() {
        let first = crate::ui::types::scheduling::RepoInstanceKey {
            remote_url: "https://repo.example.invalid/".into(),
            local_path: "R:/Mods/First".into(),
        };
        let second = crate::ui::types::scheduling::RepoInstanceKey {
            remote_url: first.remote_url.clone(),
            local_path: "R:/Mods/Second".into(),
        };
        let mut run = progress(&[&first.remote_url]);
        run.target_instances = vec![first.clone(), second.clone()];
        run.total_count = 2;
        run.pending_target = Some(first.clone());

        assert!(run.record_target(&first, false, false));
        assert!(!run.record_target(&first, true, false));
        assert_eq!((run.completed_count, run.failed_count), (1, 1));
        assert!(!run.completed_instances.contains(&second));
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
