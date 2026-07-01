use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use eframe::egui::{self, Button, RichText, Ui, Vec2};
use log::info;

use super::render_wrapped_info_row;

impl Foxy {
    /// App update settings section: update source selector, URL/repo fields, auto-check, and
    /// check-now / browse-all-versions buttons.
    pub(super) fn render_application_settings_updates(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        changed: &mut bool,
    ) {
        // --- App Updates section ---
        ui.separator();
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(RichText::new(tr("App Updates")).strong());
        });

        // Update mode selector
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Update source"));
            ui.add_space(8.0);
            let server_selected = ui.radio(
                self.settings_view_state.app_update_mode == crate::ui::types::AppUpdateMode::Server,
                tr("Server"),
            );
            if server_selected.clicked() {
                self.settings_view_state.app_update_mode = crate::ui::types::AppUpdateMode::Server;
                self.settings_view_state.app_update_mode_user_override = true;
                *changed = true;
            }
            let github_selected = ui.radio(
                self.settings_view_state.app_update_mode == crate::ui::types::AppUpdateMode::GitHub,
                tr("GitHub"),
            );
            if github_selected.clicked() {
                self.settings_view_state.app_update_mode = crate::ui::types::AppUpdateMode::GitHub;
                self.settings_view_state.app_update_mode_user_override = true;
                *changed = true;
            }
        });

        match self.settings_view_state.app_update_mode {
            crate::ui::types::AppUpdateMode::Server => {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(tr("Update source URL"));
                });
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    let text_edit_width = ui.available_width() - 2.0 * horizontal_padding;
                    let url_response = ui.add_sized(
                        Vec2::new(text_edit_width, 24.0),
                        egui::TextEdit::singleline(&mut self.settings_view_state.app_update_url)
                            .hint_text("http://myserver.com/foxy/"),
                    );
                    if url_response.changed() {
                        self.settings_view_state.app_update_url_user_override =
                            !self.settings_view_state.app_update_url.trim().is_empty();
                        *changed = true;
                    }
                    ui.add_space(horizontal_padding);
                });
                render_wrapped_info_row(
                    ui,
                    horizontal_padding,
                    RichText::new(tr(
                        "If this field is empty, Foxy can auto-detect the update source from repository-space metadata first, then repository metadata (appUpdateUrl).",
                    ))
                    .italics()
                        .color(self.color_text_dim()),
                );
            }
            crate::ui::types::AppUpdateMode::GitHub => {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(tr("GitHub repository"));
                });
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    let text_edit_width = ui.available_width() - 2.0 * horizontal_padding;
                    let repo_response = ui.add_sized(
                        Vec2::new(text_edit_width, 24.0),
                        egui::TextEdit::singleline(
                            &mut self.settings_view_state.app_update_github_repo,
                        )
                        .hint_text("owner/repo"),
                    );
                    if repo_response.changed() {
                        *changed = true;
                    }
                    ui.add_space(horizontal_padding);
                });
                render_wrapped_info_row(
                    ui,
                    horizontal_padding,
                    RichText::new(tr(
                        "Enter a public GitHub repository (e.g. owner/repo). Foxy will check GitHub Releases for updates.",
                    ))
                    .italics()
                        .color(self.color_text_dim()),
                );
            }
        };
        let update_source_configured = self.app_update_source_configured();

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let auto_check_response = ui.checkbox(
                &mut self.settings_view_state.app_update_auto_check,
                tr("Auto-check for updates on launch"),
            );
            if auto_check_response.changed() {
                *changed = true;
            }
        });

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);

            let check_now_btn =
                ui.add_enabled(update_source_configured, Button::new(tr("Check Now")));
            if check_now_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if check_now_btn.clicked() {
                self.start_update_check();
            }

            ui.add_space(8.0);

            let browse_versions_btn = ui.add_enabled(
                update_source_configured,
                Button::new(tr("Browse All Versions")),
            );
            if browse_versions_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if browse_versions_btn.clicked() {
                if matches!(
                    self.app_update_status,
                    crate::core::tasks::app_update::UpdateCheckStatus::Idle
                ) {
                    self.start_update_check();
                }
                self.open_reference_view(crate::ui::types::FoxyView::VersionBrowser);
            }

            ui.add_space(horizontal_padding);
        });

        // Show update check status
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            match &self.app_update_status {
                crate::core::tasks::app_update::UpdateCheckStatus::Checking => {
                    ui.label(tr("Checking..."));
                    ui.spinner();
                }
                crate::core::tasks::app_update::UpdateCheckStatus::UpToDate(_) => {
                    ui.label(RichText::new(tr("Up to date")).color(self.color_success()));
                }
                crate::core::tasks::app_update::UpdateCheckStatus::Available(info) => {
                    ui.label(
                        RichText::new(format!(
                            "{}: v{}",
                            tr("Update available"),
                            info.manifest.latest
                        ))
                        .color(self.color_primary_accent()),
                    );
                }
                crate::core::tasks::app_update::UpdateCheckStatus::Failed(msg) => {
                    ui.label(
                        RichText::new(format!("{}: {}", tr("Error"), msg))
                            .color(self.color_text_error()),
                    );
                }
                _ => {}
            }
        });
    }

    /// Renders the wipe-database confirmation dialog. Returns `true` if the dialog is currently
    /// shown so callers can skip other interactions if needed.
    pub(super) fn render_application_settings_wipe_db_confirmation(&mut self, ui: &mut Ui) {
        if self.show_wipe_db_confirmation {
            egui::Window::new(tr("Confirm Wipe Database"))
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
                .default_width(500.0)
                .default_height(250.0)
                .show(ui.ctx(), |ui| {
                    let sync_active = self.repository_sync_active();
                    ui.vertical_centered(|ui| {
                        ui.label(tr("Are you sure you want to completely wipe the database?"));
                        ui.label(tr("This will clear all cached repository data."));
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                                |ui| {
                                    let yes_btn = ui.add_enabled(
                                        !sync_active,
                                        egui::Button::new(tr("Yes, Wipe Database")),
                                    );
                                    if yes_btn.hovered() && !sync_active {
                                        ui.ctx()
                                            .output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if yes_btn.clicked() {
                                        self.show_wipe_db_confirmation = false;
                                        log::warn!("Database wipe confirmed");
                                        // Run the database wipe on a background thread
                                        // to avoid blocking the UI draw loop.
                                        std::thread::spawn(|| {
                                            match tokio::runtime::Runtime::new() {
                                                Ok(rt) => {
                                                    if let Err(e) = rt.block_on(
                                                        crate::core::tasks::init_database::wipe_database_live(),
                                                    ) {
                                                        log::error!("Failed to wipe database: {}", e);
                                                    } else {
                                                        info!("Database wipe completed");
                                                    }
                                                }
                                                Err(e) => {
                                                    log::error!("Failed to create runtime for database wipe: {}", e);
                                                }
                                            }
                                        });
                                        // Clear in-memory state
                                        self.clear_mod_diff_cache();
                                        self.repo_states.clear();
                                        self.update_ready_repo = None;
                                    }

                                    let no_btn = ui.button(tr("Cancel"));
                                    if no_btn.hovered() {
                                        ui.ctx()
                                            .output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if no_btn.clicked() {
                                        self.show_wipe_db_confirmation = false;
                                        info!("Database wipe canceled");
                                    }
                                },
                            );
                        });
                    });
                });
        }
    }
}
