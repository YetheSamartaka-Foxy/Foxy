mod actions;
mod addons;
mod join_preflight;
mod list_cache;
mod maintenance;
mod media;
mod reorder;
mod space_actions;
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
}
