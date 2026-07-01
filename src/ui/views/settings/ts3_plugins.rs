use crate::core::ts3_plugin;
use crate::core::utils::format::sanitize_log_path;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use eframe::egui::{self, Button, Frame, Label, Margin, RichText, ScrollArea, Ui, Vec2};
use log::{info, warn};

impl Foxy {
    /// Invalidate the TS3 plugin cache and cancel any in-flight scan.
    pub fn invalidate_ts3_plugin_cache(&mut self) {
        info!(
            "Invalidating TS3 plugin cache: had_cache={} scan_in_flight={} had_ts3_running_cache={}",
            self.ts3_plugin_cache.is_some(),
            self.ts3_plugin_scanning,
            self.ts3_running_cache.is_some()
        );
        self.ts3_plugin_cache = None;
        self.ts3_plugin_scan_rx = None;
        self.ts3_plugin_scanning = false;
        self.ts3_running_cache = None;
    }

    /// Kick off a background thread that scans repositories for TS3 plugins
    /// and checks whether TeamSpeak is running. Results arrive via channel.
    fn start_ts3_plugin_scan(&mut self) {
        let repo_paths: Vec<String> = self
            .repository_view_state
            .repositories
            .iter()
            .map(|r| r.path.clone())
            .filter(|p| !p.is_empty())
            .collect();

        info!(
            "Starting background TS3 plugin settings scan: repository_count={} tracked_installed_plugins={}",
            repo_paths.len(),
            self.settings_view_state.ts3_installed_plugin_hashes.len()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        self.ts3_plugin_scan_rx = Some(rx);
        self.ts3_plugin_scanning = true;

        std::thread::spawn(move || {
            let plugins = ts3_plugin::scan_all_repositories_for_ts3_plugins(&repo_paths);
            let ts3_running = ts3_plugin::is_teamspeak_running();
            let plugin_count = plugins.len();
            if tx.send((plugins, ts3_running)).is_err() {
                warn!(
                    "Failed to deliver TS3 plugin settings scan result: plugin_count={} ts3_running={}",
                    plugin_count, ts3_running
                );
            }
        });
    }

    /// Poll the background scan channel. Returns true if results just arrived.
    fn poll_ts3_plugin_scan(&mut self) -> bool {
        if let Some(rx) = &self.ts3_plugin_scan_rx {
            match rx.try_recv() {
                Ok((plugins, ts3_running)) => {
                    info!(
                        "Received TS3 plugin settings scan result: plugin_count={} ts3_running={}",
                        plugins.len(),
                        ts3_running
                    );
                    for plugin in &plugins {
                        let path_key = plugin.plugin_path.display().to_string();
                        let installed_hash = self
                            .settings_view_state
                            .ts3_installed_plugin_hashes
                            .get(&path_key);
                        let ts3_lookup =
                            ts3_plugin::lookup_installed_teamspeak_plugin(&plugin.plugin_path);
                        info!(
                            "Evaluated TS3 plugin install state from settings scan: addon={} path={} detected_hash={} foxy_stored_hash_present={} foxy_stored_hash_matches={} ts3_search_name={} ts3_expected_files={} ts3_candidate_plugin_dirs={} ts3_existing_plugin_dirs={} ts3_installed_matches={} ts3_missing_files={} ts3_hash_mismatches={} ts3_installed={} ts3_up_to_date={}",
                            plugin.addon_name,
                            sanitize_log_path(&plugin.plugin_path),
                            plugin.file_hash,
                            installed_hash.is_some(),
                            installed_hash == Some(&plugin.file_hash),
                            ts3_lookup.search_name,
                            ts3_lookup.expected_files.len(),
                            ts3_lookup.checked_dirs.len(),
                            ts3_lookup.existing_dirs.len(),
                            ts3_lookup.matched_files.len(),
                            ts3_lookup.missing_files.len(),
                            ts3_lookup.hash_mismatched_files.len(),
                            ts3_lookup.is_installed,
                            ts3_lookup.is_up_to_date
                        );
                        if installed_hash != Some(&plugin.file_hash) && ts3_lookup.is_up_to_date {
                            self.mark_ts3_plugin_installed(&path_key, &plugin.file_hash);
                        }
                    }
                    self.ts3_plugin_cache = Some(plugins);
                    self.ts3_running_cache = Some(ts3_running);
                    self.ts3_plugin_scan_rx = None;
                    self.ts3_plugin_scanning = false;
                    return true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    warn!("TS3 plugin settings scan worker disconnected before sending results");
                    // Worker died without sending - treat as empty result.
                    self.ts3_plugin_cache = Some(Vec::new());
                    self.ts3_running_cache = Some(false);
                    self.ts3_plugin_scan_rx = None;
                    self.ts3_plugin_scanning = false;
                }
            }
        }
        false
    }

    pub(super) fn render_ts3_plugins_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;

        // Kick off background scan if we have no cache and no scan in flight.
        if self.ts3_plugin_cache.is_none() && !self.ts3_plugin_scanning {
            self.start_ts3_plugin_scan();
        }

        // Check for results from the background thread.
        self.poll_ts3_plugin_scan();

        let ts3_running = self.ts3_running_cache.unwrap_or(false);

        ui.vertical(|ui| {
            // Info banner + recheck button row
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                let width = (ui.available_width() - horizontal_padding - 90.0).max(0.0);
                ui.add_sized(
                    Vec2::new(width, 0.0),
                    Label::new(
                        RichText::new(format!(
                            "{} {}",
                            '\u{2139}',
                            tr("TS3 plugins are TeamSpeak 3 plugin files found inside your repository addons. Installing opens the plugin with TeamSpeak, which must be closed first.")
                        ))
                        .italics()
                        .color(self.color_text_dim()),
                    )
                    .wrap(),
                );

                let recheck_btn = ui.add_enabled(
                    !self.ts3_plugin_scanning,
                    Button::new(RichText::new(format!("\u{1F504} {}", tr("Recheck"))))
                        .fill(self.color_widget_bg()),
                );
                if recheck_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if recheck_btn.clicked() {
                    info!("TS3 plugin recheck requested from settings tab");
                    self.invalidate_ts3_plugin_cache();
                    self.start_ts3_plugin_scan();
                }
                ui.add_space(horizontal_padding);
            });
            ui.separator();

            if ts3_running {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(
                        RichText::new(format!(
                            "\u{26A0} {}",
                            tr("TeamSpeak 3 is currently running. Please close it before installing plugins.")
                        ))
                        .color(self.color_warn()),
                    );
                    ui.add_space(horizontal_padding);
                });
                ui.separator();
            }

            // Show spinner while scanning
            if self.ts3_plugin_scanning {
                ui.vertical_centered(|ui| {
                    ui.add_space(30.0);
                    ui.add(egui::Spinner::new().size(24.0));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(tr("Scanning repositories for TS3 plugins..."))
                            .color(self.color_text_dim()),
                    );
                    ui.add_space(30.0);
                });
                // Request repaint so we pick up the result next frame.
                ui.ctx().request_repaint();
                return;
            }

            let plugins = match &self.ts3_plugin_cache {
                Some(p) => p.clone(),
                None => return,
            };

            if plugins.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(
                        RichText::new(tr(
                            "No TS3 plugins found in your repositories.",
                        ))
                        .color(self.color_text_dim()),
                    );
                    ui.add_space(horizontal_padding);
                });
                return;
            }

            ScrollArea::vertical().show(ui, |ui| {
                let mut install_actions: Vec<(String, String, std::path::PathBuf)> = Vec::new();

                for plugin in &plugins {
                    let path_key = plugin.plugin_path.display().to_string();
                    let installed_hash = self
                        .settings_view_state
                        .ts3_installed_plugin_hashes
                        .get(&path_key);
                    let is_up_to_date = installed_hash == Some(&plugin.file_hash);

                    ui.horizontal(|ui| {
                        ui.add_space(horizontal_padding);

                        let card_frame = Frame {
                            fill: self.color_card_bg(),
                            stroke: egui::Stroke::new(1.0, self.color_text_gray()),
                            corner_radius: eframe::egui::CornerRadius::same(5),
                            inner_margin: Margin::same(8),
                            ..Default::default()
                        };

                        let card_width = (ui.available_width() - horizontal_padding).max(0.0);
                        ui.scope(|ui| {
                            ui.set_width(card_width);
                            ui.set_max_width(card_width);
                            card_frame.show(ui, |ui| {
                                let content_width = ui.available_width().max(0.0);
                            ui.vertical(|ui| {
                                ui.add_sized(
                                    Vec2::new(content_width, 0.0),
                                    Label::new(
                                        RichText::new(&plugin.addon_name)
                                            .color(self.color_text_normal())
                                            .strong(),
                                    ),
                                );

                                ui.add_sized(
                                    Vec2::new(content_width, 0.0),
                                    Label::new(
                                        RichText::new(&path_key)
                                            .color(self.color_text_dim())
                                            .small(),
                                    )
                                    .wrap(),
                                );

                                ui.add_space(4.0);

                                ui.horizontal(|ui| {
                                    if is_up_to_date {
                                        ui.label(
                                            RichText::new(format!(
                                                "\u{2714} {}",
                                                tr("Up to date")
                                            ))
                                            .color(self.color_success()),
                                        );
                                    } else {
                                        let status_text = if installed_hash.is_some() {
                                            tr("Update available")
                                        } else {
                                            tr("Not installed")
                                        };
                                        ui.label(
                                            RichText::new(status_text)
                                                .color(self.color_warn()),
                                        );
                                    }

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let install_button = ui.add_enabled(
                                                !ts3_running,
                                                Button::new(if is_up_to_date {
                                                    tr("Reinstall")
                                                } else {
                                                    tr("Install")
                                                })
                                                .fill(self.color_widget_bg()),
                                            );

                                            if install_button.hovered() {
                                                ui.ctx().output_mut(
                                                    Foxy::set_pointing_cursor_output,
                                                );
                                            }

                                            if install_button.clicked() {
                                                info!(
                                                    "TS3 plugin install requested from settings tab: addon={} path={} detected_hash={} was_up_to_date={} ts3_running={}",
                                                    plugin.addon_name,
                                                    sanitize_log_path(&plugin.plugin_path),
                                                    plugin.file_hash,
                                                    is_up_to_date,
                                                    ts3_running
                                                );
                                                install_actions.push((
                                                    path_key.clone(),
                                                    plugin.file_hash.clone(),
                                                    plugin.plugin_path.clone(),
                                                ));
                                            }
                                        },
                                    );
                                });
                            });
                            });
                        });
                    });

                    ui.add_space(8.0);
                }

                // Open the plugin file but keep the cached list as-is.
                // The status will refresh on the next Recheck or tab re-entry.
                for (path_key, hash, plugin_path) in install_actions {
                    match ts3_plugin::open_ts3_plugin(&plugin_path) {
                        Ok(()) => {
                            info!(
                                "Opened TS3 plugin for install from settings tab: path={} hash={}",
                                sanitize_log_path(&plugin_path),
                                hash
                            );
                            self.mark_ts3_plugin_installed(&path_key, &hash);
                            self.show_success_toast(self.t(
                                "TS3 plugin opened for installation.",
                            ));
                        }
                        Err(e) => {
                            warn!(
                                "Failed to open TS3 plugin from settings tab: path={} error={}",
                                sanitize_log_path(&plugin_path),
                                e
                            );
                            self.show_error_toast(self.t(
                                "Failed to open TS3 plugin for installation.",
                            ));
                        }
                    }
                }
            });
        });
    }

    /// Render a banner prompting the user to install/update a TS3 plugin.
    /// Returns `true` if the banner was rendered.
    pub fn render_ts3_plugin_update_banner(&mut self, ui: &mut Ui) -> bool {
        let Some(prompt) = self.ts3_plugin_update_prompt.clone() else {
            return false;
        };

        let horizontal_padding = 10.0;

        let banner_frame = Frame {
            fill: self.color_widget_bg(),
            stroke: egui::Stroke::new(1.0, self.color_warn()),
            corner_radius: eframe::egui::CornerRadius::same(5),
            inner_margin: Margin::same(8),
            ..Default::default()
        };

        banner_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(
                    RichText::new(format!(
                        "\u{1F50A} {}",
                        self.t_fmt(
                            "TS3 plugin updated in {addon}. Install now?",
                            &[("addon", prompt.addon_name.clone())],
                        )
                    ))
                    .color(self.color_text_normal()),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(horizontal_padding);

                    let dismiss_button =
                        ui.add(Button::new(tr("Dismiss")).fill(self.color_card_bg()));
                    if dismiss_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if dismiss_button.clicked() {
                        info!(
                            "Dismissed TS3 plugin update prompt: addon={} path={} hash={}",
                            prompt.addon_name,
                            sanitize_log_path(&prompt.plugin_path),
                            prompt.file_hash
                        );
                        self.ts3_plugin_update_prompt = None;
                    }

                    let install_button =
                        ui.add(Button::new(tr("Install")).fill(self.color_widget_bg()));
                    if install_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if install_button.clicked() {
                        let path_key = prompt.plugin_path.display().to_string();
                        info!(
                            "TS3 plugin install requested from update prompt: addon={} path={} hash={}",
                            prompt.addon_name,
                            sanitize_log_path(&prompt.plugin_path),
                            prompt.file_hash
                        );
                        match ts3_plugin::open_ts3_plugin(&prompt.plugin_path) {
                            Ok(()) => {
                                info!(
                                    "Opened TS3 plugin for install from update prompt: path={} hash={}",
                                    sanitize_log_path(&prompt.plugin_path),
                                    prompt.file_hash
                                );
                                self.mark_ts3_plugin_installed(&path_key, &prompt.file_hash);
                                self.show_success_toast(
                                    self.t("TS3 plugin opened for installation."),
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to open TS3 plugin from update prompt: path={} error={}",
                                    sanitize_log_path(&prompt.plugin_path),
                                    e
                                );
                                self.show_error_toast(
                                    self.t("Failed to open TS3 plugin for installation."),
                                );
                            }
                        }
                        self.ts3_plugin_update_prompt = None;
                    }
                });
            });
        });

        true
    }
}
