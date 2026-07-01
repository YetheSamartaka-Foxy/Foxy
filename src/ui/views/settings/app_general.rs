use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::types::{HashIoProfilePreference, UiRendererPreference};
use eframe::egui::{self, Button, RichText, TextEdit, Ui, Vec2};
use log::{info, warn};

use super::render_wrapped_info_row;

impl Foxy {
    fn render_wrapped_settings_checkbox(
        ui: &mut Ui,
        checked: &mut bool,
        label: String,
        hover_text: Option<String>,
        row_width: f32,
        changed: &mut bool,
    ) -> egui::Response {
        let label_width = ui
            .painter()
            .layout_no_wrap(
                label.clone(),
                egui::TextStyle::Button.resolve(ui.style()),
                ui.visuals().text_color(),
            )
            .size()
            .x;
        let checkbox_padding = ui.spacing().interact_size.x + ui.spacing().item_spacing.x + 8.0;
        let slot_width = (label_width + checkbox_padding)
            .min(row_width)
            .max(ui.spacing().interact_size.x);

        let response = ui
            .allocate_ui_with_layout(
                Vec2::new(slot_width, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(slot_width);
                    ui.set_max_width(slot_width);
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    Self::ui_state_checkbox(ui, checked, label)
                },
            )
            .inner;
        let response = if let Some(hover_text) = hover_text {
            response.on_hover_text(hover_text)
        } else {
            response
        };

        if response.changed() {
            *changed = true;
        }
        if response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }

