use crate::core::utils::app_paths;
use crate::ui::app::{BackupManagerConfirmAction, Foxy};
use crate::ui::i18n::{fmt_date, locale_compare, tr};
use crate::ui::search_filter::MultiEntryFilter;
use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui};
use log::warn;
use std::collections::BTreeMap;
use std::path::Path;

use super::render_wrapped_info_row;

impl Foxy {
    pub(super) fn render_backup_manager_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;
        let mut changed = false;

        if !self.backup_manager_loaded && !self.is_backup_manager_inventory_refresh_pending() {
            self.refresh_backup_manager_inventory();
        }

        let backup_root = self
            .configured_backup_directory()
            .unwrap_or_else(app_paths::foxy_backups_dir);
        let filter_lower = self.backup_manager_filter.trim().to_lowercase();
        let multi_filter = MultiEntryFilter::parse(&self.backup_manager_filter);

        // Rebuild the cached grouped view only when records or filter change.
        let cache_stale = self.backup_manager_view_cache.as_ref().is_none_or(|cache| {
            cache.records_version != self.backup_manager_records_version
                || cache.filter != filter_lower
        });
        if cache_stale {
            let total_backups = self.backup_manager_records.len();
            let total_bytes: u64 = self
                .backup_manager_records
                .iter()
                .map(|record| record.size_bytes)
                .sum();
            let addon_count = self
                .backup_manager_records
                .iter()
                .map(|record| record.addon_name.to_lowercase())
                .collect::<std::collections::HashSet<_>>()
                .len();
            let mut grouped_backups: BTreeMap<
                String,
                Vec<crate::core::utils::addon_backup::AddonBackupRecord>,
            > = BTreeMap::new();
            for record in &self.backup_manager_records {
                if !multi_filter.is_empty() {
                    let matches_filter = multi_filter.matches_any(&[
                        record.addon_name.as_str(),
                        record.content_hash.as_str(),
                        record.folder_name.as_str(),
                    ]);
                    if !matches_filter {
                        continue;
                    }
                }
                grouped_backups
                    .entry(record.addon_name.to_lowercase())
                    .or_default()
                    .push(record.clone());
            }
            self.backup_manager_view_cache = Some(crate::ui::app::BackupManagerViewCache {
                records_version: self.backup_manager_records_version,
                filter: filter_lower.clone(),
                total_backups,
                total_bytes,
                addon_count,
                grouped_backups,
            });
        }
        let view_cache = self.backup_manager_view_cache.clone().unwrap();
        let total_backups = view_cache.total_backups;
        let total_bytes = view_cache.total_bytes;
        let addon_count = view_cache.addon_count;
        let mut grouped_backups = view_cache.grouped_backups;

        let mut open_folder_path: Option<std::path::PathBuf> = None;

        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical(|ui| {
                render_wrapped_info_row(
                    ui,
                    horizontal_padding,
                    RichText::new(format!(
                        "{} {}",
                        '\u{2139}',
                        tr("Manage stored addon backups here. Restore remains in repository settings, where the target addon path is known.")
                    ))
                    .italics()
                    .color(self.color_text_dim()),
                );
                ui.separator();

                if let Some(notice) = &self.backup_manager_notice {
                    render_wrapped_info_row(
                        ui,
                        horizontal_padding,
                        RichText::new(notice.message.clone()).color(if notice.success {
                            self.color_warn()
                        } else {
                            self.color_text_error()
                        }),
                    );
                    ui.separator();
                }

                render_wrapped_info_row(
                    ui,
                    horizontal_padding,
                    RichText::new(self.t_fmt(
                        "Backup manager is tracking {backups} backups across {addons} addons using {size}.",
                        &[
                            ("backups", total_backups.to_string()),
                            ("addons", addon_count.to_string()),
                            ("size", Self::format_bytes_short(total_bytes)),
                        ],
                    ))
                    .color(self.color_text_normal()),
                );
                render_wrapped_info_row(
                    ui,
                    horizontal_padding,
                    RichText::new(self.t_fmt(
                        "Backup root: {path}",
                        &[("path", backup_root.display().to_string())],
                    ))
                    .italics()
                    .color(self.color_text_dim()),
                );
                if self.is_backup_manager_inventory_refresh_pending() {
                    ui.horizontal(|ui| {
                        ui.add_space(horizontal_padding);
                        ui.spinner();
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(self.t("Refreshing addon backup inventory..."))
                                .color(self.color_text_dim()),
                        );
                    });
                }
                ui.separator();

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);

