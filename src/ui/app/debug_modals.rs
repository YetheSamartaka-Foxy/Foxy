//! Debug previews for startup modals.
//!
//! Startup prompts (app update available, database schema wipe) only appear
//! when real conditions are met, which makes them awkward to inspect while
//! iterating on their layout or copy. `foxy ui --debug-modal <name>` seeds the
//! state each prompt reads so it renders immediately with placeholder data.
//!
//! Add a new preview by adding a variant here plus its arm in
//! [`DebugModal::seed`]; nothing else in the launch path needs to change. Every
//! preview is inert: confirm actions that would touch real data are skipped
//! while the preview is active.

use clap::ValueEnum;

use crate::ui::app::Foxy;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DebugModal {
    /// "Foxy update available" launch prompt.
    AppUpdate,
    /// "Database update required" schema wipe prompt.
    DbSchemaWipe,
}

impl DebugModal {
    /// Stable name used in logs and the agent driver.
    pub fn as_str(self) -> &'static str {
        match self {
            DebugModal::AppUpdate => "app-update",
            DebugModal::DbSchemaWipe => "db-schema-wipe",
        }
    }

    /// Populate the state the prompt renders from, using placeholder values.
    fn seed(self, app: &mut Foxy) {
        match self {
            DebugModal::AppUpdate => {
                let current_version = env!("CARGO_PKG_VERSION").to_string();
                app.app_update_status =
                    crate::core::tasks::app_update::UpdateCheckStatus::Available(
                        crate::core::tasks::app_update::AppUpdateInfo {
                            source_base_url: String::new(),
                            manifest: crate::core::tasks::app_update::UpdateManifest {
                                schema_version: 1,
                                latest: preview_next_version(&current_version),
                                versions: Vec::new(),
                            },
                            current_version,
                            fetched_changelogs: Vec::new(),
                        },
                    );
                app.pending_app_update_prompt = true;
            }
            DebugModal::DbSchemaWipe => {
                let target = crate::core::tasks::db_schema_version::DB_SCHEMA_VERSION;
                app.pending_db_schema_wipe =
                    Some(crate::core::tasks::db_schema_version::DbSchemaWipePrompt {
                        stored_version: target.saturating_sub(1),
                        target_version: target,
                        blocking: false,
                    });
            }
        }
    }
}

/// Bump the last numeric component of a semver-ish string for preview copy.
/// Falls back to a suffixed label when the version has no trailing number.
fn preview_next_version(current: &str) -> String {
    let mut parts: Vec<String> = current.split('.').map(str::to_string).collect();
    match parts
        .last()
        .and_then(|last| last.parse::<u64>().ok())
        .map(|n| n + 1)
    {
        Some(next) => {
            if let Some(last) = parts.last_mut() {
                *last = next.to_string();
            }
            parts.join(".")
        }
        None => format!("{}-preview", current),
    }
}

impl Foxy {
    /// Seed every modal preview requested on the command line.
    pub(crate) fn apply_debug_modal_previews(&mut self) {
        for modal in self.debug_modal_previews.clone() {
            log::info!("Debug modal preview enabled: {}", modal.as_str());
            modal.seed(self);
        }
    }

    /// Whether `modal` is being previewed, so its real side effects (wipes,
    /// dismissal markers, background checks) must be skipped.
    pub(crate) fn previewing_debug_modal(&self, modal: DebugModal) -> bool {
        self.debug_modal_previews.contains(&modal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_version_bumps_last_component() {
        assert_eq!(preview_next_version("1.2.0"), "1.2.1");
        assert_eq!(preview_next_version("2"), "3");
    }

    #[test]
    fn preview_version_falls_back_for_non_numeric_tail() {
        assert_eq!(preview_next_version("1.2.0-rc1"), "1.2.0-rc1-preview");
    }

    #[test]
    fn debug_modal_names_are_stable() {
        assert_eq!(DebugModal::AppUpdate.as_str(), "app-update");
        assert_eq!(DebugModal::DbSchemaWipe.as_str(), "db-schema-wipe");
    }
}
