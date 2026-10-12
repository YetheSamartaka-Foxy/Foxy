use eframe::egui::{self, Label, RichText};
use log::{error, info, warn};

use crate::ui::app::debug_modals::DebugModal;
use crate::ui::app::{Foxy, FoxyView};

impl Foxy {
    pub(super) fn render_db_schema_wipe_prompt(&mut self, ctx: &egui::Context) {
        let preview = self.previewing_debug_modal(DebugModal::DbSchemaWipe);
        self.refresh_live_database_repair_requirement();
        if !self.database_repair_prompt_visible() {
            return;
        }
        let Some(prompt) = self.pending_db_schema_wipe else {
            return;
        };

        // A wipe must not race an in-flight sync (it drops the tables the sync
        // is writing). Mirror the settings wipe dialog's guard.
        let sync_active = self.repository_sync_active();
        let mut wipe_clicked = false;
        let mut dismiss_clicked = false;
        let mut cancel_clicked = false;
        let mut recheck_all = self.db_schema_wipe_recheck_all;
        let window_frame = self.danger_modal_chrome(ctx);

        egui::Window::new(self.danger_modal_title(self.t("Database update required")))
            .frame(window_frame)
            .title_frame(window_frame)
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .min_width(0.0)
            .max_width((ctx.content_rect().width() - 40.0).clamp(1.0, 540.0))
            .default_width((ctx.content_rect().width() - 40.0).clamp(1.0, 540.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("database_repair_body")
                    .max_height((ctx.content_rect().height() - 230.0).max(1.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                let message = self.t(
                    "The local database must be rebuilt before Foxy can check or download mod updates. You can keep using Foxy and launch or join games without wiping. This reminder will appear before each launch or join until the database is rebuilt.",
                );
                self.render_danger_callout(ui, message);
                ui.add_space(10.0);
                ui.add(
                    Label::new(
                        RichText::new(self.t(
                            "Wiping clears cached repository data only - your downloaded mods and files on disk are not touched. Foxy rebuilds the cache automatically the next time it checks each repository.",
                        ))
                        .color(self.color_text_gray()),
                    )
                    .wrap(),
                );
                ui.add_space(12.0);
                ui.add_enabled_ui(!self.database_wipe_in_flight, |ui| {
                    Self::ui_state_checkbox(ui, &mut recheck_all, self.t("Recheck all repositories"));
                });
                });
                ui.add_space(12.0);

                wipe_clicked = self
                    .add_repair_primary_button(
                        ui,
                        self.t("Wipe database and continue"),
                        !sync_active,
                    )
                    .clicked();

                ui.vertical_centered(|ui| {
                    if self.database_wipe_in_flight {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add(crate::ui::app::PacedSpinner::new());
                            ui.label(self.t("Operation in progress"));
                        });
                    } else if sync_active {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                self.t("Finish the current sync before wiping the database."),
                            )
                            .color(self.color_warn()),
                        );
                    }

                    if !self.database_wipe_in_flight {
                        ui.add_space(10.0);
                        dismiss_clicked = self
                            .add_secondary_prompt_button(
                                ui,
                                self.t("Continue without wiping"),
                            )
                            .clicked();
                        if self.pending_database_launch.is_some() {
                            cancel_clicked = ui.button(self.t("Cancel")).clicked();
                        }
                    }
                });
            });
        self.db_schema_wipe_recheck_all = recheck_all;

        if preview {
            if wipe_clicked || dismiss_clicked || cancel_clicked {
                info!("Debug modal preview: closing database wipe prompt without acting");
                self.pending_db_schema_wipe = None;
                self.show_db_schema_wipe_prompt = false;
            }
            return;
        }

        if wipe_clicked {
            warn!(
                "Database schema wipe confirmed (stored={} target={} recheck_all={})",
                prompt.stored_version, prompt.target_version, self.db_schema_wipe_recheck_all
            );
            self.database_wipe_in_flight = true;
            self.recheck_all_after_database_wipe = self.db_schema_wipe_recheck_all;
            // Wipe on a background thread so the UI draw loop is never blocked.
            let database_wipe_tx = self.database_wipe_tx.clone();
            std::thread::spawn(move || {
                let result = match tokio::runtime::Runtime::new() {
                    Ok(rt) => {
                        match rt.block_on(crate::core::tasks::init_database::wipe_database_live()) {
                            Ok(()) => {
                                info!("Database schema wipe completed");
                                Ok(())
                            }
                            Err(e) => {
                                error!("Failed to wipe database for schema upgrade: {}", e);
                                Err(e)
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to create runtime for schema wipe: {}", e);
                        Err(e.to_string())
                    }
                };
                let _ = database_wipe_tx.send(result);
            });
            // Clear in-memory caches that mirror the now-empty database.
            self.clear_mod_diff_cache();
            self.repo_states.clear();
            self.update_ready_repo = None;
        } else if dismiss_clicked {
            self.show_db_schema_wipe_prompt = false;
            self.resume_database_launch(ctx);
        } else if cancel_clicked {
            self.show_db_schema_wipe_prompt = false;
            self.pending_database_launch = None;
        }
    }

    /// Startup prompt shown when the launch update check found a newer Foxy
    /// release. Dismissal is deliberately session-only, so the prompt returns on
    /// every launch until the update is installed.
    pub(super) fn render_app_update_prompt(&mut self, ctx: &egui::Context) {
        if !self.pending_app_update_prompt {
            return;
        }
        let crate::core::tasks::app_update::UpdateCheckStatus::Available(info) =
            &self.app_update_status
        else {
            self.pending_app_update_prompt = false;
            return;
        };
        let current_version = info.current_version.clone();
        let latest_version = info.manifest.latest.clone();

        let mut update_clicked = false;
        let mut later_clicked = false;
        let window_frame = self.danger_modal_chrome(ctx);

        egui::Window::new(self.danger_modal_title(self.t("Foxy update available")))
            .frame(window_frame)
            .title_frame(window_frame)
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(540.0)
            .show(ctx, |ui| {
                self.render_danger_callout(
                    ui,
                    self.t(
                        "A newer version of Foxy is available. Updating keeps you compatible with repository servers and brings the latest fixes.",
                    ),
                );
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(self.t_fmt(
                                "Installed version: v{version}",
                                &[("version", current_version.clone())],
                            ))
                            .color(self.color_error()),
                        );
                        ui.label("  ->  ");
                        ui.label(
                            RichText::new(self.t_fmt(
                                "New version: v{version}",
                                &[("version", latest_version.clone())],
                            ))
                            .color(self.color_text_normal())
                            .strong(),
                        );
                    });
                });
                ui.add_space(14.0);

                update_clicked = self
                    .add_danger_primary_button(ui, self.t("View update"), true)
                    .clicked();

                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    later_clicked = self
                        .add_secondary_prompt_button(ui, self.t("Remind me later"))
                        .clicked();
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            self.t(
                                "Foxy asks again on every launch until the update is installed.",
                            ),
                        )
                        .color(self.color_text_dim())
                        .italics(),
                    );
                });
            });

        if update_clicked {
            info!(
                "Opening app update view from launch update prompt (current={} latest={})",
                current_version, latest_version
            );
            self.pending_app_update_prompt = false;
            self.open_reference_view(FoxyView::AppUpdate);
        } else if later_clicked {
            info!(
                "App update prompt postponed for this session (current={} latest={})",
                current_version, latest_version
            );
            self.pending_app_update_prompt = false;
        }
    }
}
