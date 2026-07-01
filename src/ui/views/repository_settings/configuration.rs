use super::should_wipe_cache_after_local_path_change;
use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::types::FoxyView;
use eframe::egui::{self, Align2, CornerRadius, Frame, Layout, Margin, Ui};
use log::{info, warn};

impl Foxy {
    pub(super) fn render_repository_configuration(&mut self, ui: &mut Ui) {
        let repo_index = match self.selected_repository_for_settings {
            Some(i) if i < self.repository_view_state.repositories.len() => i,
            _ => {
                self.current_view = FoxyView::RepositoryList;
                return;
            }
        };

        let (
            original_repo_address,
            original_repo_name,
            original_repo_path_raw,
            original_repository_space_id,
        ) = {
            let repo = &self.repository_view_state.repositories[repo_index];
            (
                repo.address.clone(),
                repo.name.clone(),
                repo.path.clone(),
                repo.repository_space_id.clone(),
            )
        };

        let inherited_space = original_repository_space_id
            .as_deref()
            .and_then(|space_id| {
                self.repository_spaces
                    .iter()
                    .find(|space| space.id == space_id)
                    .map(|space| {
                        (
                            Self::repository_space_display_name(space).to_string(),
                            space.shared_path.clone(),
                        )
                    })
            });

        let color_primary_accent = self.color_primary_accent();
        let color_text_error = self.color_text_error();
        let color_text_dim = self.color_text_dim();
        let color_card_bg = self.color_card_bg();
        let color_text_normal = self.color_text_normal();

        let mut delete_repository = false;
        let mut force_redownload = false;
        let mut wipe_repository_db_entries = false;
        let mut recheck_repository_integrity = false;
        let mut changed = false;
        let mut addr_changed = false;
        let pad_f32 = 15.0;

        Frame::NONE.inner_margin(Margin::same(15)).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let info = format!(
                        "{} {}",
                        '\u{2139}',
                        tr("Here you configure basic repository info, parameters and delete the repository")
                    );
                    ui.label(egui::RichText::new(info).italics().color(color_text_dim));
                });
                ui.separator();

                {
                    let repo = &mut self.repository_view_state.repositories[repo_index];
                    Self::render_repository_configuration_identity(
                        ui,
                        repo,
                        &inherited_space,
                        color_text_dim,
                        pad_f32,
                        &mut changed,
                        &mut addr_changed,
                    );
                }
                self.render_repository_configuration_profiles(
                    ui,
                    repo_index,
                    pad_f32,
                    &mut changed,
                );

                self.render_repository_configuration_sync(
                    ui,
                    repo_index,
                    color_text_dim,
                    &mut changed,
                );

                self.render_repository_configuration_actions(
                    ui,
                    repo_index,
                    color_primary_accent,
                    color_text_error,
                    pad_f32,
                    &mut force_redownload,
                    &mut wipe_repository_db_entries,
                    &mut recheck_repository_integrity,
                );
            });
        });

        let repository_name_for_dialogs = self.repository_view_state.repositories[repo_index]
            .name
            .clone();
        if self.show_delete_confirmation {
            egui::Window::new(tr("Confirm Deletion"))
                .frame(
                    egui::Frame::window(&ui.ctx().global_style())
                        .fill(color_card_bg)
                        .stroke(egui::Stroke::new(1.0, color_text_normal))
                        .corner_radius(CornerRadius::same(10)),
                )
                .title_bar(true)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(500.0)
                .default_height(250.0)
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(tr_fmt(
                            "Are you sure you want to delete {name}?",
                            &[("name", repository_name_for_dialogs.clone())],
                        ));
                        ui.add_space(12.0);
                        let delete_files_checkbox = ui
                            .checkbox(&mut self.delete_repository_delete_files, tr("Delete files"));
                        if delete_files_checkbox.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if self.delete_repository_delete_files {
                            ui.label(tr(
                                "This will also delete downloaded files for this repository.",
                            ));
                        }
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                Layout::centered_and_justified(egui::Direction::TopDown),
                                |ui| {
                                    let yes_btn = ui.button(tr("Yes"));
                                    if yes_btn.hovered() {
                                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if yes_btn.clicked() {
                                        delete_repository = true;
                                        self.show_delete_confirmation = false;
                                        warn!("Repository delete confirmed");
                                    }

                                    let no_btn = ui.button(tr("No"));
                                    if no_btn.hovered() {
                                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if no_btn.clicked() {
                                        self.show_delete_confirmation = false;
                                        self.delete_repository_delete_files = false;
                                        info!("Repository delete canceled");
                                    }
                                },
                            );
                        });
                    });
                });
        }

        if self.show_force_redownload_confirmation {
            egui::Window::new(tr("Confirm Force Redownload"))
                .frame(egui::Frame::window(&ui.ctx().global_style())
                    .fill(color_card_bg)
                    .stroke(egui::Stroke::new(1.0, color_text_normal))
                    .corner_radius(CornerRadius::same(10)))
                .title_bar(true)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(520.0)
                .default_height(260.0)
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(tr_fmt(
                            "Force redownload {name}?\nThis will remove local files and re-download the repository.",
                            &[("name", repository_name_for_dialogs.clone())],
                        ));
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::centered_and_justified(egui::Direction::TopDown), |ui| {
                                let yes_btn = ui.button(tr("Yes"));
                                if yes_btn.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if yes_btn.clicked() {
                                    force_redownload = true;
                                    self.show_force_redownload_confirmation = false;
                                    warn!("Force redownload confirmed");
                                }

                                let no_btn = ui.button(tr("No"));
                                if no_btn.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if no_btn.clicked() {
                                    self.show_force_redownload_confirmation = false;
                                    info!("Force redownload canceled");
                                }
                            });
                        });
                    });
                });
        }

        if self.show_wipe_repo_db_confirmation {
            egui::Window::new(tr("Confirm Repository Database Wipe"))
                .frame(
                    egui::Frame::window(&ui.ctx().global_style())
                        .fill(color_card_bg)
                        .stroke(egui::Stroke::new(1.0, color_text_normal))
                        .corner_radius(CornerRadius::same(10)),
                )
                .title_bar(true)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(520.0)
                .default_height(260.0)
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(tr_fmt(
                            "Are you sure you want to wipe database entries for {name}?",
                            &[("name", repository_name_for_dialogs.clone())],
                        ));
                        ui.label(tr("This only clears cached metadata for this repository."));
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                Layout::centered_and_justified(egui::Direction::TopDown),
                                |ui| {
                                    let yes_btn = ui.button(tr("Yes, Wipe Repository Database"));
                                    if yes_btn.hovered() {
                                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if yes_btn.clicked() {
                                        wipe_repository_db_entries = true;
                                        self.show_wipe_repo_db_confirmation = false;
                                        warn!("Repository database wipe confirmed");
                                    }

                                    let no_btn = ui.button(tr("No"));
                                    if no_btn.hovered() {
                                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if no_btn.clicked() {
                                        self.show_wipe_repo_db_confirmation = false;
                                        info!("Repository database wipe canceled");
                                    }
                                },
                            );
                        });
                    });
                });
        }

        if delete_repository {
            info!("Deleting repository from settings view");
            let delete_local_files = self.delete_repository_delete_files;
            self.delete_repository_delete_files = false;
            self.delete_repository_by_index(repo_index, delete_local_files);
            self.selected_repository_for_settings = None;
            self.current_view = FoxyView::RepositoryList;
            self.last_view = FoxyView::None;
        } else {
            let should_wipe_cache = should_wipe_cache_after_local_path_change(
                &original_repo_path_raw,
                &self.repository_view_state.repositories[repo_index].path,
            );
            if changed {
                self.save_repositories();
            }
            if should_wipe_cache {
                info!(
                    "Repository local path changed; wiping cached database entries for {}",
                    original_repo_name
                );
                // Wipe only the cached rows for the repository's *previous* folder.
                // Keying the purge by URL alone would also destroy a sibling
                // repository that shares this URL under a different folder (e.g. the
                // same repo installed in a repository space), corrupting its hashes.
                self.wipe_repository_database_entries_by_url_and_path(
                    &original_repo_address,
                    &original_repo_path_raw,
                    &original_repo_name,
                );
            }
            if addr_changed {
                self.update_repository_from_url(repo_index, ui.ctx());
            }
            if force_redownload {
                info!("Triggering repository force redownload");
                self.force_redownload_repository(repo_index);
            }
            if wipe_repository_db_entries {
                info!("Wiping repository database entries from settings view");
                self.wipe_repository_database_entries(repo_index);
            }
            if recheck_repository_integrity {
                info!("Triggering full repository integrity recheck");
                self.start_core_sync(repo_index, SyncMode::RecheckIntegrity);
            }
        }
    }
}
