use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use eframe::egui::{Button, Color32, TextEdit, Ui, Vec2};
use log::{info, warn};

impl Foxy {
    /// Creator DLC checkboxes, basic launch parameters, and additional parameters.
    pub(super) fn render_repository_configuration_profiles(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        pad_f32: f32,
        changed: &mut bool,
    ) {
        let repo = &self.repository_view_state.repositories[repo_index];
        let profile_selected = repo
            .selected_profile
            .as_ref()
            .is_some_and(|name| repo.profiles.iter().any(|profile| &profile.name == name));
        let launch_params_managed =
            !profile_selected && self.repo_apply_repo_json_client_parameters(repo);
        let managed_hint = tr(
            "Auto apply repo.json launch parameters is on, so the repository overwrites these fields on every refresh. Turn that setting off, or select a launch profile, to edit them.",
        );

        let repo = &mut self.repository_view_state.repositories[repo_index];
        let (
            csla,
            ef,
            gm,
            rf,
            spe,
            vn,
            ws,
            skip_intro,
            no_splash,
            world_empty,
            load_mission_to_memory,
            enable_ht,
            huge_pages,
            no_logs,
            additional_params,
        ) =
            match repo.selected_profile.as_ref().and_then(|selected_name| {
                repo.profiles.iter_mut().find(|p| &p.name == selected_name)
            }) {
                Some(profile) => (
                    &mut profile.csla,
                    &mut profile.ef,
                    &mut profile.gm,
                    &mut profile.rf,
                    &mut profile.spe,
                    &mut profile.vn,
                    &mut profile.ws,
                    &mut profile.skip_intro,
                    &mut profile.no_splash,
                    &mut profile.world_empty,
                    &mut profile.load_mission_to_memory,
                    &mut profile.enable_ht,
                    &mut profile.huge_pages,
                    &mut profile.no_logs,
                    &mut profile.additional_params,
                ),
                None => (
                    &mut repo.csla,
                    &mut repo.ef,
                    &mut repo.gm,
                    &mut repo.rf,
                    &mut repo.spe,
                    &mut repo.vn,
                    &mut repo.ws,
                    &mut repo.skip_intro,
                    &mut repo.no_splash,
                    &mut repo.world_empty,
                    &mut repo.load_mission_to_memory,
                    &mut repo.enable_ht,
                    &mut repo.huge_pages,
                    &mut repo.no_logs,
                    &mut repo.additional_params,
                ),
            };

        // Creator DLCs
        ui.horizontal(|ui| {
            ui.label(tr("Creator DLCs"));
        });
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(10.0, 6.0);
            ui.horizontal_wrapped(|ui| {
                for (flag, label) in &mut [
                    (csla, "\u{010C}SLA"),
                    (ef, "Expeditionary Forces"),
                    (gm, "Global Mobilization"),
                    (rf, "Reaction Forces"),
                    (spe, "Spearhead 1944"),
                    (vn, "S.O.G. PF"),
                    (ws, "Western Sahara"),
                ] {
                    let cb = Self::ui_state_checkbox(ui, flag, *label);
                    if cb.changed() {
                        *changed = true;
                    }
                    if cb.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                }
            });
        });
        ui.separator();

        // Basic Parameters
        ui.horizontal(|ui| {
            ui.label(tr("Basic Parameters"));
            if launch_params_managed {
                ui.weak(tr("(managed by repo.json)"))
                    .on_hover_text(managed_hint.as_str());
            }
        });
        ui.add_enabled_ui(!launch_params_managed, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(10.0, 6.0);
            ui.horizontal_wrapped(|ui| {
                for (flag, label, desc_key) in &mut [
                    (
                        skip_intro,
                        "-skipIntro",
                        "Skip world intros in the main menu for faster startup.",
                    ),
                    (no_splash, "-noSplash", "Bypass startup splash screens."),
                    (
                        world_empty,
                        "-world=empty",
                        "Load no default world in main menu to reduce startup load.",
                    ),
                    (
                        load_mission_to_memory,
                        "-loadMissionToMemory",
                        "Server: keep first-downloaded mission preloaded in RAM for next clients.",
                    ),
                    (
                        enable_ht,
                        "-enableHT",
                        "Allow Arma to use logical CPU cores (SMT/Hyper-Threading).",
                    ),
                    (
                        huge_pages,
                        "-hugePages",
                        "Enable huge pages with the default allocator (client and server).",
                    ),
                    (
                        no_logs,
                        "-noLogs",
                        "Disable RPT logging (crash fault block info is still saved).",
                    ),
                ] {
                    let cb = Self::ui_state_checkbox(ui, flag, *label).on_hover_text(tr(desc_key));
                    if cb.changed() {
                        *changed = true;
                    }
                    if cb.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                }
            });
        });
        ui.separator();

        // Additional Parameters
        ui.horizontal(|ui| {
            ui.label(tr("Additional Parameters"));
            if launch_params_managed {
                ui.weak(tr("(managed by repo.json)"))
                    .on_hover_text(managed_hint.as_str());
            }
        });
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            let r = ui
                .add_enabled(!launch_params_managed, TextEdit::singleline(additional_params).desired_width(w))
                .on_hover_text(tr("Extra CLI startup parameters. Separate multiple options with spaces and wrap paths with spaces in quotes."))
                .on_disabled_hover_text(managed_hint.as_str());
            if r.changed() { *changed = true; }
            if r.hovered() && !launch_params_managed {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
        });
        ui.separator();
    }

    /// Action buttons (recalculate hashes, force redownload, wipe DB entries, delete repository).
    #[expect(clippy::too_many_arguments)]
    pub(super) fn render_repository_configuration_actions(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        color_primary_accent: Color32,
        color_text_error: Color32,
        pad_f32: f32,
        _force_redownload: &mut bool,
        _wipe_repository_db_entries: &mut bool,
        recheck_repository_integrity: &mut bool,
    ) {
        let wipe_pending = {
            let repo = &self.repository_view_state.repositories[repo_index];
            self.is_repository_db_wipe_pending(&repo.address)
        };

        // Action buttons
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            let recheck_integrity_btn = ui.add_sized(
                Vec2::new(w, 30.0),
                Button::new(tr("Recheck repository integrity")).fill(color_primary_accent),
            );
            if recheck_integrity_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if recheck_integrity_btn.clicked() {
                *recheck_repository_integrity = true;
                info!("Manual repository integrity recheck requested");
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            let force_btn = ui.add_enabled(
                !wipe_pending,
                Button::new(tr("Force redownload repository"))
                    .fill(color_primary_accent)
                    .min_size(Vec2::new(w, 30.0)),
            );
            if force_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if force_btn.clicked() {
                self.show_force_redownload_confirmation = true;
                warn!("Force redownload confirmation opened");
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            let wipe_db_btn = ui.add_enabled(
                !wipe_pending,
                Button::new(tr("Wipe repository database entries"))
                    .fill(color_text_error)
                    .min_size(Vec2::new(w, 30.0)),
            );
            if wipe_db_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if wipe_db_btn.clicked() {
                self.show_wipe_repo_db_confirmation = true;
                warn!("Repository database wipe confirmation opened");
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            let d = ui.add_sized(
                Vec2::new(w, 30.0),
                Button::new(tr("Delete repository")).fill(color_text_error),
            );
            if d.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if d.clicked() {
                self.show_delete_confirmation = true;
                self.delete_repository_delete_files = false;
                warn!("Repository delete confirmation opened");
            }
        });
    }
}
