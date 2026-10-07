use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use egui::RichText;
use log::info;

use crate::core::api::StartupRepositoryInstance;
use crate::core::utils::storage_compat::{
    StorageIssue, StorageIssueCode, StorageIssueSeverity, WINDOWS_MAX_PATH_CHARS,
};
use crate::ui::app::Foxy;
use crate::ui::palette;

/// Locations shown per finding before the rest collapse into a count.
const NOTICE_LOCATION_LIMIT: usize = 3;

/// One row of the "Storage check" window: a finding plus every configured
/// location it applies to (several repositories on one FAT32 drive share a
/// row).
#[derive(Clone, Debug)]
pub(crate) struct StorageNoticeRow {
    pub(crate) issue: StorageIssue,
    pub(crate) locations: Vec<(String, PathBuf)>,
}

/// Startup storage findings queued for the "Storage check" window.
#[derive(Clone, Debug)]
pub(crate) struct StorageCompatNotice {
    pub(crate) rows: Vec<StorageNoticeRow>,
    pub(crate) fingerprint: String,
    pub(crate) suppress_future: bool,
}

/// Order-independent identity of a set of findings, stored in settings when
/// the user asks not to see them again.
pub(crate) fn storage_notice_fingerprint(rows: &[StorageNoticeRow]) -> String {
    let mut parts: Vec<String> = rows
        .iter()
        .map(|row| row.issue.fingerprint_component())
        .collect();
    parts.sort();
    parts.dedup();
    blake3::hash(parts.join("\n").as_bytes())
        .to_hex()
        .to_string()
}

/// Collapse raw findings into rows: advisories stay in the log, and findings
/// with the same identity merge their locations. Blocking rows come first.
pub(crate) fn storage_notice_rows(issues: Vec<StorageIssue>) -> Vec<StorageNoticeRow> {
    let mut rows: BTreeMap<String, StorageNoticeRow> = BTreeMap::new();
    let mut order = Vec::new();
    for issue in issues {
        if issue.severity < StorageIssueSeverity::Warning {
            continue;
        }
        let key = issue.fingerprint_component();
        let location = (issue.role.clone(), issue.path.clone());
        match rows.get_mut(&key) {
            Some(row) => {
                if !row.locations.contains(&location) {
                    row.locations.push(location);
                }
            }
            None => {
                order.push(key.clone());
                rows.insert(
                    key,
                    StorageNoticeRow {
                        issue,
                        locations: vec![location],
                    },
                );
            }
        }
    }
    let mut result: Vec<StorageNoticeRow> = order
        .into_iter()
        .filter_map(|key| rows.remove(&key))
        .collect();
    result.sort_by_key(|row| std::cmp::Reverse(row.issue.severity));
    result
}

