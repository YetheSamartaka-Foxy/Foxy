use crate::core::ts3_plugin;
use crate::core::utils::format::{sanitize_log_path, sanitize_log_path_str};
use crate::ui::app::{Foxy, Ts3PluginScanResult, Ts3PluginUpdatePrompt};
use crate::ui::i18n::tr;
use crate::ui::types::Ts3PluginStatusRecord;
use eframe::egui::{self, Button, Frame, Label, Margin, RichText, ScrollArea, Ui, Vec2};
use log::{info, warn};

/// How a plugin is presented in the settings tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ts3PluginRowStatus {
    UpToDate,
    UpdateAvailable,
    /// Opened for installation through Foxy, but TeamSpeak has not applied it yet.
    InstallPending,
    NotInstalled,
}

/// One rendered plugin entry, built either from the live scan or from the
/// persisted state of the last verified check.
#[derive(Debug, Clone)]
struct Ts3PluginRow {
    addon_name: String,
    path_key: String,
    plugin_path: std::path::PathBuf,
    file_hash: String,
    status: Ts3PluginRowStatus,
    /// False when the row comes from persisted state rather than this session's scan.
    verified_now: bool,
}

fn row_status(
    is_installed: bool,
    is_up_to_date: bool,
    foxy_hash_matches: bool,
) -> Ts3PluginRowStatus {
    if is_up_to_date {
        Ts3PluginRowStatus::UpToDate
    } else if is_installed {
        Ts3PluginRowStatus::UpdateAvailable
    } else if foxy_hash_matches {
        Ts3PluginRowStatus::InstallPending
    } else {
        Ts3PluginRowStatus::NotInstalled
    }
}

impl Foxy {
    /// Kick off a background thread that scans repositories for TS3 plugins,
    /// verifies them against the TeamSpeak 3 client, and checks whether
    /// TeamSpeak is running. Results arrive via channel and are polled every
    /// frame, so no part of this runs on the UI thread.
    pub(crate) fn start_ts3_plugin_scan(&mut self, reason: &str, prompt_on_update: bool) {
        if self.ts3_plugin_scanning {
            self.ts3_plugin_scan_prompt_on_update |= prompt_on_update;
            self.ts3_plugin_scan_requeued = true;
            info!(
                "Deferred TS3 plugin scan because one is already running: reason={} prompt_on_update={}",
                reason, prompt_on_update
            );
            return;
        }

        let repo_paths: Vec<String> = self
            .repository_view_state
            .repositories
            .iter()
            .map(|r| r.path.clone())
            .filter(|p| !p.is_empty())
            .collect();

        info!(
            "Starting background TS3 plugin scan: reason={} repository_count={} tracked_installed_plugins={} persisted_statuses={} prompt_on_update={}",
            reason,
            repo_paths.len(),
            self.settings_view_state.ts3_installed_plugin_hashes.len(),
            self.settings_view_state.ts3_plugin_statuses.len(),
            prompt_on_update
        );
        let (tx, rx) = std::sync::mpsc::channel();
        self.ts3_plugin_scan_rx = Some(rx);
        self.ts3_plugin_scanning = true;
        self.ts3_plugin_scan_prompt_on_update = prompt_on_update;
        self.ts3_plugin_scan_requeued = false;

        std::thread::spawn(move || {
            let statuses = ts3_plugin::resolve_ts3_plugin_statuses(&repo_paths);
            let ts3_running = ts3_plugin::is_teamspeak_running();
            let plugin_count = statuses.len();
            if tx
                .send(Ts3PluginScanResult {
                    statuses,
                    ts3_running,
                    prompt_on_update,
                })
                .is_err()
            {
                warn!(
                    "Failed to deliver TS3 plugin scan result: plugin_count={} ts3_running={}",
                    plugin_count, ts3_running
                );
            }
        });
    }

    /// Verify the persisted TS3 plugin state once per launch, in the background,
    /// so the game space settings tab opens on a fresh status instead of an
    /// empty list. Only Arma 3 spaces expose the tab.
    pub(crate) fn start_startup_ts3_plugin_scan(&mut self) {
        let active = crate::core::game::spaces::active_game_space();
        if active.game_id != crate::core::game::arma3::ARMA3_GAME_ID {
            return;
        }
        if self.ts3_plugin_cache.is_some() || self.ts3_plugin_scanning {
            return;
        }
        self.start_ts3_plugin_scan("startup verification", false);
    }

