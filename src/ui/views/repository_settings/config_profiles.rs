use crate::core::game::LaunchFlagField;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::palette::contrast_text_on;
use crate::ui::types::{has_launch_param_token, set_launch_param_token};
use eframe::egui::{Button, Color32, RichText, TextEdit, Ui, Vec2};
use log::{info, warn};

struct LaunchParamTarget<'a> {
    csla: &'a mut bool,
    ef: &'a mut bool,
    gm: &'a mut bool,
    rf: &'a mut bool,
    spe: &'a mut bool,
    vn: &'a mut bool,
    ws: &'a mut bool,
    skip_intro: &'a mut bool,
    no_splash: &'a mut bool,
    world_empty: &'a mut bool,
    load_mission_to_memory: &'a mut bool,
    enable_ht: &'a mut bool,
    huge_pages: &'a mut bool,
    no_logs: &'a mut bool,
    additional_params: &'a mut String,
}

impl LaunchParamTarget<'_> {
    fn dedicated_field(&mut self, field: LaunchFlagField) -> Option<&mut bool> {
        match field {
            LaunchFlagField::SkipIntro => Some(self.skip_intro),
            LaunchFlagField::NoSplash => Some(self.no_splash),
            LaunchFlagField::WorldEmpty => Some(self.world_empty),
            LaunchFlagField::LoadMissionToMemory => Some(self.load_mission_to_memory),
            LaunchFlagField::EnableHt => Some(self.enable_ht),
            LaunchFlagField::HugePages => Some(self.huge_pages),
            LaunchFlagField::NoLogs => Some(self.no_logs),
            LaunchFlagField::AdditionalParamToken => None,
        }
    }
}

impl Foxy {
    /// Show launch options from the active module's capabilities so games never inherit another game's flags.
    pub(super) fn render_repository_configuration_profiles(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        pad_f32: f32,
        changed: &mut bool,
    ) {
        let module = crate::core::game::registry().active();
        let show_creator_dlc = module.capabilities().creator_dlc;
        let launch_flags = module.repository_launch_flags();

        let repo = &self.repository_view_state.repositories[repo_index];
        let profile_selected = repo
            .selected_profile
            .as_ref()
            .is_some_and(|name| repo.profiles.iter().any(|profile| &profile.name == name));
        let launch_params_managed = !repo.is_local()
            && !profile_selected
            && self.repo_apply_repo_json_client_parameters(repo);
        let managed_hint = tr(
            "Auto apply repo.json launch parameters is on, so the repository overwrites these fields on every refresh. Turn that setting off, or select a launch profile, to edit them.",
        );

        let repo = &mut self.repository_view_state.repositories[repo_index];
        let mut target =
            match repo.selected_profile.as_ref().and_then(|selected_name| {
                repo.profiles.iter_mut().find(|p| &p.name == selected_name)
            }) {
                Some(profile) => LaunchParamTarget {
                    csla: &mut profile.csla,
                    ef: &mut profile.ef,
                    gm: &mut profile.gm,
                    rf: &mut profile.rf,
                    spe: &mut profile.spe,
                    vn: &mut profile.vn,
                    ws: &mut profile.ws,
                    skip_intro: &mut profile.skip_intro,
                    no_splash: &mut profile.no_splash,
                    world_empty: &mut profile.world_empty,
                    load_mission_to_memory: &mut profile.load_mission_to_memory,
                    enable_ht: &mut profile.enable_ht,
                    huge_pages: &mut profile.huge_pages,
                    no_logs: &mut profile.no_logs,
                    additional_params: &mut profile.additional_params,
                },
                None => LaunchParamTarget {
                    csla: &mut repo.csla,
                    ef: &mut repo.ef,
                    gm: &mut repo.gm,
                    rf: &mut repo.rf,
                    spe: &mut repo.spe,
                    vn: &mut repo.vn,
                    ws: &mut repo.ws,
                    skip_intro: &mut repo.skip_intro,
                    no_splash: &mut repo.no_splash,
                    world_empty: &mut repo.world_empty,
                    load_mission_to_memory: &mut repo.load_mission_to_memory,
                    enable_ht: &mut repo.enable_ht,
                    huge_pages: &mut repo.huge_pages,
                    no_logs: &mut repo.no_logs,
                    additional_params: &mut repo.additional_params,
                },
            };

        if show_creator_dlc {
            ui.horizontal(|ui| {
                ui.label(tr("Creator DLCs"));
            });
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(10.0, 6.0);
                ui.horizontal_wrapped(|ui| {
                    for (flag, label) in [
                        (&mut *target.csla, "\u{010C}SLA"),
                        (&mut *target.ef, "Expeditionary Forces"),
                        (&mut *target.gm, "Global Mobilization"),
                        (&mut *target.rf, "Reaction Forces"),
                        (&mut *target.spe, "Spearhead 1944"),
                        (&mut *target.vn, "S.O.G. PF"),
                        (&mut *target.ws, "Western Sahara"),
                    ] {
                        let cb = Self::ui_state_checkbox(ui, flag, label);
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
        }

        if !launch_flags.is_empty() {
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
                    for flag in &launch_flags {
                        let cb = match target.dedicated_field(flag.field) {
                            Some(value) => Self::ui_state_checkbox(ui, value, flag.flag),
                            None => {
                                let mut enabled =
                                    has_launch_param_token(target.additional_params, flag.flag);
                                let cb = Self::ui_state_checkbox(ui, &mut enabled, flag.flag);
                                if cb.changed() {
                                    *target.additional_params = set_launch_param_token(
                                        target.additional_params,
                                        flag.flag,
                                        enabled,
                                    );
                                }
                                cb
                            }
                        };
                        let cb = cb.on_hover_text(tr(flag.help));
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
        }

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
                .add_enabled(!launch_params_managed, TextEdit::singleline(target.additional_params).desired_width(w))
                .on_hover_text(tr("Extra CLI startup parameters. Separate multiple options with spaces and wrap paths with spaces in quotes."))
                .on_disabled_hover_text(managed_hint.as_str());
            if r.changed() { *changed = true; }
            if r.hovered() && !launch_params_managed {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
        });
        ui.separator();
    }

    #[expect(clippy::too_many_arguments)]
    pub(super) fn render_repository_configuration_actions(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        color_primary_accent: Color32,
        color_action_destructive: Color32,
        pad_f32: f32,
        _force_redownload: &mut bool,
        _wipe_repository_db_entries: &mut bool,
        recheck_repository_integrity: &mut bool,
    ) {
        let wipe_pending = {
            let repo = &self.repository_view_state.repositories[repo_index];
            self.is_repository_db_wipe_pending(&repo.address)
        };
        let accent_ink = contrast_text_on(color_primary_accent);
        let destructive_ink = contrast_text_on(color_action_destructive);

        if !self.repository_view_state.repositories[repo_index].is_local() {
            ui.horizontal(|ui| {
                let w = ui.available_width() - 2.0 * pad_f32;
                let recheck_integrity_btn = ui.add_sized(
                    Vec2::new(w, 30.0),
                    Button::new(RichText::new(tr("Recheck repository integrity")).color(accent_ink))
                        .fill(color_primary_accent),
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
                    Button::new(RichText::new(tr("Force redownload repository")).color(accent_ink))
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
                    Button::new(
                        RichText::new(tr("Wipe repository database entries")).color(destructive_ink),
                    )
                    .fill(color_action_destructive)
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
        }
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            let d = ui.add_sized(
                Vec2::new(w, 30.0),
                Button::new(RichText::new(tr("Delete repository")).color(destructive_ink))
                    .fill(color_action_destructive),
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