impl Foxy {
    /// Repository instances the background storage check may query. Empty
    /// while the database must not be opened (another Foxy owns it, or a
    /// schema wipe is pending) and in debug mode, mirroring the quick scan.
    pub(crate) fn storage_check_repositories(&self) -> Vec<StartupRepositoryInstance> {
        if self.settings_view_state.debug_mode
            || self.db_lock_conflict.is_some()
            || self.pending_db_schema_wipe.is_some()
        {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        let mut instances = Vec::new();
        for repo in &self.repository_view_state.repositories {
            let local_path = repo.path.trim();
            if repo.address.trim().is_empty() || local_path.is_empty() {
                continue;
            }
            let repo_url = Self::normalize_repo_url(&repo.address);
            let key = (
                repo_url.clone(),
                crate::core::utils::content_hash::normalize_path(local_path),
            );
            if !seen.insert(key) {
                continue;
            }
            instances.push(StartupRepositoryInstance {
                repo_url,
                local_path: local_path.to_string(),
            });
        }
        instances
    }

    /// Turn the background report into the window's content, or `None` when
    /// there is nothing above advisory level or the user already acknowledged
    /// exactly these findings.
    pub(crate) fn build_storage_compat_notice(
        &self,
        issues: Vec<StorageIssue>,
    ) -> Option<StorageCompatNotice> {
        let rows = storage_notice_rows(issues);
        if rows.is_empty() {
            return None;
        }
        let fingerprint = storage_notice_fingerprint(&rows);
        if fingerprint == self.settings_view_state.storage_notice_acknowledged {
            info!(
                "Storage check: {} finding(s) match the acknowledged set; notice not shown",
                rows.len()
            );
            return None;
        }
        Some(StorageCompatNotice {
            rows,
            fingerprint,
            suppress_future: false,
        })
    }

    pub(crate) fn dismiss_storage_compat_notice(&mut self) {
        let Some(notice) = self.storage_compat_notice.take() else {
            return;
        };
        if notice.suppress_future
            && !self.previewing_debug_modal(crate::ui::app::debug_modals::DebugModal::StorageCheck)
        {
            self.settings_view_state.storage_notice_acknowledged = notice.fingerprint;
            self.save_settings();
            info!(
                "Storage check findings acknowledged; the notice stays hidden while they are unchanged"
            );
        }
        self.needs_repaint = true;
    }

    pub(in crate::ui::app) fn render_storage_compat_notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.storage_compat_notice.clone() else {
            return;
        };
        let has_blocking = notice
            .rows
            .iter()
            .any(|row| row.issue.severity == StorageIssueSeverity::Blocking);
        let mut suppress_future = notice.suppress_future;
        let mut ok_clicked = false;

        egui::Window::new(self.t("Storage check"))
            .frame(self.modal_window_chrome(ctx))
            .title_frame(self.modal_window_chrome(ctx))
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.set_max_width(560.0);
                ui.label(if has_blocking {
                    self.t(
                        "Some of the locations Foxy uses sit on a drive or filesystem it cannot use safely. Fix the items marked as unsafe before syncing.",
                    )
                } else {
                    self.t(
                        "Some of the locations Foxy uses sit on a drive or filesystem with known risks. Foxy keeps working, but read the notes below before syncing large repositories.",
                    )
                });
                ui.add_space(10.0);

                egui::ScrollArea::vertical()
                    .id_salt("storage_compat_notice_rows")
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (index, row) in notice.rows.iter().enumerate() {
                            if index > 0 {
                                ui.add_space(8.0);
                            }
                            self.render_storage_notice_row(ui, row);
                        }
                    });

                ui.add_space(12.0);
                Foxy::ui_state_checkbox(
                    ui,
                    &mut suppress_future,
                    self.t("Do not show this again for these locations"),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let ok_btn = ui.button(self.t("OK"));
                    if ok_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if ok_btn.clicked() {
                        ok_clicked = true;
                    }
                });
            });

        if let Some(live) = self.storage_compat_notice.as_mut() {
            live.suppress_future = suppress_future;
        }
        if ok_clicked {
            self.dismiss_storage_compat_notice();
        }
    }

    fn render_storage_notice_row(&self, ui: &mut egui::Ui, row: &StorageNoticeRow) {
        let issue = &row.issue;
        let (tag, color) = match issue.severity {
            StorageIssueSeverity::Blocking => (self.t("Unsafe"), palette::ERROR),
            _ => (self.t("Warning"), palette::WARN),
        };
        egui::Frame::NONE
            .stroke(egui::Stroke::new(1.0, color))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(tag).color(color).strong());
                    ui.label(
                        RichText::new(format!(
                            "{} ({})",
                            issue.volume_root.display(),
                            issue.filesystem
                        ))
                        .strong(),
                    );
                });
                ui.add_space(4.0);
                ui.label(self.storage_issue_message(issue));
                ui.add_space(4.0);
                for (role, path) in row.locations.iter().take(NOTICE_LOCATION_LIMIT) {
                    ui.label(
                        RichText::new(format!(
                            "{}: {}",
                            self.storage_role_label(role),
                            path.display()
                        ))
                        .weak(),
                    );
                }
                let hidden = row.locations.len().saturating_sub(NOTICE_LOCATION_LIMIT);
                if hidden > 0 {
                    ui.label(
                        RichText::new(self.t_fmt(
                            "and {count} more locations",
                            &[("count", hidden.to_string())],
                        ))
                        .weak(),
                    );
                }
            });
    }

    fn storage_role_label(&self, role: &str) -> String {
        match role {
            "database" | "game_space" | "app_data" => self.t("Foxy data"),
            "logs" => self.t("Logs"),
            "temp" => self.t("Temporary files"),
            "backups" => self.t("Backups"),
            "repository_space" => self.t("Repository space folder"),
            _ => self.t("Repository folder"),
        }
    }

    fn storage_issue_message(&self, issue: &StorageIssue) -> String {
        match issue.code {
            StorageIssueCode::ReadOnlyVolume => {
                self.t("The drive is read-only. Foxy cannot write here.")
            }
            StorageIssueCode::DatabaseOnNetworkShare => self.t(
                "Foxy's database is stored on a network share. File locking is unreliable over a network and the database can be corrupted. Keep Foxy's data on a local drive (set FOXY_CONFIG_DIR or use a local user profile).",
            ),
            StorageIssueCode::NetworkShare => self.t(
                "This folder is on a network share. Syncing is slow, and a dropped connection during a download leaves half-written files behind. A local drive is strongly recommended.",
            ),
            StorageIssueCode::VolatileStorage => self.t(
                "This location is memory-backed storage (a RAM disk or tmpfs). Everything stored here is lost at reboot.",
            ),
            StorageIssueCode::FatFileSizeLimit => self.t(
                "The drive is formatted as FAT32. It cannot store files larger than 4 GiB, so repositories with large addons cannot be downloaded here, and it is not journaled, so an interrupted download can corrupt the drive. Reformat the drive as NTFS or exFAT, or choose another folder.",
            ),
            StorageIssueCode::NotJournaled => self.t_fmt(
                "The drive is formatted as {fs}, which is not journaled. A crash, power loss or unplugged drive during a write can corrupt the whole drive, not only the file being written. NTFS is recommended for Foxy's data.",
                &[("fs", issue.filesystem.clone())],
            ),
            StorageIssueCode::RemovableStateDrive => self.t(
                "Foxy's database is on a removable drive. Unplugging it while Foxy is running corrupts the local state and forces a rebuild.",
            ),
            StorageIssueCode::FileExceedsFilesystemLimit => self.t_fmt(
                "{count} files in this repository are larger than the 4 GiB limit of the drive's FAT32 filesystem (largest: {size}). They cannot be downloaded to this folder. Move the repository to an NTFS or exFAT drive.",
                &[
                    ("count", issue.affected_files.to_string()),
                    ("size", Self::format_bytes_short(issue.largest_file_bytes)),
                ],
            ),
            StorageIssueCode::PathTooLongForWindows => self.t_fmt(
                "{count} files in this repository have paths at or beyond the Windows limit of {limit} characters (longest: {longest}). Windows programs that are not long-path aware, including most games, cannot open them. Use a shorter repository folder, such as D:\\Mods.",
                &[
                    ("count", issue.affected_files.to_string()),
                    ("limit", WINDOWS_MAX_PATH_CHARS.to_string()),
                    ("longest", issue.longest_path_chars.to_string()),
                ],
            ),
            StorageIssueCode::InvalidWindowsName => self.t_fmt(
                "{count} files in this repository have names Windows cannot create (for example {example}). The repository maintainer has to rename them.",
                &[
                    ("count", issue.affected_files.to_string()),
                    ("example", issue.example.clone()),
                ],
            ),
            StorageIssueCode::CaseCollision => self.t_fmt(
                "Two files in this repository differ only by letter case ({example}) and cannot coexist on this drive.",
                &[("example", issue.example.clone())],
            ),
            StorageIssueCode::CopyOnWriteDatabase => self.t(
                "The database sits on a copy-on-write filesystem, which fragments database files over time. Consider disabling copy-on-write for Foxy's data folder.",
            ),
            StorageIssueCode::CaseSensitiveContent => self.t(
                "This drive is case-sensitive. Addon folders must match the letter case the game expects.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::utils::storage_compat::{FilesystemFamily, VolumeInfo, evaluate_volume};
    use std::path::Path;

    fn fat_volume(root: &str) -> VolumeInfo {
        VolumeInfo {
            root: PathBuf::from(root),
            filesystem: "FAT32".to_string(),
            family: FilesystemFamily::Fat,
            removable: false,
            remote: false,
            read_only: false,
        }
    }

    #[test]
    fn rows_merge_locations_on_the_same_drive_and_drop_advisories() {
        let vol = fat_volume("D:\\");
        let mut issues = evaluate_volume("repository", Path::new("D:\\mods\\a"), &vol);
        issues.extend(evaluate_volume(
            "repository",
            Path::new("D:\\mods\\b"),
            &vol,
        ));
        issues.extend(evaluate_volume("temp", Path::new("D:\\tmp"), &vol));
        let mut btrfs = fat_volume("/");
        btrfs.filesystem = "btrfs".to_string();
        btrfs.family = FilesystemFamily::Btrfs;
        issues.extend(evaluate_volume(
            "database",
            Path::new("/home/x/foxy"),
            &btrfs,
        ));

        let rows = storage_notice_rows(issues);
        assert_eq!(rows.len(), 1, "one FAT32 row, advisory dropped");
        assert_eq!(rows[0].issue.code, StorageIssueCode::FatFileSizeLimit);
        assert_eq!(rows[0].locations.len(), 3);
        assert_eq!(rows[0].locations[0].0, "repository");
        assert_eq!(rows[0].locations[2].0, "temp");
    }

    #[test]
    fn blocking_rows_sort_first_but_keep_discovery_order_otherwise() {
        let fat = fat_volume("D:\\");
        let mut read_only = fat_volume("E:\\");
        read_only.read_only = true;
        let mut issues = evaluate_volume("repository", Path::new("D:\\mods"), &fat);
        issues.extend(evaluate_volume(
            "repository",
            Path::new("E:\\mods"),
            &read_only,
        ));
        let rows = storage_notice_rows(issues);
        let codes: Vec<_> = rows.iter().map(|row| row.issue.code).collect();
        assert_eq!(
            codes,
            vec![
                StorageIssueCode::ReadOnlyVolume,
                StorageIssueCode::FatFileSizeLimit,
                StorageIssueCode::FatFileSizeLimit,
            ]
        );
        assert_eq!(rows[1].issue.volume_root, PathBuf::from("D:\\"));
        assert_eq!(rows[2].issue.volume_root, PathBuf::from("E:\\"));
    }

    #[test]
    fn fingerprint_is_order_independent_and_changes_with_the_drive_set() {
        let fat_d = fat_volume("D:\\");
        let fat_e = fat_volume("E:\\");
        let mut forward = evaluate_volume("repository", Path::new("D:\\mods"), &fat_d);
        forward.extend(evaluate_volume("repository", Path::new("E:\\mods"), &fat_e));
        let mut reverse = evaluate_volume("repository", Path::new("E:\\mods"), &fat_e);
        reverse.extend(evaluate_volume(
            "repository",
            Path::new("D:\\other"),
            &fat_d,
        ));
        let a = storage_notice_fingerprint(&storage_notice_rows(forward));
        let b = storage_notice_fingerprint(&storage_notice_rows(reverse));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);

        let only_d = evaluate_volume("repository", Path::new("D:\\mods"), &fat_d);
        assert_ne!(a, storage_notice_fingerprint(&storage_notice_rows(only_d)));
    }
}
