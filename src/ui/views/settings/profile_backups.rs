use crate::core::profile_backups::{self, BackupEntry};
use crate::ui::app::Foxy;
use crate::ui::app::profile_backups::{MutationResult, ProfileBackupConfirm};
use crate::ui::i18n::tr;
use eframe::egui::{self, Button, Ui};

impl Foxy {
    pub(crate) fn render_profile_backups(&mut self, ui: &mut Ui, padding: f32) {
        let active = self.editing_active_game_space();
        let mut policy = self.edited_game_space_settings().profile_backups.clone();
        ui.add_space(padding);
        ui.heading(tr("Profile backups"));
        ui.label(tr("Keeps the latest 3 backups per profile. Daily backups run when Foxy is open. Missions and saves are not included."));
        let mut changed = ui
            .checkbox(&mut policy.enabled, tr("Enable automatic profile backups"))
            .changed();
        ui.horizontal(|ui| {
            let custom_days = policy.interval_days.max(1);
            let selected = if policy.interval_days == 0 {
                tr("Each app start")
            } else {
                tr("Interval (days)")
            };
            egui::ComboBox::from_id_salt("profile_backup_interval")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut policy.interval_days, 0, tr("Each app start"))
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut policy.interval_days,
                            custom_days,
                            tr("Interval (days)"),
                        )
                        .changed();
                });
            if policy.interval_days >= 1 {
                changed |= ui
                    .add_enabled(
                        policy.enabled,
                        egui::DragValue::new(&mut policy.interval_days).range(1..=365),
                    )
                    .changed();
            }
        });
        if changed {
            self.edited_game_space_settings_mut().profile_backups = policy;
            self.save_edited_game_space_settings();
        }
        if active {
            self.request_profile_backup_list(ui.ctx());
            let busy = self.profile_backup_state.busy;
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!busy, Button::new(tr("Back up now")))
                    .clicked()
                {
                    let custom = self
                        .settings_view_state
                        .arma3_profiles_directory
                        .trim()
                        .to_owned();
                    let ctx = ui.ctx().clone();
                    self.start_profile_backup_mutation(&ctx, move |root| {
                        if crate::core::arma3_profiles::is_arma3_running() {
                            return Err("Arma 3 is currently running. Close the game before managing profiles.".to_owned());
                        }
                        let custom = (!custom.is_empty()).then(|| std::path::PathBuf::from(custom));
                        let profiles =
                            crate::core::arma3_profiles::detect_all_profiles(custom.as_deref());
                        profile_backups::create_backups(&root, &profiles, chrono::Local::now())
                            .map(MutationResult::Entries)
                            .map_err(|_| "Profile backup operation failed.".to_owned())
                    });
                }
                if ui.add_enabled(!busy, Button::new(tr("Refresh"))).clicked() {
                    self.profile_backup_state.list_loaded_for = None;
                    self.request_profile_backup_list(ui.ctx());
                }
                if ui
                    .add_enabled(!busy, Button::new(tr("Open backup folder")))
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    self.start_profile_backup_mutation(&ctx, move |root| {
                        open_backup_folder(&root)
                            .map(|()| MutationResult::Done)
                            .map_err(|_| "Profile backup operation failed.".to_owned())
                    });
                }
                if ui
                    .add_enabled(
                        !busy,
                        Button::new(tr("Delete all backups")),
                    )
                    .clicked()
                {
                    self.profile_backup_state.confirm = Some(ProfileBackupConfirm::DeleteAll);
                    self.profile_backup_state.confirm_initial_focus = true;
                }
            });
            if busy {
                ui.label(tr("Working..."));
            }
            if let Some(status) = &self.profile_backup_state.status {
                ui.label(tr(status));
            }
            if self.profile_backup_state.entries.is_empty() {
                ui.label(tr("No profile backups found."));
            } else {
                let entries = self.profile_backup_state.entries.clone();
                for entry in &entries {
                    self.render_profile_backup_row(ui, entry, busy);
                }
            }
        } else {
            ui.label(tr("Open this game space to manage Arma 3 profiles."));
        }
        self.render_profile_backup_confirmation(ui);
    }

    fn render_profile_backup_row(&mut self, ui: &mut Ui, entry: &BackupEntry, busy: bool) {
        ui.push_id(("profile_backup", entry.id.as_str()), |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&entry.profile_name);
                    ui.label(crate::ui::i18n::fmt_date(entry.created_at.max(0) as u64));
                    ui.label(format!("{} {}", entry.file_count, tr("Files")));
                    ui.label(crate::ui::i18n::fmt_bytes(entry.bytes));
                });
                ui.label(entry.original_path.display().to_string());
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!busy, Button::new(tr("Restore"))).clicked() {
                        self.profile_backup_state.confirm =
                            Some(ProfileBackupConfirm::Restore(entry.id.clone()));
                        self.profile_backup_state.confirm_initial_focus = true;
                    }
                    if ui
                        .add_enabled(!busy, Button::new(tr("Export ZIP")))
                        .clicked()
                    {
                        let suggested = format!("{}.zip", entry.profile_name);
                        if let Some(destination) = crate::ui::app::agent_support::save_file(|| {
                            rfd::FileDialog::new()
                                .set_file_name(&suggested)
                                .add_filter("ZIP", &["zip"])
                                .save_file()
                        }) {
                            let id = entry.id.clone();
                            let ctx = ui.ctx().clone();
                            self.start_profile_backup_mutation(&ctx, move |root| {
                                profile_backups::export_backup(&root, &id, &destination)
                                    .map(|()| MutationResult::Done)
                                    .map_err(|_| "Profile backup operation failed.".to_owned())
                            });
                        }
                    }
                    if ui
                        .add_enabled(!busy, Button::new(tr("Open backup folder")))
                        .clicked()
                    {
                        let folder = entry.directory.clone();
                        let ctx = ui.ctx().clone();
                        self.start_profile_backup_mutation(&ctx, move |_| {
                            open_backup_folder(&folder)
                                .map(|()| MutationResult::Done)
                                .map_err(|_| "Profile backup operation failed.".to_owned())
                        });
                    }
                });
            });
        });
    }

    fn render_profile_backup_confirmation(&mut self, ui: &mut Ui) {
        let Some(action) = self.profile_backup_state.confirm.clone() else {
            return;
        };
        let (title, body) = match action {
            ProfileBackupConfirm::Restore(_) => (
                tr("Confirm profile restore"),
                tr("This will replace the current profile files with this backup."),
            ),
            ProfileBackupConfirm::DeleteAll => (
                tr("Confirm delete all profile backups"),
                tr("This permanently deletes all profile backups."),
            ),
        };
        let mut close = false;
        let mut confirm = false;
        let response = egui::Modal::new(egui::Id::unique("profile_backup_confirmation")).show(
            ui.ctx(),
            |ui| {
                ui.set_max_width(480.0);
                ui.heading(title);
                if let ProfileBackupConfirm::Restore(id) = &action
                    && let Some(entry) = self
                        .profile_backup_state
                        .entries
                        .iter()
                        .find(|entry| &entry.id == id)
                {
                    ui.strong(&entry.profile_name);
                    ui.label(crate::ui::i18n::fmt_date(entry.created_at.max(0) as u64));
                    ui.label(entry.original_path.display().to_string());
                }
                ui.label(body);
                ui.horizontal(|ui| {
                    let cancel = ui.button(tr("Cancel"));
                    if self.profile_backup_state.confirm_initial_focus {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        close = true;
                    }
                    if ui.button(tr("Confirm")).clicked() {
                        confirm = true;
                        close = true;
                    }
                });
                self.profile_backup_state.confirm_initial_focus = false;
            },
        );
        if close || response.should_close() {
            self.profile_backup_state.confirm = None;
        }
        if confirm {
            match action {
                ProfileBackupConfirm::Restore(id) => {
                    let custom = self
                        .settings_view_state
                        .arma3_profiles_directory
                        .trim()
                        .to_owned();
                    let ctx = ui.ctx().clone();
                    self.start_profile_backup_mutation(&ctx, move |root| {
                        if crate::core::arma3_profiles::is_arma3_running() {
                            return Err("Arma 3 is currently running. Close the game before managing profiles.".to_owned());
                        }
                        let custom = (!custom.is_empty()).then(|| std::path::PathBuf::from(custom));
                        let roots = profile_backups::restore_roots(custom.as_deref());
                        profile_backups::restore_backup(&root, &id, &roots)
                            .map(|_| MutationResult::Restored(crate::core::arma3_profiles::detect_all_profiles(custom.as_deref())))
                            .map_err(|_| "Profile backup operation failed.".to_owned())
                    });
                }
                ProfileBackupConfirm::DeleteAll => {
                    let ctx = ui.ctx().clone();
                    self.start_profile_backup_mutation(&ctx, move |root| {
                        profile_backups::delete_all_backups(&root)
                            .map(|()| MutationResult::Done)
                            .map_err(|_| "Profile backup operation failed.".to_owned())
                    })
                }
            }
        }
    }
}

fn open_backup_folder(path: &std::path::Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(path)?;
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command.arg(path).spawn().map(|_| ())
}
