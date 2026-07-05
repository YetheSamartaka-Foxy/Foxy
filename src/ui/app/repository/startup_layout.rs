use log::info;

use crate::ui::app::Foxy;
use crate::ui::types::{Repository, RepositorySpace};

impl Foxy {
    pub(crate) fn maybe_log_startup_repository_layout(&mut self) {
        if self.startup_repository_layout_logged || !self.startup_tasks_started {
            return;
        }
        if self.startup_pending_restore_worker.is_some()
            || self.startup_pending_restore_rx.is_some()
            || self.startup_quick_scan_filter_worker.is_some()
            || self.startup_quick_scan_filter_rx.is_some()
            || self.quick_scan_worker.is_some()
            || !self.active_quick_scan_instance_keys.is_empty()
            || self.syncing_repository.is_some()
            || !self.startup_recheck_queue.is_empty()
        {
            return;
        }

        self.startup_repository_layout_logged = true;
        for line in self.repository_layout_lines() {
            info!("{line}");
        }
    }

    pub(crate) fn repository_layout_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push("Repository layout after startup checks:".to_string());

        for space in &self.repository_spaces {
            lines.push(format!(
                "Repo space - name={} url={} path={}",
                display_value(Self::repository_space_display_name(space)),
                display_value(repository_space_url(space)),
                display_value(&space.shared_path)
            ));

            let mut count = 0usize;
            for repo in self.repositories_for_space(&space.id) {
                count += 1;
                lines.push(format!(
                    "- repository - name={} url={} path={} path_source={} status={}",
                    display_value(&repo.name),
                    display_value(&repo.address),
                    display_value(&repo.path),
                    self.repository_space_path_source(repo, space),
                    self.repository_status_name(repo)
                ));
            }
            if count == 0 {
                lines.push("- repository - none configured".to_string());
            }
        }

        lines.push("Standalone repositories:".to_string());
        let mut standalone_count = 0usize;
        for repo in self.standalone_repositories() {
            standalone_count += 1;
            lines.push(format!(
                "- repository - name={} url={} path={} status={}",
                display_value(&repo.name),
                display_value(&repo.address),
                display_value(&repo.path),
                self.repository_status_name(repo)
            ));
        }
        if standalone_count == 0 {
            lines.push("- repository - none configured".to_string());
        }

        lines
    }

    fn repositories_for_space<'a>(
        &'a self,
        space_id: &str,
    ) -> impl Iterator<Item = &'a Repository> {
        self.repository_view_state
            .repositories
            .iter()
            .filter(move |repo| repo.repository_space_id.as_deref() == Some(space_id))
    }

    fn standalone_repositories(&self) -> impl Iterator<Item = &Repository> {
        self.repository_view_state
            .repositories
            .iter()
            .filter(|repo| {
                repo.repository_space_id.as_deref().is_none_or(|space_id| {
                    !self
                        .repository_spaces
                        .iter()
                        .any(|space| space.id == space_id)
                })
            })
    }

    fn repository_space_path_source(
        &self,
        repo: &Repository,
        space: &RepositorySpace,
    ) -> &'static str {
        let repo_path = Self::repo_instance_path_key(&repo.path);
        let space_path = Self::repo_instance_path_key(&space.shared_path);
        if !space_path.is_empty() && repo_path == space_path {
            "inherited"
        } else {
            "override"
        }
    }

    fn repository_status_name(&self, repo: &Repository) -> &'static str {
        match self.repo_state_for_address(&repo.address, &repo.path) {
            crate::ui::types::RepoState::Synced => "synced",
            crate::ui::types::RepoState::PendingUpdate => "pending-update",
            crate::ui::types::RepoState::Updating => "updating",
            crate::ui::types::RepoState::Unknown => "unknown",
        }
    }
}

fn repository_space_url(space: &RepositorySpace) -> &str {
    if !space.source_address.trim().is_empty() {
        &space.source_address
    } else {
        &space.source_base_url
    }
}

fn display_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "n/a".to_string()
    } else {
        trimmed.to_string()
    }
}
