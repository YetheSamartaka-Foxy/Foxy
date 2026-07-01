use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::types::HashAlgorithmPreference;
use eframe::egui::{self, Ui, Vec2};

impl Foxy {
    const REPO_SETTING_BLOCK_HEIGHT: f32 = 60.0;
    const REPO_SETTING_LABEL_TO_COMBO_GAP: f32 = 1.0;
    const REPO_SETTING_COMBO_MAX_WIDTH: f32 = 190.0;

    fn render_repo_setting_label(ui: &mut Ui, label: String, block_width: f32) -> egui::Response {
        ui.set_width(block_width);
        let response = ui.add(egui::Label::new(label).wrap());
        ui.add_space(Self::REPO_SETTING_LABEL_TO_COMBO_GAP);
        response
    }

    fn render_repo_override_combo(
        ui: &mut Ui,
        repo_value: &mut Option<bool>,
        global_value: bool,
        id_salt: (&'static str, usize),
        label_key: &'static str,
        block_width: f32,
        changed: &mut bool,
    ) {
        ui.allocate_ui_with_layout(
            Vec2::new(block_width, Self::REPO_SETTING_BLOCK_HEIGHT),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let combo_width =
                    (block_width - 8.0).clamp(150.0, Self::REPO_SETTING_COMBO_MAX_WIDTH);
                Self::render_repo_setting_label(ui, tr(label_key), block_width);

                let selected_text = match *repo_value {
                    Some(true) => tr("On (override)"),
                    Some(false) => tr("Off (override)"),
                    None => {
                        if global_value {
                            tr("Use global (On)")
                        } else {
                            tr("Use global (Off)")
                        }
                    }
                };

                let combo = egui::ComboBox::from_id_salt(id_salt)
                    .width(combo_width)
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        let use_global =
                            ui.selectable_label(repo_value.is_none(), tr("Use global"));
                        if use_global.clicked() {
                            *repo_value = None;
                            *changed = true;
                        }

                        let on = ui.selectable_label(*repo_value == Some(true), tr("On"));
                        if on.clicked() {
                            *repo_value = Some(true);
                            *changed = true;
                        }

                        let off = ui.selectable_label(*repo_value == Some(false), tr("Off"));
                        if off.clicked() {
                            *repo_value = Some(false);
                            *changed = true;
                        }
                    });
                if combo.response.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
            },
        );
    }

    /// Auto recheck, auto quick scan, auto backup, auto apply repo.json params, auto apply
    /// repo.json DLC, and hashing algorithm preference.
    pub(super) fn render_repository_configuration_sync(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        _color_text_dim: egui::Color32,
        changed: &mut bool,
    ) {
        let global_auto_recheck_on_launch = self.settings_view_state.auto_recheck_on_launch;
        let global_auto_quick_scan_on_launch = self.settings_view_state.auto_quick_scan_on_launch;
        let global_auto_backup_on_update = self.settings_view_state.auto_backup_on_update;
        let global_apply_repo_json_client_parameters =
            self.settings_view_state.apply_repo_json_client_parameters;
        let global_apply_repo_json_dlc_content =
            self.settings_view_state.apply_repo_json_dlc_content;
        let global_warn_editor_external_addons =
            self.settings_view_state.warn_editor_external_addons;
        let global_enable_editor_mission_list = self.settings_view_state.enable_editor_mission_list;
        let global_enable_server_list = self.settings_view_state.enable_server_list;
        let global_check_server_addons_before_join =
            self.settings_view_state.check_server_addons_before_join;
        let global_check_ts3_running_before_join =
            self.settings_view_state.check_ts3_running_before_join;
        let global_check_steam_running_before_launch =
            self.settings_view_state.check_steam_running_before_launch;
        let global_hide_repository_image = self.settings_view_state.hide_repository_image;

        let repo = &mut self.repository_view_state.repositories[repo_index];

        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(14.0, 18.0);
            let item_spacing_x = ui.spacing().item_spacing.x;
            let available_width = ui.available_width().max(240.0);
            let target_columns = if available_width >= 1400.0 {
                5.0
            } else if available_width >= 1100.0 {
                4.0
            } else if available_width >= 840.0 {
                3.0
            } else if available_width >= 560.0 {
                2.0
            } else {
                1.0
            };
            let block_width = ((available_width - (target_columns - 1.0) * item_spacing_x)
                / target_columns)
                .clamp(220.0, 360.0);

            ui.horizontal_wrapped(|ui| {
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.auto_recheck_on_launch,
                    global_auto_recheck_on_launch,
                    ("repo_auto_recheck_on_launch", repo_index),
                    "Auto recheck on launch",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.auto_quick_scan_on_launch,
                    global_auto_quick_scan_on_launch,
                    ("repo_auto_quick_scan_on_launch", repo_index),
                    "Auto quick scan on launch",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.auto_backup_on_update,
                    global_auto_backup_on_update,
                    ("repo_auto_backup_on_update", repo_index),
                    "Auto backup addons before update",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.apply_repo_json_client_parameters,
                    global_apply_repo_json_client_parameters,
                    ("repo_apply_repo_json_client_parameters", repo_index),
                    "Auto apply repo.json launch parameters",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.apply_repo_json_dlc_content,
                    global_apply_repo_json_dlc_content,
                    ("repo_apply_repo_json_dlc_content", repo_index),
                    "Auto apply repo.json DLC content",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.warn_editor_external_addons,
                    global_warn_editor_external_addons,
                    ("repo_warn_editor_external_addons", repo_index),
                    "Editor external addons warning",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.enable_editor_mission_list,
                    global_enable_editor_mission_list,
                    ("repo_enable_editor_mission_list", repo_index),
                    "Show Editor Missions list",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.enable_server_list,
                    global_enable_server_list,
                    ("repo_enable_server_list", repo_index),
                    "Show Servers list",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.check_server_addons_before_join,
                    global_check_server_addons_before_join,
                    ("repo_check_server_addons_before_join", repo_index),
                    "Check server addons before joining",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.check_ts3_running_before_join,
                    global_check_ts3_running_before_join,
                    ("repo_check_ts3_running_before_join", repo_index),
                    "Check TeamSpeak is running before joining",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.check_steam_running_before_launch,
                    global_check_steam_running_before_launch,
                    ("repo_check_steam_running_before_launch", repo_index),
                    "Check Steam is running before launching",
                    block_width,
                    changed,
                );
                Self::render_repo_override_combo(
                    ui,
                    &mut repo.hide_repo_image,
                    global_hide_repository_image,
                    ("repo_hide_repo_image", repo_index),
                    "Hide repository image",
                    block_width,
                    changed,
                );

                ui.allocate_ui_with_layout(
                    Vec2::new(block_width, Self::REPO_SETTING_BLOCK_HEIGHT),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let combo_width =
                            (block_width - 8.0).clamp(150.0, Self::REPO_SETTING_COMBO_MAX_WIDTH);
                        let hashing_help = tr("When set to Prefer Swifty, Foxy will use MD5 hashing instead of BLAKE3 if the repository supports it.");
                        Self::render_repo_setting_label(ui, tr("Hashing algorithm"), block_width)
                            .on_hover_text(hashing_help.as_str());

                        let selected_text = match repo.hash_algorithm_preference {
                            HashAlgorithmPreference::PreferFoxy => tr("Prefer Foxy (BLAKE3)"),
                            HashAlgorithmPreference::PreferSwifty => tr("Prefer Swifty (MD5)"),
                        };
                        let combo = egui::ComboBox::from_id_salt((
                            "repo_hash_algorithm_preference",
                            repo_index,
                        ))
                        .width(combo_width)
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            let foxy = ui.selectable_label(
                                repo.hash_algorithm_preference
                                    == HashAlgorithmPreference::PreferFoxy,
                                tr("Prefer Foxy (BLAKE3)"),
                            );
                            if foxy.clicked() {
                                repo.hash_algorithm_preference =
                                    HashAlgorithmPreference::PreferFoxy;
                                *changed = true;
                            }

                            let swifty = ui.selectable_label(
                                repo.hash_algorithm_preference
                                    == HashAlgorithmPreference::PreferSwifty,
                                tr("Prefer Swifty (MD5)"),
                            );
                            if swifty.clicked() {
                                repo.hash_algorithm_preference =
                                    HashAlgorithmPreference::PreferSwifty;
                                *changed = true;
                            }
                        });
                        if combo.response.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        combo.response.on_hover_text(hashing_help);
                    },
                );
            });
        });

        ui.separator();
    }
}
