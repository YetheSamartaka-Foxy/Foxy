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
    /// "Storage check" filesystem findings window.
    StorageCheck,
}

impl DebugModal {
    /// Stable name used in logs and the agent driver.
    pub fn as_str(self) -> &'static str {
        match self {
            DebugModal::AppUpdate => "app-update",
            DebugModal::DbSchemaWipe => "db-schema-wipe",
            DebugModal::StorageCheck => "storage-check",
        }
    }

    /// Populate the state the prompt renders from, using placeholder values.
    fn seed(self, app: &mut Foxy) {
        match self {
            DebugModal::AppUpdate => {
                let current_version = env!("CARGO_PKG_VERSION").to_string();
                let latest = preview_next_version(&current_version);
                let (versions, changelogs) = preview_app_update_versions(&current_version, &latest);
                app.app_update_status =
                    crate::core::tasks::app_update::UpdateCheckStatus::Available(
                        crate::core::tasks::app_update::AppUpdateInfo {
                            source_base_url: String::new(),
                            manifest: crate::core::tasks::app_update::UpdateManifest {
                                schema_version: 1,
                                latest,
                                versions,
                            },
                            current_version,
                            fetched_changelogs: changelogs.clone(),
                        },
                    );
                app.app_update_changelogs = changelogs;
                app.app_update_changelogs_requested = true;
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
            DebugModal::StorageCheck => {
                app.storage_compat_notice = Some(preview_storage_notice());
            }
        }
    }
}

/// Placeholder findings covering every row style: a blocking per-repository
/// limit, a blocking volume finding, and a warning shared by two folders.
fn preview_storage_notice() -> crate::ui::app::runtime::StorageCompatNotice {
    use crate::core::utils::storage_compat::{
        FAT_MAX_FILE_BYTES, FilesystemFamily, PathLimitReport, VolumeInfo, evaluate_volume,
    };
    use crate::ui::app::runtime::{storage_notice_fingerprint, storage_notice_rows};
    use std::path::{Path, PathBuf};

    let fat = VolumeInfo {
        root: PathBuf::from("E:\\"),
        filesystem: "FAT32".to_string(),
        family: FilesystemFamily::Fat,
        removable: true,
        remote: false,
        read_only: false,
    };
    let share = VolumeInfo {
        root: PathBuf::from("\\\\nas\\foxy\\"),
        filesystem: "NTFS".to_string(),
        family: FilesystemFamily::Ntfs,
        removable: false,
        remote: true,
        read_only: false,
    };
    let repo = Path::new("E:\\Mods\\Main Repository");
    let mut issues = evaluate_volume("repository", repo, &fat);
    issues.extend(evaluate_volume("temp", Path::new("E:\\Foxy Temp"), &fat));
    issues.extend(evaluate_volume(
        "database",
        Path::new("\\\\nas\\foxy\\games\\arma3\\database.db"),
        &share,
    ));
    let files = [
        (
            "E:\\Mods\\Main Repository\\@map\\addons\\terrain.pbo",
            FAT_MAX_FILE_BYTES + 1,
        ),
        (
            "E:\\Mods\\Main Repository\\@map\\addons\\world.pbo",
            6_442_450_944,
        ),
    ];
    issues.extend(
        PathLimitReport::from_files(files.iter().copied(), &fat).issues("repository", repo, &fat),
    );
    let rows = storage_notice_rows(issues);
    let fingerprint = storage_notice_fingerprint(&rows);
    crate::ui::app::runtime::StorageCompatNotice {
        rows,
        fingerprint,
        suppress_future: false,
    }
}