                    let refresh_button = ui.button(tr("Refresh"))
                        .on_hover_text(tr("Reload the backup inventory from disk."));
                    if refresh_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if refresh_button.clicked() {
                        self.refresh_backup_manager_inventory();
                    }

                    let open_button = ui.button(tr("Open folder"))
                        .on_hover_text(tr("Open the backup root folder in Explorer."));
                    if open_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if open_button.clicked() {
                        open_folder_path = Some(backup_root.clone());
                    }

                    let cleanup_button = ui.button(tr("Run cleanup now"))
                        .on_hover_text(tr("Apply the configured retention rules and delete old backups now."));
                    if cleanup_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cleanup_button.clicked() {
                        self.backup_manager_confirm_action =
                            Some(BackupManagerConfirmAction::RunCleanup);
                    }
                });
                ui.separator();

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(tr("Keep latest backups per addon"))
                        .on_hover_text(tr("Keep this many most recent backups per addon. Set to 0 to keep all backups."));
                    ui.add_space(10.0);
                    let retention_response = ui.add(
                        egui::DragValue::new(
                            &mut self.settings_view_state.backup_keep_latest_per_addon,
                        )
                        .range(0..=100),
                    );
                    if retention_response.changed() {
                        changed = true;
                    }
                    if retention_response.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    ui.add_space(10.0);
                    ui.label(tr("0 = unlimited"));
                });

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    let mut max_age_enabled = self.settings_view_state.backup_max_age_days.is_some();
                    let max_age_checkbox = Self::ui_state_checkbox(
                        ui,
                        &mut max_age_enabled,
                        tr("Delete backups older than"),
                    ).on_hover_text(tr("Enable to delete backups older than the specified number of days when cleanup runs."));
                    if max_age_checkbox.changed() {
                        self.settings_view_state.backup_max_age_days = if max_age_enabled {
                            Some(30)
                        } else {
                            None
                        };
                        changed = true;
                    }
                    if max_age_checkbox.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }

                    if max_age_enabled {
                        let max_age_days =
                            self.settings_view_state.backup_max_age_days.get_or_insert(30);
                        if *max_age_days == 0 {
                            *max_age_days = 30;
                        }
                        let days_response = ui.add(
                            egui::DragValue::new(max_age_days)
                                .range(1..=3650)
                                .suffix(format!(" {}", tr("days"))),
                        );
                        if days_response.changed() {
                            changed = true;
                        }
                        if days_response.hovered() {
                            ui.ctx()
                                .output_mut(Foxy::set_pointing_cursor_output);
                        }
                    }
                });
                render_wrapped_info_row(
                    ui,
                    horizontal_padding,
                    RichText::new(tr(
                        "Cleanup rules apply when you click Run cleanup now. Set both rules to keep a tighter backup history.",
                    ))
                    .italics()
                    .color(self.color_text_dim()),
                );
                ui.separator();

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(tr("Filter:"));
                    ui.add_space(horizontal_padding);
                    let text_edit_width = (ui.available_width() - 2.0 * horizontal_padding).max(0.0);
                    let filter_edit = ui.add(
                        TextEdit::singleline(&mut self.backup_manager_filter)
                            .desired_width(text_edit_width),
                    );
                    if filter_edit.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                });
                ui.separator();

                if grouped_backups.is_empty() {
                    render_wrapped_info_row(
                        ui,
                        horizontal_padding,
                        RichText::new(if total_backups == 0 {
                            tr("No addon backups found.")
                        } else {
                            tr("No addon backups match the current filter.")
                        })
                        .italics()
                        .color(self.color_text_dim()),
                    );
                } else {
                    for backups in grouped_backups.values_mut() {
                        backups.sort_by(|a, b| {
                            b.created_at_unix_secs
                                .cmp(&a.created_at_unix_secs)
                                .then_with(|| locale_compare(&a.folder_name, &b.folder_name))
                        });
                    }

                    for backups in grouped_backups.values() {
                        let addon_name = backups
                            .first()
                            .map(|record| record.addon_name.clone())
                            .unwrap_or_default();
                        let addon_total_size: u64 =
                            backups.iter().map(|record| record.size_bytes).sum();

                        ui.horizontal(|ui| {
                            ui.add_space(horizontal_padding);
                            ui.label(
                                RichText::new(self.t_fmt(
                                    "{name} ({count} backups, {size})",
                                    &[
                                        ("name", addon_name.clone()),
                                        ("count", backups.len().to_string()),
                                        ("size", Self::format_bytes_short(addon_total_size)),
                                    ],
                                ))
                                .strong(),
                            );

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let delete_all_button = ui.button(tr("Delete all backups"))
                                    .on_hover_text(tr("Delete all stored backups for this addon."));
                                if delete_all_button.hovered() {
                                    ui.ctx()
                                        .output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if delete_all_button.clicked() {
                                    self.backup_manager_confirm_action =
                                        Some(BackupManagerConfirmAction::DeleteAddonGroup {
                                            addon_name: addon_name.clone(),
                                            backup_count: backups.len(),
                                        });
                                }
                            });
                        });

                        for backup in backups {
                            ui.add_space(4.0);
                            Frame::NONE
                                .fill(self.color_main_bg())
                                .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                                .corner_radius(eframe::egui::CornerRadius::same(5))
                                .inner_margin(Margin::same(8))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new(backup.folder_name.clone())
                                                    .color(self.color_text_normal()),
                                            );
                                            ui.label(
                                                RichText::new(self.t_fmt(
                                                    "Created {date} - Hash {hash} - Size {size}",
                                                    &[
                                                        (
                                                            "date",
                                                            fmt_date(
                                                                backup.created_at_unix_secs,
                                                            ),
                                                        ),
                                                        ("hash", backup.content_hash.clone()),
                                                        (
                                                            "size",
                                                            Self::format_bytes_short(
                                                                backup.size_bytes,
                                                            ),
                                                        ),
                                                    ],
                                                ))
                                                .color(self.color_text_dim()),
                                            );
                                        });

                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            let delete_button = ui.button(tr("Delete backup"))
                                                .on_hover_text(tr("Delete this specific backup."));
                                            if delete_button.hovered() {
                                                ui.ctx().output_mut(|o| {
                                                    Foxy::set_pointing_cursor_output(o)
                                                });
                                            }
                                            if delete_button.clicked() {
                                                self.backup_manager_confirm_action = Some(
                                                    BackupManagerConfirmAction::DeleteBackup(
                                                        backup.clone(),
                                                    ),
                                                );
                                            }

                                            let open_button = ui.button(tr("Open folder"))
                                                .on_hover_text(tr("Open this backup's folder in Explorer."));
                                            if open_button.hovered() {
                                                ui.ctx().output_mut(|o| {
                                                    Foxy::set_pointing_cursor_output(o)
                                                });
                                            }
                                            if open_button.clicked() {
                                                open_folder_path = Some(backup.path.clone());
                                            }
                                        });
                                    });
                                });
                        }

                        ui.separator();
                    }
                }
            });
        });

        if changed {
            self.save_settings();
        }

        if let Some(path) = open_folder_path
            && !self.open_backup_path(&path)
        {
            self.show_error_toast(self.t("Failed to open backup folder."));
        }

        self.render_backup_manager_confirmation_modal(ui);
    }

    fn render_backup_manager_confirmation_modal(&mut self, ui: &mut Ui) {
        let Some(action) = self.backup_manager_confirm_action.clone() else {
            return;
        };

        let (title, message, yes_label) = match &action {
            BackupManagerConfirmAction::DeleteBackup(record) => (
                tr("Confirm backup deletion"),
                self.t_fmt(
                    "Delete backup {name} ({hash})?",
                    &[
                        ("name", record.addon_name.clone()),
                        ("hash", record.content_hash.clone()),
                    ],
                ),
                tr("Delete backup"),
            ),
            BackupManagerConfirmAction::DeleteAddonGroup {
                addon_name,
                backup_count,
            } => (
                tr("Confirm backup deletion"),
                self.i18n.tr_plural_fmt(
                    "Delete all {count} backups for {name}?",
                    *backup_count as u64,
                    &[("name", addon_name.clone())],
                ),
                tr("Delete all backups"),
            ),
            BackupManagerConfirmAction::RunCleanup => (
                tr("Confirm backup cleanup"),
                tr("Run backup cleanup using the configured retention rules?"),
                tr("Run cleanup now"),
            ),
        };

        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(eframe::egui::CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(480.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(message);
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        let yes_btn = ui.button(yes_label);
                        if yes_btn.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if yes_btn.clicked() {
                            self.backup_manager_confirm_action = None;
                            match action {
                                BackupManagerConfirmAction::DeleteBackup(record) => {
                                    self.delete_backup_manager_record(&record);
                                }
                                BackupManagerConfirmAction::DeleteAddonGroup {
                                    addon_name, ..
                                } => {
                                    self.delete_backup_manager_addon_group(&addon_name);
                                }
                                BackupManagerConfirmAction::RunCleanup => {
                                    self.run_backup_manager_cleanup();
                                }
                            }
                        }

                        let cancel_btn = ui.button(tr("Cancel"));
                        if cancel_btn.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if cancel_btn.clicked() {
                            self.backup_manager_confirm_action = None;
                        }
                    });
                });
            });
    }

    #[cfg(target_os = "windows")]
    pub(super) fn open_backup_path(&self, path: &Path) -> bool {
        if let Err(err) = std::fs::create_dir_all(path) {
            warn!("Failed to create backup folder: {}", err);
            return false;
        }

        if let Err(err) = std::process::Command::new("explorer")
            .arg(path.as_os_str())
            .spawn()
        {
            warn!("Failed to open backup folder: {}", err);
            return false;
        }
        true
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn open_backup_path(&self, path: &Path) -> bool {
        if let Err(err) = std::fs::create_dir_all(path) {
            warn!("Failed to create backup folder: {}", err);
            return false;
        }

        if let Err(err) = crate::core::utils::platform::open_with_default_app(path) {
            warn!("Failed to open backup folder: {}", err);
            return false;
        }
        true
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn open_log_folder(&self) -> bool {
        let log_dir = app_paths::foxy_logs_dir();

        if let Err(err) = std::process::Command::new("explorer")
            .arg(log_dir.as_os_str())
            .spawn()
        {
            warn!("Failed to open log folder: {}", err);
            return false;
        }
        true
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn open_log_folder(&self) -> bool {
        let log_dir = app_paths::foxy_logs_dir();

        if let Err(err) = crate::core::utils::platform::open_with_default_app(&log_dir) {
            warn!("Failed to open log folder: {}", err);
            return false;
        }
        true
    }

    #[cfg(target_os = "windows")]
    pub(super) fn open_config_directory(&self) -> bool {
        let config_dir = app_paths::foxy_data_dir();
        if let Err(err) = std::process::Command::new("explorer")
            .arg(config_dir.as_os_str())
            .spawn()
        {
            warn!("Failed to open config directory: {}", err);
            return false;
        }
        true
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn open_config_directory(&self) -> bool {
        let config_dir = app_paths::foxy_data_dir();
        if let Err(err) = crate::core::utils::platform::open_with_default_app(&config_dir) {
            warn!("Failed to open config directory: {}", err);
            return false;
        }
        true
    }

    /// Zip every file in the logs directory into a user-chosen location.
    /// The default filename includes the timestamp extracted from the last
    /// line of the most-recently-modified log file.
    /// Returns `Ok(true)` when a ZIP was written, `Ok(false)` when the user
    /// cancelled the save dialog, and `Err` on I/O or zip errors.
    pub(crate) fn export_logs_to_zip(&self) -> Result<bool, String> {
        use rfd::FileDialog;
        use std::fs;
        use std::io::{Read, Write};
        use zip::CompressionMethod;
        use zip::write::SimpleFileOptions;

        let logs_dir = app_paths::foxy_logs_dir();

        // Collect log files (non-recursive – logs sit directly in the folder).
        let mut entries: Vec<_> = fs::read_dir(&logs_dir)
            .map_err(|e| format!("Failed to read log directory: {e}"))?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("log"))
                {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        if entries.is_empty() {
            return Err("No log files found to export.".into());
        }

        // Sort by modification time (newest last) so we can grab the latest.
        entries.sort_by(|a, b| {
            let ma = a.metadata().and_then(|m| m.modified()).ok();
            let mb = b.metadata().and_then(|m| m.modified()).ok();
            ma.cmp(&mb)
        });

        // Build a filename timestamp from the last line of the newest log file.
        let zip_file_name = Self::zip_name_from_last_log_line(entries.last().unwrap())
            .unwrap_or_else(|| "foxy_logs.zip".to_string());

        // Ask the user where to save the ZIP.
        let dest = crate::ui::app::agent_support::save_file(|| {
            FileDialog::new()
                .set_file_name(&zip_file_name)
                .add_filter("ZIP archive", &["zip"])
                .save_file()
        });

        let dest = match dest {
            Some(p) => p,
            None => return Ok(false), // user cancelled
        };

        let file =
            fs::File::create(&dest).map_err(|e| format!("Failed to create ZIP file: {e}"))?;
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        zip.start_file("diagnostics_manifest.txt", options)
            .map_err(|e| format!("ZIP manifest error: {e}"))?;
        zip.write_all(self.build_diagnostics_manifest(&entries).as_bytes())
            .map_err(|e| format!("Failed to write ZIP manifest: {e}"))?;

        let mut buf = Vec::new();
        for path in &entries {
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            zip.start_file(&file_name, options)
                .map_err(|e| format!("ZIP error for {file_name}: {e}"))?;

            buf.clear();
            fs::File::open(path)
                .and_then(|mut f| f.read_to_end(&mut buf))
                .map_err(|e| format!("Failed to read {file_name}: {e}"))?;

            let redacted = String::from_utf8_lossy(&buf)
                .lines()
                .map(crate::core::utils::format::redact_log_text)
                .collect::<Vec<_>>()
                .join("\n");
            zip.write_all(redacted.as_bytes())
                .map_err(|e| format!("Failed to write {file_name} to ZIP: {e}"))?;
        }

        zip.finish()
            .map_err(|e| format!("Failed to finalize ZIP: {e}"))?;

        Ok(true)
    }

    fn build_diagnostics_manifest(&self, log_entries: &[std::path::PathBuf]) -> String {
        let process = crate::ui::memory::sample_process_memory();
        let memory_report = self.build_memory_diagnostics_report();
        let log_bytes = log_entries
            .iter()
            .map(|path| file_size(path).unwrap_or_default())
            .sum::<u64>();
        let database_dir = Self::get_config_directory();
        let database_db = database_dir.join("database.db");
        let database_wal = database_dir.join("database.db-wal");
        let database_shm = database_dir.join("database.db-shm");
        let database_db_size = file_size(&database_db);
        let database_wal_size = file_size(&database_wal);
        let database_shm_size = file_size(&database_shm);
        let database_total_size = database_db_size.unwrap_or_default()
            + database_wal_size.unwrap_or_default()
            + database_shm_size.unwrap_or_default();

        let mut manifest = String::new();
        manifest.push_str("Foxy diagnostics export\n");
        manifest.push_str(&format!(
            "generated_at={}\n",
            chrono::Local::now().to_rfc3339()
        ));
        manifest.push_str(&format!("version={}\n", env!("CARGO_PKG_VERSION")));
        manifest.push_str(&format!("retained_log_files={}\n", log_entries.len()));
        manifest.push_str(&format!(
            "retained_log_bytes={}\n",
            Self::format_bytes_short(log_bytes)
        ));

        manifest.push_str("\n[system]\n");
        for line in
            crate::core::api::startup_system_diagnostics_lines(&self.startup_storage_paths())
        {
            manifest.push_str(&line);
            manifest.push('\n');
        }

        manifest.push_str("\n[storage_devices]\n");
        for line in crate::core::api::all_storage_devices_lines() {
            manifest.push_str(&line);
            manifest.push('\n');
        }

        manifest.push_str("\n[process_memory]\n");
        manifest.push_str(&format!(
            "task_manager_memory={}\n",
            Self::format_optional_bytes(process.task_manager_memory_bytes())
        ));
        manifest.push_str(&format!(
            "working_set={}\n",
            Self::format_optional_bytes(process.working_set_bytes)
        ));
        manifest.push_str(&format!(
            "peak_working_set={}\n",
            Self::format_optional_bytes(process.peak_working_set_bytes)
        ));
        manifest.push_str(&format!(
            "private_bytes={}\n",
            Self::format_optional_bytes(process.private_bytes)
        ));
        manifest.push_str(&format!(
            "virtual_bytes={}\n",
            Self::format_optional_bytes(process.virtual_bytes)
        ));
        manifest.push_str(&format!(
            "page_faults={}\n",
            Self::format_optional_count(process.page_fault_count)
        ));
        manifest.push_str(&format!(
            "tracked_foxy_allocations={}\n",
            Self::format_bytes_short(memory_report.tracked_total_bytes as u64)
        ));
        manifest.push_str(&format!(
            "untracked_estimate={}\n",
            memory_report
                .untracked_bytes
                .map(Self::format_bytes_delta)
                .unwrap_or_else(|| "n/a".to_string())
        ));
        for bucket in memory_report.buckets.iter().take(8) {
            manifest.push_str(&format!(
                "tracked_bucket={} bytes={} detail={}\n",
                bucket.label,
                Self::format_bytes_short(bucket.bytes as u64),
                bucket.detail
            ));
        }

        manifest.push_str("\n[database]\n");
        manifest.push_str(&format!(
            "database_db={}\n",
            Self::format_optional_bytes(database_db_size)
        ));
        manifest.push_str(&format!(
            "database_wal={}\n",
            Self::format_optional_bytes(database_wal_size)
        ));
        manifest.push_str(&format!(
            "database_shm={}\n",
            Self::format_optional_bytes(database_shm_size)
        ));
        manifest.push_str(&format!(
            "database_total={}\n",
            Self::format_bytes_short(database_total_size)
        ));
        manifest.push_str(&format!(
            "settings_json={}\n",
            Self::format_optional_bytes(file_size(database_dir.join("settings.json")))
        ));
        manifest.push_str(&format!(
            "repositories_json={}\n",
            Self::format_optional_bytes(file_size(database_dir.join("repositories.json")))
        ));
        manifest.push_str(&format!(
            "repository_spaces_json={}\n",
            Self::format_optional_bytes(file_size(database_dir.join("repository_spaces.json")))
        ));

        manifest.push_str("\n[app_state]\n");
        manifest.push_str(&format!(
            "repositories={}\n",
            self.repository_view_state.repositories.len()
        ));
        manifest.push_str(&format!(
            "repository_spaces={}\n",
            self.repository_spaces.len()
        ));
        manifest.push_str(&format!(
            "activity_log_entries={}\n",
            self.activity_log_cache.len()
        ));
        manifest.push_str(&format!("progress_events={}\n", self.progress_events.len()));
        manifest.push_str(&format!(
            "memory_diagnostic_samples={}\n",
            self.memory_diagnostics_history.len()
        ));
        manifest.push_str(&format!(
            "cached_textures={}\n",
            self.tracked_texture_count()
        ));
        manifest.push_str(&format!(
            "current_sync_mode={}\n",
            self.current_sync_mode
                .map(|mode| format!("{mode:?}"))
                .unwrap_or_else(|| "none".to_string())
        ));
        manifest.push_str(&format!(
            "queued_startup_rechecks={}\n",
            self.startup_recheck_queue.len()
        ));
        manifest.push_str(&format!(
            "queued_quick_scans={}\n",
            self.pending_quick_scan_urls.len()
        ));

        manifest.push_str("\n[zip_contents]\n");
        for path in log_entries {
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            manifest.push_str(&format!(
                "log_file={} size={}\n",
                file_name,
                Self::format_optional_bytes(file_size(path))
            ));
        }

        manifest
    }

    /// Read the tail of a log file and extract a filesystem-safe timestamp
    /// from its last non-empty line.  Returns `Some("foxy_logs_2026-04-12_094106.zip")`.
    ///
    /// flexi_logger `detailed_format` lines look like:
    ///   `[2026-04-12 09:41:06.227525 +02:00] INFO …`
    /// We extract the date and time from inside the brackets.
    fn zip_name_from_last_log_line(path: &std::path::Path) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};

        let mut file = std::fs::File::open(path).ok()?;
        let file_len = file.metadata().ok()?.len();

        // Read the last 4 KiB – more than enough to contain the final line.
        let start = file_len.saturating_sub(4096);
        file.seek(SeekFrom::Start(start)).ok()?;

        let mut tail = String::new();
        file.read_to_string(&mut tail).ok()?;

        let last_line = tail.lines().rev().find(|l| !l.trim().is_empty())?;

        // Extract content between the first '[' and ']'.
        let after_bracket = last_line.strip_prefix('[')?;
        let bracket_content = after_bracket.split(']').next()?;

        // bracket_content = "2026-04-12 09:41:06.227525 +02:00"
        // Take date (10 chars) and time (8 chars) separated by space.
        let date = bracket_content.get(..10)?;
        let time = bracket_content.get(11..19)?;

        // "2026-04-12" + "09:41:06" → "foxy_logs_2026-04-12_094106.zip"
        let safe_time: String = time.replace(':', "");
        Some(format!("foxy_logs_{date}_{safe_time}.zip"))
    }
}

fn file_size(path: impl AsRef<std::path::Path>) -> Option<u64> {
    path.as_ref().metadata().ok().map(|metadata| metadata.len())
}
