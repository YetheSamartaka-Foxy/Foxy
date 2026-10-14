use eframe::egui;

use crate::ui::app::{Foxy, debug_modals::DebugModal};
use crate::ui::types::{Repository, RepositoryServer};
use crate::ui::views::repository::LaunchDispatchResult;

pub(crate) struct DatabaseLaunchContinuation {
    pub repository: Repository,
    pub server: Option<RepositoryServer>,
    pub repo_name: String,
    pub launch_label: &'static str,
}

impl Foxy {
    pub(crate) fn database_repair_prompt_visible(&self) -> bool {
        self.show_db_schema_wipe_prompt
            && self.pending_db_schema_wipe.is_some()
            && self.pending_join_preflight.is_none()
            && self.pending_join_status_query.is_none()
            && self.pending_join_preflight_query.is_none()
    }

    pub(crate) fn refresh_live_database_repair_requirement(&mut self) {
        if self.database_wipe_in_flight
            || self
                .pending_db_schema_wipe
                .is_some_and(|prompt| prompt.blocking)
            || self.previewing_debug_modal(DebugModal::DbSchemaWipe)
        {
            return;
        }
        if let Some(requirement) =
            crate::core::tasks::db_schema_version::blocking_prompt_if_live_schema_incompatible()
        {
            if self.pending_db_schema_wipe.is_none() {
                self.show_db_schema_wipe_prompt = true;
            }
            self.pending_db_schema_wipe = Some(requirement);
        }
    }

    pub(crate) fn refresh_database_repair_requirement(&mut self) {
        if self.database_wipe_in_flight || self.previewing_debug_modal(DebugModal::DbSchemaWipe) {
            return;
        }
        let requirement = crate::core::tasks::db_schema_version::current_wipe_requirement();
        if requirement.is_some() && self.pending_db_schema_wipe.is_none() {
            self.show_db_schema_wipe_prompt = true;
        }
        self.pending_db_schema_wipe = requirement;
        if requirement.is_none() {
            self.show_db_schema_wipe_prompt = false;
        }
    }

    pub(crate) fn database_updates_blocked(&self) -> bool {
        self.db_lock_conflict.is_some()
            || self.pending_db_schema_wipe.is_some()
            || self.database_wipe_in_flight
            || crate::core::tasks::db_schema_check::live_schema_incompatible()
    }

    pub(crate) fn block_database_update_request(&mut self) -> bool {
        self.refresh_database_repair_requirement();
        if !self.database_updates_blocked() {
            return false;
        }
        self.show_db_schema_wipe_prompt = true;
        self.needs_repaint = true;
        true
    }

    pub(crate) fn resume_database_launch(&mut self, ctx: &egui::Context) {
        let Some(launch) = self.pending_database_launch.take() else {
            return;
        };
        let result = self.launch_repository_with_steam_guard(
            &launch.repository,
            launch.server.as_ref(),
            &launch.repo_name,
            launch.launch_label,
        );
        if result == LaunchDispatchResult::Launched {
            self.handle_post_launch_window_behavior(ctx, "launch completed");
        }
    }
}
