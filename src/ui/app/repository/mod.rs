mod actions;
mod addons;
pub(crate) mod game_space_overview;
mod join_preflight;
mod linked_targets;
mod list_cache;
mod local;
mod maintenance;
mod media;
pub(crate) mod prelaunch_checks;
mod reorder;
mod selection_check;
pub(crate) mod space_actions;
pub(crate) mod space_freshness;
mod space_selection;
mod space_settings;
mod startup_layout;
mod sync;
mod ts3_plugins;

use crate::ui::app::Foxy;

impl Foxy {
    pub fn normalize_repo_url(repo_url: &str) -> String {
        crate::core::models::repository::normalize_repository_url(repo_url)
    }

    /// Use this case-insensitive key for matching addresses only, never as a request URL or persisted key.
    pub fn repo_url_match_key(repo_url: &str) -> String {
        crate::core::models::repository::repository_url_match_key(repo_url)
    }
}