        response
    }

    /// Language selector, download speed limit, checkboxes (auto-backup, auto-apply, auto-recheck,
    /// auto-quick-scan), open config/logs buttons, reset, debug windows, close/hide-after-launch,
    /// and wipe database (dev).
    pub(super) fn render_application_settings_general(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        changed: &mut bool,
    ) {
        render_wrapped_info_row(
            ui,
            horizontal_padding,
            RichText::new(format!(
                "{} {}",
                '\u{2139}',
                tr("Here you configure basic app options")
            ))
            .italics()
            .color(self.color_text_dim()),
        );
        ui.separator();

        // Language selector
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Language"));
            ui.add_space(horizontal_padding);

            // (code, translation key, English name) tuples for the language selector.
            let languages: &[(&str, &str, &str)] = &[
                ("system", "System", ""),
                ("ar", "Arabic", "Arabic"),
                ("bg", "Bulgarian", "Bulgarian"),
                ("bn", "Bengali", "Bengali"),
                ("cs", "Czech", "Czech"),
                ("de", "German", "German"),
                ("en", "English", "English"),
                ("es", "Spanish", "Spanish"),
                ("fa", "Persian", "Persian"),
                ("fr", "French", "French"),
                ("he", "Hebrew", "Hebrew"),
                ("hi", "Hindi", "Hindi"),
                ("hr", "Croatian", "Croatian"),
                ("hu", "Hungarian", "Hungarian"),
                ("id", "Indonesian", "Indonesian"),
                ("it", "Italian", "Italian"),
                ("ja", "Japanese", "Japanese"),
                ("ko", "Korean", "Korean"),
                ("da", "Danish", "Danish"),
                ("et", "Estonian", "Estonian"),
                ("fi", "Finnish", "Finnish"),
                ("el", "Greek", "Greek"),
                ("lt", "Lithuanian", "Lithuanian"),
                ("lv", "Latvian", "Latvian"),
                ("nb", "Norwegian", "Norwegian"),
                ("nl", "Dutch", "Dutch"),
                ("pl", "Polish", "Polish"),
                ("pt", "Portuguese", "Portuguese"),
                ("pt-BR", "Brazilian Portuguese", "Brazilian Portuguese"),
                ("ro", "Romanian", "Romanian"),
                ("ru", "Russian", "Russian"),
                ("sk", "Slovak", "Slovak"),
                ("sl", "Slovenian", "Slovenian"),
                ("sr", "Serbian", "Serbian"),
                ("sv", "Swedish", "Swedish"),
                ("th", "Thai", "Thai"),
                ("tl", "Tagalog", "Tagalog"),
                ("tr", "Turkish", "Turkish"),
                ("uk", "Ukrainian", "Ukrainian"),
                ("ur", "Urdu", "Urdu"),
                ("vi", "Vietnamese", "Vietnamese"),
                ("zh", "Chinese", "Chinese"),
            ];

            let format_label = |tr_key: &str, eng: &str| -> String {
                if eng.is_empty() {
                    tr(tr_key)
                } else {
                    format!("{} ({})", tr(tr_key), eng)
                }
            };

            let selected_language = languages
                .iter()
                .find(|(code, _, _)| *code == self.settings_view_state.locale.as_str())
                .map(|(_, tr_key, eng)| format_label(tr_key, eng))
                .unwrap_or_else(|| format_label("English", "English"));

            let normalized_filter = self
                .settings_view_state
                .language_filter
                .trim()
                .to_ascii_lowercase();

            let locale_combo = egui::ComboBox::from_label("")
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .selected_text(selected_language)
                .show_ui(ui, |ui| {
                    ui.set_min_width(320.0);
                    let _search_response = ui.add(
                        TextEdit::singleline(&mut self.settings_view_state.language_filter)
                            .hint_text(tr("Search...")),
                    );
                    ui.separator();

                    let filtered_languages: Vec<(&str, &str, &str)> = languages
                        .iter()
                        .copied()
                        .filter(|(_, tr_key, eng)| {
                            if normalized_filter.is_empty() {
                                return true;
                            }
                            let localized = tr(tr_key);
                            let english = eng.to_ascii_lowercase();
                            localized.to_ascii_lowercase().contains(&normalized_filter)
                                || english.contains(&normalized_filter)
                                || tr_key.to_ascii_lowercase().contains(&normalized_filter)
                        })
                        .collect();

                    egui::ScrollArea::vertical()
                        .id_salt("settings_language_picker_list")
                        .max_height(240.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for &(code, tr_key, eng) in &filtered_languages {
                                let selected = self.settings_view_state.locale == code;
                                let label = format_label(tr_key, eng);
                                let response = ui.selectable_label(selected, label);
                                if response.clicked() && !selected {
                                    self.settings_view_state.locale = code.to_string();
                                    self.i18n.set_language(code);
                                    *changed = true;
                                }
                            }
                        });
                });
            if locale_combo.response.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Download speed limit
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Download Speed Limit"));

            let mut unlimited = self.settings_view_state.download_speed_limit_mbps.is_none();
            let unlimited_checkbox = Self::ui_state_checkbox(ui, &mut unlimited, tr("Unlimited"));
            if unlimited_checkbox.changed() {
                self.settings_view_state.download_speed_limit_mbps =
                    if unlimited { None } else { Some(1) };
                *changed = true;
            }
            if unlimited_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            if !unlimited {
                let limit_mbps = self
                    .settings_view_state
                    .download_speed_limit_mbps
                    .get_or_insert(1);
                if *limit_mbps == 0 {
                    *limit_mbps = 1;
                }
                let speed_limit_input = ui.add(
                    egui::DragValue::new(limit_mbps)
                        .range(1..=u32::MAX)
                        .suffix(format!(" {}", tr("Mbps"))),
                );
                if speed_limit_input.changed() {
                    *changed = true;
                }
                if speed_limit_input.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
            }

            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Hashing profile
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Hashing profile"));

            let profile_label = |profile: HashIoProfilePreference| -> String {
                match profile {
                    HashIoProfilePreference::Auto => tr("Auto"),
                    HashIoProfilePreference::Conservative => tr("Conservative"),
                    HashIoProfilePreference::Balanced => tr("Balanced"),
                    HashIoProfilePreference::Aggressive => tr("Aggressive"),
                }
            };
            let selected_text = profile_label(self.settings_view_state.hash_io_profile);
            let combo = egui::ComboBox::from_id_salt("settings_hash_io_profile")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for (profile, description) in [
                        (
                            HashIoProfilePreference::Auto,
                            tr("Benchmark initial hashing work and choose the fastest profile."),
                        ),
                        (
                            HashIoProfilePreference::Conservative,
                            tr("Low concurrency for constrained systems."),
                        ),
                        (
                            HashIoProfilePreference::Balanced,
                            tr("Moderate disk concurrency for mixed systems."),
                        ),
                        (
                            HashIoProfilePreference::Aggressive,
                            tr("High concurrency for systems that can sustain it."),
                        ),
                    ] {
                        let response = ui
                            .selectable_value(
                                &mut self.settings_view_state.hash_io_profile,
                                profile,
                                profile_label(profile),
                            )
                            .on_hover_text(description.as_str());
                        if response.changed() {
                            *changed = true;
                        }
                    }
                });
            if combo.response.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Renderer
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Renderer"));

            let renderer_label = |renderer: UiRendererPreference| -> String {
                match renderer {
                    UiRendererPreference::Auto => tr("Auto (WGPU)"),
                    UiRendererPreference::Wgpu => tr("WGPU"),
                    UiRendererPreference::Glow => tr("Glow"),
                }
            };
            let selected_text = renderer_label(self.settings_view_state.ui_renderer);
            let combo = egui::ComboBox::from_id_salt("settings_ui_renderer")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for (renderer, description) in [
                        (
                            UiRendererPreference::Auto,
                            tr("Use WGPU by default, but allow Foxy to switch to Glow after a WGPU renderer crash."),
                        ),
                        (
                            UiRendererPreference::Wgpu,
                            tr("Force WGPU. This is usually fastest, but may be less reliable on some graphics drivers."),
                        ),
                        (
                            UiRendererPreference::Glow,
                            tr("Force Glow/OpenGL. This is usually more compatible after graphics driver or WGPU crashes."),
                        ),
                    ] {
                        let response = ui
                            .selectable_value(
                                &mut self.settings_view_state.ui_renderer,
                                renderer,
                                renderer_label(renderer),
                            )
                            .on_hover_text(description.as_str());
                        if response.changed() {
                            *changed = true;
                        }
                    }
                });
            if combo
                .response
                .on_hover_text(tr(
                    "Select the graphics renderer used by the Foxy UI. Changes take effect after restart.",
                ))
                .hovered()
            {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Repository and launch automation options
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let row_width = (ui.available_width() - horizontal_padding).max(0.0);
            ui.allocate_ui_with_layout(
                Vec2::new(row_width, ui.spacing().interact_size.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(row_width);
                    ui.horizontal_wrapped(|ui| {
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.auto_backup_on_update,
                            tr("Auto backup addons before update"),
                            Some(tr("Automatically create a backup of each addon before downloading updates so you can restore the previous version if needed.")),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.apply_repo_json_client_parameters,
                            tr("Auto apply repo.json launch parameters"),
                            Some(tr("Automatically apply launch parameters from the repository's repo.json when launching Arma 3.")),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.apply_repo_json_dlc_content,
                            tr("Auto apply repo.json DLC content"),
                            Some(tr("Automatically enable DLC content specified by the repository's repo.json when launching Arma 3.")),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.warn_editor_external_addons,
                            tr("Warn before launching editor with external addons"),
                            Some(tr(
                                "Show a confirmation before opening Eden Editor when additional/external addons are enabled.",
                            )),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.enable_editor_mission_list,
                            tr("Show Editor Missions list"),
                            Some(tr(
                                "Show the Editor Missions section in the repository view. Can be overridden per repository.",
                            )),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.enable_server_list,
                            tr("Show Servers list"),
                            Some(tr(
                                "Show the Servers section in the repository view. Can be overridden per repository.",
                            )),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.check_server_addons_before_join,
                            tr("Check server addons before joining"),
                            Some(tr(
                                "Before joining a server, query its addon list and offer to enable matching disabled local addons.",
                            )),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.check_ts3_running_before_join,
                            tr("Check TeamSpeak is running before joining"),
                            Some(tr(
                                "Before joining a server with a repository that ships TeamSpeak plugins, warn if TeamSpeak 3 is not running and offer to launch it.",
                            )),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.check_steam_running_before_launch,
                            tr("Check Steam is running before launching"),
                            Some(tr(
                                "Before launching or joining, warn if Steam is not running (Arma 3 needs Steam) and offer to launch it.",
                            )),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.auto_recheck_on_launch,
                            tr("Auto recheck repositories on launch"),
                            Some(tr("Automatically run a remote data recheck for all repositories when the app starts.")),
                            row_width,
                            changed,
                        );
                        Self::render_wrapped_settings_checkbox(
                            ui,
                            &mut self.settings_view_state.auto_quick_scan_on_launch,
                            tr("Auto quick scan for changes on launch"),
                            Some(tr("Automatically run a quick local file scan for all repositories when the app starts.")),
                            row_width,
                            changed,
                        );
                    });
                },
            );
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Open config directory / Open log folder buttons
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let row_width = (ui.available_width() - 2.0 * horizontal_padding).max(0.0);
            let button_width = ((row_width - ui.spacing().item_spacing.x).max(0.0)) / 2.0;

            let open_config_button = ui.add_sized(
                Vec2::new(button_width, 30.0),
                Button::new(tr("Open config directory")).fill(self.color_main_bg()),
            ).on_hover_text(tr("Open the folder where Foxy stores its configuration, settings, and database files."));

            if open_config_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if open_config_button.clicked() {
                info!("Opening config directory from settings");
                if !self.open_config_directory() {
                    self.show_error_toast(self.t("Failed to open config directory."));
                }
            }

            let open_logs_button = ui.add_sized(
                Vec2::new(button_width, 30.0),
                Button::new(tr("Open log folder")).fill(self.color_main_bg()),
            ).on_hover_text(tr("Open the folder where Foxy stores its log files."));

            if open_logs_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if open_logs_button.clicked() {
                info!("Opening log folder from settings");
                if !self.open_log_folder() {
                    self.show_error_toast(self.t("Failed to open log folder."));
                }
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Export logs to ZIP
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let button_width = ui.available_width() - 2.0 * horizontal_padding;
            let export_logs_button = ui.add_sized(
                Vec2::new(button_width, 40.0),
                Button::new(
                    RichText::new(format!("\u{1F4E6}  {}", tr("Export logs to ZIP"))).size(16.0),
                )
                .fill(self.color_main_bg()),
            ).on_hover_text(tr("Package Foxy log files into a timestamped ZIP archive for sharing with support."));

            if export_logs_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if export_logs_button.clicked() {
                info!("Export logs to ZIP triggered from settings");
                match self.export_logs_to_zip() {
                    Ok(true) => {
                        self.show_success_toast(self.t("Logs exported successfully."));
                    }
                    Ok(false) => {
                        // User cancelled the save dialog – nothing to report.
                    }
                    Err(err) => {
                        warn!("Failed to export logs to ZIP: {}", err);
                        self.show_error_toast(self.t("Failed to export logs."));
                    }
                }
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Reset button
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let button_width = ui.available_width() - 2.0 * horizontal_padding;
            let reset_button = ui.add_sized(
                Vec2::new(button_width, 30.0),
                Button::new(tr("Reset")).fill(self.color_main_bg()),
            ).on_hover_text(tr("Reset all application settings to their defaults. Repository data is also cleared. This cannot be undone."));

            if reset_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if reset_button.clicked() {
                self.pending_settings_reset_confirmation = true;
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Show debug windows, memory diagnostics, close after launch, hide to tray
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);

            let show_debug_windows_checkbox = Self::ui_state_checkbox(
                ui,
                &mut self.settings_view_state.show_debug_windows,
                tr("Show Debug Windows"),
            ).on_hover_text(tr("Show developer debug windows for advanced diagnostics."));
            if show_debug_windows_checkbox.changed() {
                *changed = true;
            }
            if show_debug_windows_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let show_memory_diagnostics_icon_checkbox = Self::ui_state_checkbox(
                ui,
                &mut self.settings_view_state.show_memory_diagnostics_icon,
                tr("Show memory diagnostics icon in footer"),
            ).on_hover_text(tr("Show or hide the memory diagnostics icon in the footer. Opens the memory diagnostics panel (F4)."));
            if show_memory_diagnostics_icon_checkbox.changed() {
                if !self.settings_view_state.show_memory_diagnostics_icon {
                    self.show_memory_diagnostics_window = false;
                }
                *changed = true;
            }
            if show_memory_diagnostics_icon_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let show_fps_counter_checkbox = Self::ui_state_checkbox(
                ui,
                &mut self.settings_view_state.show_fps_counter,
                tr("Show FPS counter"),
            )
            .on_hover_text(tr(
                "Display a frames-per-second counter in the bottom-right corner. Keeps the UI repainting continuously while enabled.",
            ));
            if show_fps_counter_checkbox.changed() {
                *changed = true;
            }
            if show_fps_counter_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let hide_repository_image_checkbox = Self::ui_state_checkbox(
                ui,
                &mut self.settings_view_state.hide_repository_image,
                tr("Hide repository image"),
            )
            .on_hover_text(tr(
                "Hide the banner image shown at the top of the repository and repository space views. Individual repositories can override this in their settings.",
            ));
            if hide_repository_image_checkbox.changed() {
                *changed = true;
            }
            if hide_repository_image_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let close_after_launch_checkbox = Self::ui_state_checkbox(
                ui,
                &mut self.settings_view_state.close_after_launch,
                tr("Close after launch"),
            ).on_hover_text(tr("Automatically close Foxy after Arma 3 launches."));
            if close_after_launch_checkbox.changed() {
                if self.settings_view_state.close_after_launch {
                    self.settings_view_state.hide_to_tray_after_launch = false;
                }
                *changed = true;
            }
            if close_after_launch_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let tray_available = crate::ui::tray::TrayManager::is_available();
            if !tray_available && self.settings_view_state.hide_to_tray_after_launch {
                self.settings_view_state.hide_to_tray_after_launch = false;
                *changed = true;
            }
            let hide_to_tray_checkbox = ui
                .add_enabled_ui(!self.settings_view_state.close_after_launch && tray_available, |ui| {
                    Self::ui_state_checkbox(
                        ui,
                        &mut self.settings_view_state.hide_to_tray_after_launch,
                        tr("Hide to tray after launch"),
                    ).on_hover_text(tr("Minimize Foxy to the system tray instead of closing after Arma 3 launches. Disabled when Close after launch is enabled."))
                })
                .inner;
            if hide_to_tray_checkbox.changed() {
                *changed = true;
            }
            if hide_to_tray_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            ui.add_space(horizontal_padding);
        });

        ui.separator();

        // Migrate from Swifty button
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let button_width = ui.available_width() - 2.0 * horizontal_padding;
            let migrate_button = ui.add_sized(
                Vec2::new(button_width, 30.0),
                Button::new(tr("Migrate from Swifty")).fill(self.color_main_bg()),
            ).on_hover_text(tr("Import repositories from an existing Swifty installation. Your Swifty data will not be modified."));

            if migrate_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if migrate_button.clicked() {
                info!("Opening Swifty migration view from settings");
                self.open_swifty_migration_view();
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Wipe Database button
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let button_width = ui.available_width() - 2.0 * horizontal_padding;
            let sync_active = self.repository_sync_active();
            let wipe_button = ui
                .add_enabled_ui(!sync_active, |ui| {
                    ui.add_sized(
                        Vec2::new(button_width, 30.0),
                        Button::new(tr("Wipe Database")).fill(self.color_text_error()),
                    ).on_hover_text(tr("Remove all cached repository data from the database. Use this only as a last resort."))
                })
                .inner;

            if wipe_button.hovered() && !sync_active {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if wipe_button.clicked() {
                warn!("Database wipe confirmation opened from settings view");
                self.show_wipe_db_confirmation = true;
            }
            ui.add_space(horizontal_padding);
        });
    }

    pub(super) fn render_application_settings_reset_confirmation(&mut self, ui: &mut Ui) {
        if !self.pending_settings_reset_confirmation {
            return;
        }

        egui::Window::new(tr("Confirm Settings Reset"))
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
            .default_width(520.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(tr(
                        "Are you sure you want to reset all settings and repositories to defaults? This cannot be undone.",
                    ));
                    ui.add_space(20.0);
                    let yes_btn = ui.button(tr("Reset all settings"));
                    if yes_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if yes_btn.clicked() {
                        self.pending_settings_reset_confirmation = false;
                        warn!("Resetting settings and repositories to defaults from settings view");
                        self.reset_settings();
                        self.reset_repositories();
                        self.update_debug_mode();
                        self.save_settings();
                    }

                    let no_btn = ui.button(tr("Cancel"));
                    if no_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if no_btn.clicked() {
                        self.pending_settings_reset_confirmation = false;
                        info!("Settings reset canceled");
                    }
                });
            });
    }
}