/// Manifest entries and changelogs for the app update preview, so the update
/// and version browser views render changelogs and download buttons exactly as
/// a real release does. The text is placeholder data, deliberately English only.
fn preview_app_update_versions(
    current: &str,
    latest: &str,
) -> (
    Vec<crate::core::tasks::app_update::VersionEntry>,
    Vec<crate::core::tasks::app_update::ChangelogVersion>,
) {
    use crate::core::tasks::app_update::{
        ChangelogSection, ChangelogVersion, PlatformEntry, VersionEntry, current_platform_key,
        default_installer_hash_algorithm,
    };

    let version_entry = |version: &str, installer_size: u64| VersionEntry {
        version: version.to_string(),
        changelog: format!("changelogs/{version}.json"),
        platforms: [(
            current_platform_key().to_string(),
            PlatformEntry {
                installer_path: format!("installers/Foxy-{version}-setup.exe"),
                installer_hash: "0".repeat(64),
                installer_hash_algorithm: default_installer_hash_algorithm(),
                installer_size,
            },
        )]
        .into_iter()
        .collect(),
    };
    let section = |title: &str, items: &[&str]| ChangelogSection {
        title: title.to_string(),
        items: items.iter().map(|item| item.to_string()).collect(),
    };

    let versions = vec![
        version_entry(latest, 48_234_496),
        version_entry(current, 47_710_208),
    ];
    let changelogs = vec![
        ChangelogVersion {
            version: latest.to_string(),
            date: "Preview release".to_string(),
            sections: vec![
                section(
                    "Added",
                    &[
                        "Preview entry: repositories can be rechecked in bulk after a database reset.",
                        "Preview entry: the update prompt shows the installed and new version side by side.",
                    ],
                ),
                section(
                    "Changed",
                    &["Preview entry: startup prompts use a stronger warning style."],
                ),
                section(
                    "Fixed",
                    &[
                        "Preview entry: a long changelog line wraps inside the update view instead of widening the window past the screen edge.",
                        "Preview entry: the footer update badge stays visible at every font size.",
                    ],
                ),
            ],
        },
        ChangelogVersion {
            version: current.to_string(),
            date: "Installed release".to_string(),
            sections: vec![section(
                "Fixed",
                &["Preview entry: placeholder notes for the version you are running."],
            )],
        },
    ];
    (versions, changelogs)
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
    fn app_update_preview_offers_an_installer_and_changelog_for_the_latest_version() {
        let (versions, changelogs) = preview_app_update_versions("1.2.0", "1.2.1");
        let latest = versions
            .iter()
            .find(|entry| entry.version == "1.2.1")
            .expect("latest version entry");
        assert!(
            latest
                .platforms
                .contains_key(crate::core::tasks::app_update::current_platform_key())
        );
        assert!(
            changelogs
                .iter()
                .any(|changelog| changelog.version == "1.2.1" && !changelog.sections.is_empty())
        );
    }

    #[test]
    fn debug_modal_names_are_stable() {
        assert_eq!(DebugModal::AppUpdate.as_str(), "app-update");
        assert_eq!(DebugModal::DbSchemaWipe.as_str(), "db-schema-wipe");
        assert_eq!(DebugModal::StorageCheck.as_str(), "storage-check");
    }

    #[test]
    fn storage_check_preview_covers_blocking_and_warning_rows() {
        use crate::core::utils::storage_compat::{StorageIssueCode, StorageIssueSeverity};
        let notice = preview_storage_notice();
        let codes: Vec<_> = notice.rows.iter().map(|row| row.issue.code).collect();
        assert!(codes.contains(&StorageIssueCode::FileExceedsFilesystemLimit));
        assert!(codes.contains(&StorageIssueCode::DatabaseOnNetworkShare));
        assert!(codes.contains(&StorageIssueCode::FatFileSizeLimit));
        assert_eq!(
            notice.rows[0].issue.severity,
            StorageIssueSeverity::Blocking
        );
        let fat_row = notice
            .rows
            .iter()
            .find(|row| row.issue.code == StorageIssueCode::FatFileSizeLimit)
            .expect("fat row");
        assert_eq!(fat_row.locations.len(), 2);
        assert!(!notice.fingerprint.is_empty());
    }
}
