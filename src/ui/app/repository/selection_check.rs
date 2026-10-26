use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use log::info;

use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::types::{RepoState, Repository};

impl Foxy {
    /// The addons a sync of `repo` treats as enabled, after its selected profile.
    pub(crate) fn addon_selection_fingerprint(repo: &Repository) -> u64 {
        let profile = repo
            .selected_profile
            .as_deref()
            .and_then(|name| repo.profiles.iter().find(|profile| profile.name == name));
        let (addons, optional_addons) = match profile {
            Some(profile) => (&profile.addons, &profile.optional_addons),
            None => (&repo.addons, &repo.optional_addons),
        };
        let mut enabled: Vec<String> = addons
            .iter()
            .chain(optional_addons.iter())
            .filter(|(_, enabled)| *enabled)
            .map(|(name, _)| name.to_lowercase())
            .collect();
        enabled.sort_unstable();
        enabled.dedup();
        let mut hasher = DefaultHasher::new();
        enabled.hash(&mut hasher);
        hasher.finish()
    }

    pub(in crate::ui::app) fn reset_addon_selection_baseline(&mut self) {
        self.pending_addon_selection_checks.clear();
        self.addon_selection_baseline = self
            .repository_view_state
            .repositories
            .iter()
            .map(|repo| {
                (
                    Self::repo_instance_key(&repo.address, &repo.path),
                    Self::addon_selection_fingerprint(repo),
                )
            })
            .collect();
    }

    /// A starting sync stores the repository's selection in the database, which
    /// makes a queued selection check for it redundant.
    pub(in crate::ui::app) fn note_addon_selection_synced(&mut self, repo_idx: usize) {
        let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
            return;
        };
        let key = Self::repo_instance_key(&repo.address, &repo.path);
        self.addon_selection_baseline
            .insert(key.clone(), Self::addon_selection_fingerprint(repo));
        self.pending_addon_selection_checks.remove(&key);
    }

    /// Background checks read the selection stored in the database, which only
    /// a sync updates, so a changed selection gets its own quick local check.
    pub(in crate::ui::app) fn queue_checks_for_changed_addon_selections(&mut self) {
        for repo in &self.repository_view_state.repositories {
            let key = Self::repo_instance_key(&repo.address, &repo.path);
            let fingerprint = Self::addon_selection_fingerprint(repo);
            if self
                .addon_selection_baseline
                .insert(key.clone(), fingerprint)
                .is_some_and(|previous| previous != fingerprint)
                && self.pending_addon_selection_checks.insert(key)
            {
                info!(
                    "Addon selection changed for {}; queueing a quick local check",
                    repo.name
                );
            }
        }
    }

    fn addon_selection_check_blocked(&self) -> bool {
        !self.startup_tasks_started
            || self.close_requested_at.is_some()
            || self.database_updates_blocked()
            || self.repository_sync_active()
            || self.is_direct_download_running()
            || self.quick_scan_worker.is_some()
            || self.startup_pending_restore_worker.is_some()
            || !self.startup_recheck_queue.is_empty()
            || !self.repository_space_sync_queue.is_empty()
            || !self.repository_visual_folder_sync_queue.is_empty()
            || self.repository_space_bulk_progress.is_some()
            || self.scheduler_active_run.is_some()
            || self.scheduler_pending_post_action.is_some()
            || !self.scheduler_held_jobs.is_empty()
            || !self.pending_repository_db_wipes.is_empty()
            || self.benchmark_armed.is_some()
            || self.update_modal_open
    }

    /// Start the next queued selection check once nothing else needs the sync
    /// slot. A repository whose status is still unknown waits for its first
    /// check or restored result.
    pub(in crate::ui::app) fn process_addon_selection_checks(&mut self) {
        if self.pending_addon_selection_checks.is_empty() || self.addon_selection_check_blocked() {
            return;
        }
        let mut still_configured = std::collections::HashSet::new();
        let mut next = None;
        for (idx, repo) in self.repository_view_state.repositories.iter().enumerate() {
            let key = Self::repo_instance_key(&repo.address, &repo.path);
            if !self.pending_addon_selection_checks.contains(&key) {
                continue;
            }
            if next.is_none()
                && self.repo_state_for_address(&repo.address, &repo.path) != RepoState::Unknown
            {
                next = Some((idx, key.clone()));
            }
            still_configured.insert(key);
        }
        self.pending_addon_selection_checks
            .retain(|key| still_configured.contains(key));
        let Some((repo_idx, key)) = next else {
            return;
        };
        self.pending_addon_selection_checks.remove(&key);
        info!(
            "Running a quick local check for {} after its addon selection changed",
            self.repository_view_state.repositories[repo_idx].name
        );
        self.start_core_sync(repo_idx, SyncMode::QuickCheckOnly);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::types::RepositoryProfile;

    #[test]
    fn the_fingerprint_follows_enabled_addons_and_the_selected_profile() {
        let mut repo = Repository {
            addons: vec![("@mod_01".to_string(), true)],
            optional_addons: vec![
                ("@mod_02".to_string(), false),
                ("@mod_03".to_string(), true),
            ],
            ..Repository::default()
        };
        let base = Foxy::addon_selection_fingerprint(&repo);

        repo.optional_addons.reverse();
        repo.addons[0].0 = "@MOD_01".to_string();
        assert_eq!(Foxy::addon_selection_fingerprint(&repo), base);

        repo.optional_addons[1].1 = true;
        let enabled_more = Foxy::addon_selection_fingerprint(&repo);
        assert_ne!(enabled_more, base);

        repo.profiles.push(RepositoryProfile {
            name: "Lean".to_string(),
            addons: vec![("@mod_01".to_string(), true)],
            ..RepositoryProfile::default()
        });
        assert_eq!(Foxy::addon_selection_fingerprint(&repo), enabled_more);
        repo.selected_profile = Some("Lean".to_string());
        assert_ne!(Foxy::addon_selection_fingerprint(&repo), enabled_more);
    }
}