    /// Poll the background scan channel. Safe to call every frame from anywhere.
    pub(crate) fn poll_ts3_plugin_scan(&mut self) {
        let Some(rx) = &self.ts3_plugin_scan_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                info!(
                    "Received TS3 plugin scan result: plugin_count={} ts3_running={} prompt_on_update={}",
                    result.statuses.len(),
                    result.ts3_running,
                    result.prompt_on_update
                );
                self.apply_ts3_plugin_scan_result(result);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                warn!("TS3 plugin scan worker disconnected before sending results");
                // Worker died without sending - keep the persisted state and
                // stop showing the tab as busy.
                self.ts3_plugin_scan_rx = None;
                self.ts3_plugin_scanning = false;
                self.ts3_plugin_scan_prompt_on_update = false;
            }
        }
    }

    fn apply_ts3_plugin_scan_result(&mut self, result: Ts3PluginScanResult) {
        let Ts3PluginScanResult {
            statuses,
            ts3_running,
            prompt_on_update,
        } = result;
        let mut prompt: Option<Ts3PluginUpdatePrompt> = None;
        let mut records = Vec::with_capacity(statuses.len());
        let mut settings_changed = false;

        for status in &statuses {
            let plugin = &status.info;
            let path_key = plugin.plugin_path.display().to_string();
            let had_installed_hash = self
                .settings_view_state
                .ts3_installed_plugin_hashes
                .contains_key(&path_key);
            let foxy_hash_matches = self
                .settings_view_state
                .ts3_installed_plugin_hashes
                .get(&path_key)
                == Some(&plugin.file_hash);
            info!(
                "Evaluated TS3 plugin install state: addon={} path={} detected_hash={} foxy_stored_hash_present={} foxy_stored_hash_matches={} ts3_installed={} ts3_up_to_date={}",
                plugin.addon_name,
                sanitize_log_path(&plugin.plugin_path),
                plugin.file_hash,
                had_installed_hash,
                foxy_hash_matches,
                status.is_installed,
                status.is_up_to_date
            );

            records.push(Ts3PluginStatusRecord {
                plugin_path: path_key.clone(),
                addon_name: plugin.addon_name.clone(),
                package_hash: plugin.file_hash.clone(),
                is_installed: status.is_installed,
                is_up_to_date: status.is_up_to_date,
            });

            if !foxy_hash_matches && status.is_up_to_date {
                self.settings_view_state
                    .ts3_installed_plugin_hashes
                    .insert(path_key.clone(), plugin.file_hash.clone());
                settings_changed = true;
                info!(
                    "Recorded verified TS3 plugin install: path={} hash={}",
                    sanitize_log_path_str(&path_key),
                    plugin.file_hash
                );
            }

            // Only prompt when the plugin was previously installed through the
            // app or detected in TeamSpeak, and the local package differs from
            // that installed copy.
            if prompt_on_update
                && prompt.is_none()
                && (had_installed_hash || status.is_installed)
                && !status.is_up_to_date
            {
                info!(
                    "TS3 plugin update detected: addon={} path={} current_hash={}",
                    plugin.addon_name,
                    sanitize_log_path(&plugin.plugin_path),
                    plugin.file_hash
                );
                prompt = Some(Ts3PluginUpdatePrompt {
                    plugin_path: plugin.plugin_path.clone(),
                    addon_name: plugin.addon_name.clone(),
                    file_hash: plugin.file_hash.clone(),
                });
            }
        }

        if prompt.is_some() {
            self.ts3_plugin_update_prompt = prompt;
        }
        if self.settings_view_state.ts3_plugin_statuses != records {
            self.settings_view_state.ts3_plugin_statuses = records;
            settings_changed = true;
        }
        if settings_changed {
            self.save_settings();
        }

        self.ts3_plugin_cache = Some(statuses);
        self.ts3_running_cache = Some(ts3_running);
        self.ts3_plugin_scan_rx = None;
        self.ts3_plugin_scanning = false;
        self.ts3_plugin_scan_prompt_on_update = false;

        if self.ts3_plugin_scan_requeued {
            self.ts3_plugin_scan_requeued = false;
            self.start_ts3_plugin_scan("requeued while a scan was running", false);
        }
    }

    /// Build the rows to render, preferring this session's verified scan and
    /// falling back to the persisted state of the last check.
    fn ts3_plugin_rows(&self) -> Vec<Ts3PluginRow> {
        let hashes = &self.settings_view_state.ts3_installed_plugin_hashes;
        if let Some(statuses) = &self.ts3_plugin_cache {
            return statuses
                .iter()
                .map(|status| {
                    let path_key = status.info.plugin_path.display().to_string();
                    let foxy_hash_matches = hashes.get(&path_key) == Some(&status.info.file_hash);
                    Ts3PluginRow {
                        addon_name: status.info.addon_name.clone(),
                        path_key,
                        plugin_path: status.info.plugin_path.clone(),
                        file_hash: status.info.file_hash.clone(),
                        status: row_status(
                            status.is_installed,
                            status.is_up_to_date,
                            foxy_hash_matches,
                        ),
                        verified_now: true,
                    }
                })
                .collect();
        }

        self.settings_view_state
            .ts3_plugin_statuses
            .iter()
            .map(|record| {
                let foxy_hash_matches =
                    hashes.get(&record.plugin_path) == Some(&record.package_hash);
                Ts3PluginRow {
                    addon_name: record.addon_name.clone(),
                    path_key: record.plugin_path.clone(),
                    plugin_path: std::path::PathBuf::from(&record.plugin_path),
                    file_hash: record.package_hash.clone(),
                    status: row_status(
                        record.is_installed,
                        record.is_up_to_date,
                        foxy_hash_matches,
                    ),
                    verified_now: false,
                }
            })
            .collect()
    }

    pub(crate) fn render_ts3_plugins_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;

        // Refresh once per tab entry, but never block on it: the rows below
        // render from persisted state until the background result lands.
        if self.ts3_plugin_cache.is_none() && !self.ts3_plugin_scanning {
            self.start_ts3_plugin_scan("game space settings TS3 tab opened", false);
        }

        let rows = self.ts3_plugin_rows();
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
                    self.start_ts3_plugin_scan("manual recheck from settings tab", false);
                }
                ui.add_space(horizontal_padding);
            });

            if self.ts3_plugin_scanning {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(
                        RichText::new(tr("Checking TS3 plugins in the background..."))
                            .italics()
                            .small()
                            .color(self.color_text_dim()),
                    );
                });
                // Request repaint so we pick up the result next frame.
                ui.ctx().request_repaint();
            } else if !rows.is_empty() && rows.iter().all(|row| !row.verified_now) {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(
                        RichText::new(tr("Showing the last known state."))
                            .italics()
                            .small()
                            .color(self.color_text_dim()),
                    );
                });
            }
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

            if rows.is_empty() {
                // Nothing known yet only means "scanning" on a first-ever run.
                if !self.ts3_plugin_scanning {
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
                }
                return;
            }

            ScrollArea::vertical().show(ui, |ui| {
                let mut install_actions: Vec<(String, String, std::path::PathBuf)> = Vec::new();

                for plugin in &rows {
                    let path_key = plugin.path_key.clone();
                    let is_up_to_date = plugin.status == Ts3PluginRowStatus::UpToDate;

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
                                    match plugin.status {
                                        Ts3PluginRowStatus::UpToDate => {
                                            ui.label(
                                                RichText::new(format!(
                                                    "\u{2714} {}",
                                                    tr("Up to date")
                                                ))
                                                .color(self.color_success()),
                                            );
                                        }
                                        Ts3PluginRowStatus::UpdateAvailable => {
                                            ui.label(
                                                RichText::new(tr("Update available"))
                                                    .color(self.color_warn()),
                                            );
                                        }
                                        Ts3PluginRowStatus::InstallPending => {
                                            ui.label(
                                                RichText::new(tr(
                                                    "Waiting for TeamSpeak to finish installing",
                                                ))
                                                .color(self.color_warn()),
                                            );
                                        }
                                        Ts3PluginRowStatus::NotInstalled => {
                                            ui.label(
                                                RichText::new(tr("Not installed"))
                                                    .color(self.color_warn()),
                                            );
                                        }
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

                // TeamSpeak applies the package through its own dialog, so the
                // verified state only changes after the user finishes there.
                // Keep the list as-is; Recheck or the next tab entry confirms it.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_teamspeak_state_wins_over_foxy_bookkeeping() {
        // Installed and current in TeamSpeak, even when Foxy never recorded it
        // (for example a plugin installed before Foxy, or on a fresh config).
        assert_eq!(row_status(true, true, false), Ts3PluginRowStatus::UpToDate);
        // Foxy thinks it installed the current package, but TeamSpeak has an
        // older payload: the real state is what the user must act on.
        assert_eq!(
            row_status(true, false, true),
            Ts3PluginRowStatus::UpdateAvailable
        );
    }

    #[test]
    fn foxy_hash_marks_install_as_pending_until_teamspeak_applies_it() {
        assert_eq!(
            row_status(false, false, true),
            Ts3PluginRowStatus::InstallPending
        );
        assert_eq!(
            row_status(false, false, false),
            Ts3PluginRowStatus::NotInstalled
        );
    }
}
