use super::arma3_editor_display_name;
use crate::ui::app::{
    Foxy, JoinPreflightAddonOrigin, JoinPreflightKnownRemoteAddon, JoinPreflightMatchConfidence,
    PendingJoinPreflightState, PendingRepositoryDuplicateAddAction, RepositoryContextConfirmAction,
    RepositorySpaceImportContinuation,
};
use crate::ui::types::{RepoState, RepositorySpaceBulkMode};
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align, Align2, Button, CornerRadius, CursorIcon, Frame, Layout, Margin, RichText,
    ScrollArea, TextEdit, Ui,
};
use log::{info, warn};

/// How often the preflight modal re-checks whether TeamSpeak or Steam started.
const PRELAUNCH_RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1500);

impl Foxy {
    pub(super) fn render_join_preflight_modal(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.pending_join_preflight.clone() else {
            return;
        };

        let mut launch_with_suggestions = false;
        let mut launch_without_suggestions = false;
        let mut launch_ts3 = false;
        let mut launch_steam = false;
        let mut cancel = false;
        let mut pending_changed = false;
        let mut remote_download_request = None;

        let has_addon_actions = !pending.suggestions.is_empty()
            || !pending.ambiguous.is_empty()
            || !pending.known_remote.is_empty()
            || !pending.extra_enabled.is_empty();

        let ts3_attention = pending.ts3_required && !pending.ts3_running;
        let steam_attention = pending.steam_required && !pending.steam_running;

        // Throttled re-check so the TeamSpeak/Steam warnings clear automatically
        // once the respective client starts after the user clicks "Launch ...".
        if ts3_attention || steam_attention {
            let now = std::time::Instant::now();
            let due = self
                .prelaunch_recheck_at
                .is_none_or(|at| now.duration_since(at) >= PRELAUNCH_RECHECK_INTERVAL);
            if due {
                self.prelaunch_recheck_at = Some(now);
                if ts3_attention && crate::core::ts3_plugin::is_teamspeak_running() {
                    pending.ts3_running = true;
                    pending_changed = true;
                    info!(
                        "TeamSpeak detected running while preflight modal open for repository {}; clearing warning",
                        pending.repo_name
                    );
                }
                if steam_attention && crate::core::steam::is_steam_running() {
                    pending.steam_running = true;
                    pending_changed = true;
                    info!(
                        "Steam detected running while preflight modal open for repository {}; clearing warning",
                        pending.repo_name
                    );
                }
            }
            ctx.request_repaint_after(PRELAUNCH_RECHECK_INTERVAL);
        }

        let title = if !has_addon_actions && steam_attention {
            self.t("Steam is not running")
        } else if !has_addon_actions && ts3_attention {
            self.t("TeamSpeak 3 is not running")
        } else if !has_addon_actions {
            // Opened solely to warn about enabled addons that can't be found.
            self.t("Some enabled addons are missing")
        } else if pending.extra_enabled.is_empty()
            || !pending.suggestions.is_empty()
            || !pending.ambiguous.is_empty()
            || !pending.known_remote.is_empty()
        {
            self.t("Enable server-required addons?")
        } else {
            self.t("Enabled additional/external addons")
        };

        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(760.0)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(self.color_main_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(RichText::new(&pending.repo_name).strong());
                        if !pending.suggestions.is_empty() || !pending.ambiguous.is_empty() {
                            ui.label(self.t_fmt(
                                "The server reports addons that are available locally but disabled for this launch.",
                                &[],
                            ));
                        }
                        if !pending.extra_enabled.is_empty() {
                            ui.label(self.t("Enabled additional/external addons"));
                        }
                    });

                if pending.steam_required && !pending.steam_running {
                    ui.add_space(10.0);
                    self.join_preflight_section(ui, |ui| {
                        ui.label(
                            RichText::new(self.t("Steam is not running"))
                                .strong()
                                .color(self.color_warn()),
                        );
                        ui.add_space(4.0);
                        ui.label(self.t(
                            "Steam must be running to launch Arma 3. Start Steam before launching.",
                        ));
                        ui.add_space(8.0);
                        let launch_steam_btn = ui.add(
                            Button::new(
                                RichText::new(format!("\u{1F6F0}  {}", self.t("Launch Steam")))
                                    .strong()
                                    .color(self.color_text_normal()),
                            )
                            .fill(self.color_widget_bg_active())
                            .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                            .corner_radius(CornerRadius::same(6))
                            .min_size(egui::Vec2::new(0.0, 30.0)),
                        );
                        if launch_steam_btn.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if launch_steam_btn.clicked() {
                            launch_steam = true;
                        }
                    });
                }

                if pending.ts3_required && !pending.ts3_running {
                    ui.add_space(10.0);
                    self.join_preflight_section(ui, |ui| {
                        ui.label(
                            RichText::new(self.t("TeamSpeak 3 is not running"))
                                .strong()
                                .color(self.color_warn()),
                        );
                        ui.add_space(4.0);
                        ui.label(self.t(
                            "This repository includes TeamSpeak plugins (e.g. radio mods). Start TeamSpeak 3 before joining.",
                        ));
                        ui.add_space(8.0);
                        let launch_ts3_btn = ui.add(
                            Button::new(
                                RichText::new(format!(
                                    "\u{1F3A7}  {}",
                                    self.t("Launch TeamSpeak")
                                ))
                                .strong()
                                .color(self.color_text_normal()),
                            )
                            .fill(self.color_widget_bg_active())
                            .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                            .corner_radius(CornerRadius::same(6))
                            .min_size(egui::Vec2::new(0.0, 30.0)),
                        );
                        if launch_ts3_btn.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if launch_ts3_btn.clicked() {
                            launch_ts3 = true;
                        }
                    });
                }

                if !pending.suggestions.is_empty() {
                    ui.add_space(10.0);
                    self.join_preflight_section(ui, |ui| {
                        ui.label(RichText::new(self.t("Addons to enable for this join")).strong());
                        ui.add_space(6.0);
                        ScrollArea::vertical()
                            .id_salt((
                                "join_preflight_addons",
                                &pending.server.address,
                                &pending.server.port,
                            ))
                            .max_height(180.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for suggestion in &mut pending.suggestions {
                                    self.join_preflight_row(ui, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            let before = suggestion.selected;
                                            let checkbox = Self::ui_state_checkbox(
                                                ui,
                                                &mut suggestion.selected,
                                                "",
                                            );
                                            if checkbox.hovered() {
                                                ui.ctx()
                                                    .output_mut(Foxy::set_pointing_cursor_output);
                                            }
                                            if suggestion.selected != before {
                                                pending_changed = true;
                                            }
                                            ui.label(
                                                RichText::new(&suggestion.addon_name).strong(),
                                            );
                                            self.join_preflight_badge(
                                                ui,
                                                self.join_preflight_origin_label(
                                                    &suggestion.origin,
                                                ),
                                            );
                                            self.join_preflight_badge(
                                                ui,
                                                self.join_preflight_confidence_label(
                                                    &suggestion.confidence,
                                                ),
                                            );
                                            if suggestion.reported_name != suggestion.addon_name {
                                                ui.label(self.t_fmt(
                                                    "Server name: {name}",
                                                    &[("name", suggestion.reported_name.clone())],
                                                ));
                                            }
                                        });
                                    });
                                    ui.add_space(4.0);
                                }
                            });
                    });
                }

                if !pending.ambiguous.is_empty() {
                    ui.add_space(10.0);
                    self.join_preflight_section(ui, |ui| {
                        ui.label(RichText::new(self.t("Ambiguous local matches")).strong());
                        ui.add_space(6.0);
                        for ambiguous in &mut pending.ambiguous {
                            self.join_preflight_row(ui, |ui| {
                                ui.label(RichText::new(&ambiguous.reported_name).strong());
                                ui.add_space(4.0);
                                for (candidate_idx, candidate) in
                                    ambiguous.candidates.iter().enumerate()
                                {
                                    ui.horizontal_wrapped(|ui| {
                                        let before = ambiguous.selected_candidate;
                                        let response = ui.radio_value(
                                            &mut ambiguous.selected_candidate,
                                            Some(candidate_idx),
                                            &candidate.addon_name,
                                        );
                                        if response.hovered() {
                                            ui.ctx()
                                                .output_mut(Foxy::set_pointing_cursor_output);
                                        }
                                        if ambiguous.selected_candidate != before {
                                            pending_changed = true;
                                            info!(
                                                "Selected ambiguous join preflight candidate {} for requirement {}",
                                                candidate.addon_name,
                                                ambiguous.reported_name
                                            );
                                        }
                                        self.join_preflight_badge(
                                            ui,
                                            self.join_preflight_origin_label(&candidate.origin),
                                        );
                                        self.join_preflight_badge(
                                            ui,
                                            self.join_preflight_confidence_label(
                                                &candidate.confidence,
                                            ),
                                        );
                                    });
                                }
                            });
                            ui.add_space(4.0);
                        }
                    });
                }

                if !pending.known_remote.is_empty() {
                    ui.add_space(10.0);
                    self.join_preflight_section(ui, |ui| {
                        ui.label(
                            RichText::new(
                                if pending.known_remote.iter().all(|remote| remote.available) {
                                    self.t("Addons to enable for this join")
                                } else {
                                    self.t("Missing addons available from other repositories")
                                },
                            )
                            .strong(),
                        );
                        ui.add_space(6.0);
                        for remote in &mut pending.known_remote {
                            self.join_preflight_row(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let before = remote.selected;
                                    let checkbox =
                                        Self::ui_state_checkbox(ui, &mut remote.selected, "");
                                    if checkbox.hovered() {
                                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if remote.selected != before {
                                        pending_changed = true;
                                        info!(
                                            "Toggled known-remote join preflight addon {} from repository {} (url={}) to selected={}",
                                            remote.addon_name,
                                            remote.repository_name,
                                            remote.repository_url,
                                            remote.selected
                                        );
                                    }
                                    ui.label(RichText::new(&remote.reported_name).strong());
                                    ui.label(self.t_fmt(
                                        "Source repository: {name}",
                                        &[("name", remote.repository_name.clone())],
                                    ));
                                    self.join_preflight_badge(
                                        ui,
                                        self.join_preflight_confidence_label(&remote.confidence),
                                    );
                                    if !remote.available {
                                        let download_btn = ui.button(self.t("Standalone download"));
                                        if download_btn.hovered() {
                                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                        }
                                        if download_btn.clicked() {
                                            remote_download_request = Some(remote.clone());
                                        }
                                    }
                                });
                            });
                            ui.add_space(4.0);
                        }
                    });
                }

                if !pending.extra_enabled.is_empty() {
                    ui.add_space(10.0);
                    self.join_preflight_section(ui, |ui| {
                        ui.label(
                            RichText::new(self.t("Enabled additional/external addons")).strong(),
                        );
                        ui.add_space(6.0);
                        ScrollArea::vertical()
                            .id_salt((
                                "join_preflight_extra_enabled",
                                &pending.server.address,
                                &pending.server.port,
                            ))
                            .max_height(140.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for extra in &mut pending.extra_enabled {
                                    self.join_preflight_row(ui, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            let before = extra.selected;
                                            let checkbox = Self::ui_state_checkbox(
                                                ui,
                                                &mut extra.selected,
                                                "",
                                            );
                                            if checkbox.hovered() {
                                                ui.ctx()
                                                    .output_mut(Foxy::set_pointing_cursor_output);
                                            }
                                            if extra.selected != before {
                                                pending_changed = true;
                                            }
                                            ui.label(RichText::new(&extra.addon_name).strong());
                                            self.join_preflight_badge(
                                                ui,
                                                self.join_preflight_origin_label(&extra.origin),
                                            );
                                        });
                                    });
                                    ui.add_space(4.0);
                                }
                            });
                    });
                }

                if !pending.unavailable_enabled.is_empty() {
                    ui.add_space(10.0);
                    self.join_preflight_section(ui, |ui| {
                        ui.label(
                            RichText::new(self.t("Enabled addons that could not be found"))
                                .strong()
                                .color(self.color_warn()),
                        );
                        ui.add_space(4.0);
                        ui.label(self.t(
                            "These addons are enabled but their files are missing, so Arma 3 will not load them. Fix or re-add them in the repository's External Addons settings.",
                        ));
                        ui.add_space(6.0);
                        ScrollArea::vertical()
                            .id_salt((
                                "join_preflight_unavailable_enabled",
                                &pending.server.address,
                                &pending.server.port,
                            ))
                            .max_height(140.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for unavailable in &pending.unavailable_enabled {
                                    self.join_preflight_row(ui, |ui| {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new(&unavailable.addon_name).strong(),
                                            );
                                            if !unavailable.path.trim().is_empty() {
                                                ui.label(
                                                    RichText::new(&unavailable.path)
                                                        .color(self.color_text_dim()),
                                                );
                                            }
                                        });
                                    });
                                    ui.add_space(4.0);
                                }
                            });
                    });
                }

                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    let selected_local_suggestions =
                        pending.suggestions.iter().any(|suggestion| suggestion.selected);
                    let selected_ambiguous_suggestions = pending
                        .ambiguous
                        .iter()
                        .any(|ambiguous| ambiguous.selected_candidate.is_some());
                    // Extra enabled addons are always actionable on the "launch
                    // with selected addons" path: a ticked entry is kept loaded
                    // and an unticked one is stripped, so either tick state is a
                    // meaningful selection.
                    let has_extra_enabled = !pending.extra_enabled.is_empty();
                    let selected_known_remote =
                        pending.known_remote.iter().any(|remote| remote.selected);
                    let selected_missing_known_remote = pending
                        .known_remote
                        .iter()
                        .any(|remote| remote.selected && !remote.available);
                    if !pending.suggestions.is_empty()
                        || selected_ambiguous_suggestions
                        || !pending.known_remote.is_empty()
                        || !pending.extra_enabled.is_empty()
                    {
                        let has_selected_addons = selected_local_suggestions
                            || selected_ambiguous_suggestions
                            || selected_known_remote
                            || has_extra_enabled;
                        let action_label = if selected_missing_known_remote {
                            self.t("Standalone download")
                        } else {
                            self.t("Launch with selected addons")
                        };
                        let launch_with_btn = ui.add_enabled(
                            has_selected_addons,
                            Button::new(action_label),
                        );
                        if launch_with_btn.hovered() && has_selected_addons {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if launch_with_btn.clicked() {
                            launch_with_suggestions = true;
                        }
                    }

                    // With no addon actions this is a TeamSpeak/Steam-only modal,
                    // so the button just proceeds: "Launch" for a plain launch,
                    // "Join" for a server join, rather than "Launch without ...".
                    let launch_without_label = if has_addon_actions {
                        self.t("Launch without suggested addons")
                    } else if pending.launch_only {
                        self.t("Launch")
                    } else {
                        self.t("Join")
                    };
                    let launch_without_btn = ui.button(launch_without_label);
                    if launch_without_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if launch_without_btn.clicked() {
                        launch_without_suggestions = true;
                    }

                    let cancel_btn = ui.button(self.t("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_join_preflight = None;
            self.prelaunch_recheck_at = None;
            return;
        }

        if launch_steam {
            let steam_dir = self.settings_view_state.steam_directory.clone();
            match crate::core::steam::launch_steam(&steam_dir) {
                Ok(()) => {
                    info!(
                        "Launched Steam from preflight modal for repository {}",
                        pending.repo_name
                    );
                    self.show_success_toast(self.t("Launching Steam…"));
                }
                Err(err) => {
                    warn!("Failed to launch Steam from preflight modal: {}", err);
                    self.show_error_toast(self.t(&err));
                }
            }
            // Recheck promptly on the next frame so the warning clears once
            // Steam finishes starting.
            self.prelaunch_recheck_at = None;
            self.pending_join_preflight = Some(pending);
            return;
        }

        if launch_ts3 {
            let ts3_dir = self.settings_view_state.teamspeak3_directory.clone();
            match crate::core::ts3_plugin::launch_teamspeak(&ts3_dir) {
                Ok(()) => {
                    info!(
                        "Launched TeamSpeak 3 from join preflight modal for repository {}",
                        pending.repo_name
                    );
                    self.show_success_toast(self.t("Launching TeamSpeak 3…"));
                }
                Err(err) => {
                    warn!(
                        "Failed to launch TeamSpeak 3 from join preflight modal: {}",
                        err
                    );
                    self.show_error_toast(self.t(&err));
                }
            }
            // Recheck promptly on the next frame so the warning clears once the
            // client finishes starting.
            self.prelaunch_recheck_at = None;
            self.pending_join_preflight = Some(pending);
            return;
        }

        if let Some(remote) = remote_download_request {
            if self.start_join_preflight_remote_download(&pending, &remote) {
                self.pending_join_preflight = None;
            } else {
                self.pending_join_preflight = Some(pending);
            }
            return;
        }

        if launch_with_suggestions {
            let selected_missing_known_remote = pending
                .known_remote
                .iter()
                .any(|remote| remote.selected && !remote.available);
            if selected_missing_known_remote {
                if self.start_selected_join_preflight_remote_download(&pending) {
                    self.pending_join_preflight = None;
                } else {
                    self.pending_join_preflight = Some(pending);
                }
                return;
            }

            self.pending_join_preflight = None;
            let selected_repository = Self::repository_with_join_preflight_selections(&pending);
            let result = self.launch_repository_with_steam_guard(
                &selected_repository,
                Some(&pending.server),
                &pending.repo_name,
                "join",
            );
            if result == super::LaunchDispatchResult::Launched {
                self.handle_post_launch_window_behavior(ctx, "join launch completed");
            }
            return;
        }

        if launch_without_suggestions {
            self.pending_join_preflight = None;
            self.prelaunch_recheck_at = None;
            let launch_repository = Self::repository_without_join_preflight_suggestions(&pending);
            let (server_arg, launch_label) = if pending.launch_only {
                (None, "regular")
            } else {
                (Some(&pending.server), "join")
            };
            let result = self.launch_repository_with_steam_guard(
                &launch_repository,
                server_arg,
                &pending.repo_name,
                launch_label,
            );
            if result == super::LaunchDispatchResult::Launched {
                self.handle_post_launch_window_behavior(ctx, "launch completed");
            }
            return;
        }

        if pending_changed {
            self.pending_join_preflight = Some(pending);
        }
    }

    fn start_selected_join_preflight_remote_download(
        &mut self,
        pending: &PendingJoinPreflightState,
    ) -> bool {
        let selected_remotes = pending
            .known_remote
            .iter()
            .filter(|remote| remote.selected && !remote.available)
            .collect::<Vec<_>>();
        let Some(remote) = selected_remotes.first() else {
            warn!(
                "Join preflight remote download requested for repository {} server {}:{} but no known-remote addon was selected",
                pending.repo_name, pending.server.address, pending.server.port
            );
            return false;
        };

        let normalized_url = Self::normalize_repo_url(&remote.repository_url);
        let addon_names = selected_remotes
            .iter()
            .filter(|selected| Self::normalize_repo_url(&selected.repository_url) == normalized_url)
            .map(|selected| selected.addon_name.clone())
            .collect::<Vec<_>>();
        let skipped_count = selected_remotes.len().saturating_sub(addon_names.len());
        if skipped_count > 0 {
            warn!(
                "Join preflight selected {} known-remote addons from multiple source repositories for repository {} server {}:{}; starting {} addons from source_repository={} and skipping {} addons from other repositories",
                selected_remotes.len(),
                pending.repo_name,
                pending.server.address,
                pending.server.port,
                addon_names.len(),
                remote.repository_name,
                skipped_count
            );
        }

        let Some(repo_idx) = self.repo_index_by_normalized_url(&normalized_url) else {
            warn!(
                "Join preflight remote addon download ignored: source repository no longer exists for addon={} source_repository={} url={}",
                remote.addon_name, remote.repository_name, normalized_url
            );
            return false;
        };

        info!(
            "Starting join preflight remote addon download: requested_repository={} server={}:{} source_repository={} source_url={} addons={:?}",
            pending.repo_name,
            pending.server.address,
            pending.server.port,
            remote.repository_name,
            normalized_url,
            addon_names
        );

        self.standalone_download_addons(repo_idx, &addon_names)
    }

    fn start_join_preflight_remote_download(
        &mut self,
        pending: &PendingJoinPreflightState,
        remote: &JoinPreflightKnownRemoteAddon,
    ) -> bool {
        let normalized_url = Self::normalize_repo_url(&remote.repository_url);
        let Some(repo_idx) = self.repo_index_by_normalized_url(&normalized_url) else {
            warn!(
                "Join preflight remote addon download ignored: source repository no longer exists for addon={} source_repository={} url={}",
                remote.addon_name, remote.repository_name, normalized_url
            );
            return false;
        };

        info!(
            "Starting join preflight remote addon download: requested_repository={} server={}:{} source_repository={} source_url={} addon={} reported_name={}",
            pending.repo_name,
            pending.server.address,
            pending.server.port,
            remote.repository_name,
            normalized_url,
            remote.addon_name,
            remote.reported_name
        );

        self.standalone_download_addon(repo_idx, &remote.addon_name)
    }

    fn join_preflight_section(&self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
        Frame::NONE
            .fill(self.color_widget_bg())
            .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
            .corner_radius(CornerRadius::same(7))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                add_contents(ui);
            });
    }

    fn join_preflight_row(&self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
        Frame::NONE
            .fill(self.color_main_bg())
            .stroke(egui::Stroke::new(1.0, self.color_text_dim()))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                add_contents(ui);
            });
    }

    fn join_preflight_badge(&self, ui: &mut Ui, text: String) {
        Frame::NONE
            .fill(self.color_card_bg())
            .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
            .corner_radius(CornerRadius::same(5))
            .inner_margin(Margin::symmetric(6, 2))
            .show(ui, |ui| {
                ui.label(RichText::new(text).color(self.color_text_gray()));
            });
    }

    fn join_preflight_origin_label(&self, origin: &JoinPreflightAddonOrigin) -> String {
        match origin {
            JoinPreflightAddonOrigin::Required => self.t("Required addon"),
            JoinPreflightAddonOrigin::Optional => self.t("Optional addon"),
            JoinPreflightAddonOrigin::External => self.t("External addon"),
        }
    }

    fn join_preflight_confidence_label(&self, confidence: &JoinPreflightMatchConfidence) -> String {
        match confidence {
            JoinPreflightMatchConfidence::ExactNormalizedName => self.t("Name match"),
        }
    }

    pub(super) fn render_delete_mission_modal(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.pending_mission_delete.clone() else {
            return;
        };

        if self
            .repository_view_state
            .repositories
            .get(pending.repo_idx)
            .is_none()
        {
            self.pending_mission_delete = None;
            return;
        }

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(self.t("Confirm Deletion"))
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(660.0)
            .show(ctx, |ui| {
                ui.label(self.t_fmt(
                    "Are you sure you want to delete {name}?",
                    &[(
                        "name",
                        arma3_editor_display_name(&pending.mission.folder_name),
                    )],
                ));
                if let Some(error) = &pending.error {
                    ui.colored_label(self.color_text_error(), error);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let confirm_btn = ui.add(
                        Button::new(self.t("Delete mission")).fill(self.color_action_destructive()),
                    );
                    if confirm_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_btn.clicked() {
                        confirm = true;
                    }

                    let cancel_btn = ui.button(self.t("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_mission_delete = None;
            return;
        }

        if confirm {
            match self.delete_mission_from_pending(&pending) {
                Ok(_) => return,
                Err(err) => pending.error = Some(err),
            }
        }

        self.pending_mission_delete = Some(pending);
    }

    pub(super) fn render_remove_mission_dependencies_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_mission_remove_dependencies.clone() else {
            return;
        };

        if self
            .repository_view_state
            .repositories
            .get(pending.repo_idx)
            .is_none()
        {
            self.pending_mission_remove_dependencies = None;
            return;
        }

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(self.t("Confirm Dependency Removal"))
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(660.0)
            .show(ctx, |ui| {
                ui.label(self.t_fmt(
                    "Are you sure you want to remove addon dependencies from {name}?",
                    &[(
                        "name",
                        arma3_editor_display_name(&pending.mission.display_name),
                    )],
                ));
                if let Some(error) = &pending.error {
                    ui.colored_label(self.color_text_error(), error);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let confirm_btn = ui.add(
                        Button::new(self.t("Remove dependencies"))
                            .fill(self.color_action_destructive()),
                    );
                    if confirm_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_btn.clicked() {
                        confirm = true;
                    }

                    let cancel_btn = ui.button(self.t("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_mission_remove_dependencies = None;
            return;
        }

        if confirm {
            self.pending_mission_remove_dependencies = None;
            self.remove_mission_dependencies(&pending.mission);
        }
    }

    pub(super) fn render_duplicate_mission_modal(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.pending_mission_duplicate.clone() else {
            return;
        };

        if self
            .repository_view_state
            .repositories
            .get(pending.repo_idx)
            .is_none()
        {
            self.pending_mission_duplicate = None;
            return;
        }

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(self.t("Duplicate mission"))
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(arma3_editor_display_name(&pending.mission.folder_name)).strong(),
                );
                let name_input = ui.add(
                    TextEdit::singleline(&mut pending.name_input)
                        .hint_text(pending.suggested_name.as_str())
                        .desired_width(ui.available_width()),
                );
                if name_input.hovered() {
                    ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                }
                if let Some(error) = &pending.error {
                    ui.colored_label(self.color_text_error(), error);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let confirm_btn = ui.button(self.t("Confirm"));
                    if confirm_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_btn.clicked() {
                        confirm = true;
                    }

                    let cancel_btn = ui.button(self.t("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_mission_duplicate = None;
            return;
        }

        if confirm {
            match self.duplicate_mission_from_pending(&pending) {
                Ok(_) => return,
                Err(err) => pending.error = Some(err),
            }
        }

        self.pending_mission_duplicate = Some(pending);
    }

    pub(super) fn render_editor_mission_external_addons_warning_modal(
        &mut self,
        ctx: &egui::Context,
    ) {
        let Some(pending) = self.pending_mission_editor_launch_warning.clone() else {
            return;
        };

        if self
            .repository_view_state
            .repositories
            .get(pending.repo_idx)
            .is_none()
        {
            self.pending_mission_editor_launch_warning = None;
            return;
        }

        let mut launch_with_addons = false;
        let mut launch_without_external_addons = false;
        let mut cancel = false;

        let title = if pending.external_addons.is_empty() {
            self.t("Some enabled addons are missing")
        } else {
            self.t("Launch editor with external addons?")
        };
        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(720.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(arma3_editor_display_name(&pending.mission.display_name))
                        .strong(),
                );
                if !pending.external_addons.is_empty() {
                    ui.label(self.t(
                        "This mission will open in Eden Editor with additional/external addons enabled. If you save the mission, those addon dependencies may be written into mission.sqm.",
                    ));

                    ui.add_space(10.0);
                    ui.label(RichText::new(self.t("Enabled additional/external addons")).strong());
                    ScrollArea::vertical()
                        .id_salt(("editor_launch_external_addons", pending.repo_idx))
                        .max_height(220.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for addon in &pending.external_addons {
                                ui.label(addon);
                            }
                        });
                }

                if !pending.unavailable_enabled.is_empty() {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(self.t("Enabled addons that could not be found"))
                            .strong()
                            .color(self.color_warn()),
                    );
                    ui.label(self.t(
                        "These addons are enabled but their files are missing, so Arma 3 will not load them. Fix or re-add them in the repository's External Addons settings.",
                    ));
                    ui.add_space(6.0);
                    ScrollArea::vertical()
                        .id_salt(("editor_launch_unavailable_addons", pending.repo_idx))
                        .max_height(160.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for unavailable in &pending.unavailable_enabled {
                                ui.label(RichText::new(&unavailable.addon_name).strong());
                                if !unavailable.path.trim().is_empty() {
                                    ui.label(
                                        RichText::new(&unavailable.path)
                                            .color(self.color_text_dim()),
                                    );
                                }
                            }
                        });
                }

                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    let launch_with_btn = ui.button(self.t("Launch with addons"));
                    if launch_with_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if launch_with_btn.clicked() {
                        launch_with_addons = true;
                    }

                    let launch_without_btn =
                        ui.button(self.t("Launch without additional/external addons"));
                    if launch_without_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if launch_without_btn.clicked() {
                        launch_without_external_addons = true;
                    }

                    let cancel_btn = ui.button(self.t("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_mission_editor_launch_warning = None;
            return;
        }

        if launch_with_addons {
            self.pending_mission_editor_launch_warning = None;
            self.launch_editor_with_mission(
                ctx,
                &pending.effective_repository,
                &pending.mission,
                &pending.repo_name,
            );
            return;
        }

        if launch_without_external_addons {
            self.pending_mission_editor_launch_warning = None;
            let mut effective = pending.effective_repository.clone();
            effective.include_steam_addons = false;
            effective.external_addons.clear();
            self.launch_editor_with_mission(ctx, &effective, &pending.mission, &pending.repo_name);
        }
    }

    pub(super) fn render_add_repository_modal(&mut self, ctx: &egui::Context) {
        if !self.show_add_repository_modal {
            return;
        }

        let mut submit = false;
        let mut close = false;
        egui::Window::new(self.t("Add repository"))
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(self.t("Repository address or repository space address"));
                let input = ui.add(
                    TextEdit::singleline(&mut self.add_repository_input_address)
                        // Stable id so the agent GUI driver's `focus` command can
                        // target this field deterministically (see agent_driver
                        // AGENT_FOCUS_TARGETS).
                        .id(egui::Id::new("agent.add-repository-input"))
                        .desired_width(ui.available_width()),
                );
                if input.hovered() {
                    ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                }

                // Optional overrides applied only when the input resolves to a
                // plain repository (not a repository space). Empty name derives
                // one from the address; empty path leaves the folder unset.
                let optional_hint = self.t("Optional");
                ui.add_space(8.0);
                ui.label(self.t("Name"));
                let name_input = ui.add(
                    TextEdit::singleline(&mut self.add_repository_input_name)
                        .hint_text(optional_hint.clone())
                        .char_limit(100)
                        .desired_width(ui.available_width()),
                );
                if name_input.hovered() {
                    ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                }

                ui.add_space(8.0);
                ui.label(self.t("Local Path"));
                let browse_label = self.t("Browse");
                ui.horizontal(|ui| {
                    let browse_width = 90.0;
                    let path_width = (ui.available_width() - browse_width).max(100.0);
                    let path_input = ui.add(
                        TextEdit::singleline(&mut self.add_repository_input_path)
                            .hint_text(optional_hint)
                            .char_limit(500)
                            .desired_width(path_width),
                    );
                    if path_input.hovered() {
                        ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                    }
                    let browse_btn = ui.add(Button::new(browse_label));
                    if browse_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if browse_btn.clicked()
                        && let Some(dir) = crate::ui::app::agent_support::pick_folder(|| {
                            rfd::FileDialog::new().pick_folder()
                        })
                    {
                        self.add_repository_input_path = dir.display().to_string();
                    }
                });

                if let Some(error) = &self.add_repository_input_error {
                    ui.colored_label(self.color_text_error(), error);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let importing = self.repository_space_import_in_flight;
                    let add_button = ui.add_enabled(!importing, Button::new(self.t("Add")));
                    if add_button.hovered() && !importing {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if add_button.clicked() {
                        submit = true;
                    }

                    let cancel_button = ui.button(self.t("Cancel"));
                    if cancel_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_button.clicked() {
                        close = true;
                    }

                    // The address is checked against the server off the UI
                    // thread; show progress so the dialog doesn't look frozen.
                    if importing {
                        ui.add(egui::Spinner::new());
                    }
                });
            });

        if close {
            self.show_add_repository_modal = false;
            self.add_repository_input_error = None;
            self.add_repository_input_name.clear();
            self.add_repository_input_path.clear();
            self.pending_repository_duplicate_add = None;
            return;
        }

        if !submit || self.repository_space_import_in_flight {
            return;
        }

        let address_input = self.add_repository_input_address.trim().to_string();
        if address_input.is_empty() {
            self.add_repository_input_error = Some(self.t("Address is required"));
            return;
        }

        // Probe the address for a repository-space manifest off the UI thread.
        // The result is applied by `poll_repository_space_import_results`, which
        // either opens the space selector or falls back to adding a plain repo.
        self.add_repository_input_error = None;
        let name_input = self.add_repository_input_name.trim().to_string();
        let path_input = self.add_repository_input_path.trim().to_string();
        self.dispatch_repository_space_import(
            &address_input,
            RepositorySpaceImportContinuation::AddRepositoryDialog {
                address_input: address_input.clone(),
                name: name_input,
                path: path_input,
            },
        );
    }

    pub(super) fn repository_state_label(&self, state: RepoState) -> String {
        match state {
            RepoState::Synced => self.t("Up to date"),
            RepoState::PendingUpdate => self.t("Updates available"),
            RepoState::Updating => self.t("Updating..."),
            RepoState::Unknown => self.t("Unknown"),
        }
    }

    pub(super) fn render_repository_space_bulk_action_modal(&mut self, ctx: &egui::Context) {
        let Some(mut action) = self.pending_repository_space_bulk_action.clone() else {
            return;
        };

        let space_exists = self
            .repository_spaces
            .iter()
            .any(|space| space.id == action.space_id);
        if !space_exists {
            self.pending_repository_space_bulk_action = None;
            return;
        }

        let mut confirm = false;
        let mut cancel = false;
        let mut select_all = false;
        let mut deselect_all = false;
        let row_text_color = self.color_text_normal();
        let state_text_color = self.color_text_dim();
        let required_text_color = self.color_warn();
        let bulk_generation = galley_cache::fingerprint((
            action.space_id.as_str(),
            action.mode as u8,
            action
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.repo_index,
                        entry.repo_name.as_str(),
                        entry.current_state as u8,
                        entry.required,
                    )
                })
                .collect::<Vec<_>>(),
        ));
        self.bulk_action_entry_galleys.ensure(
            action.entries.len(),
            3,
            bulk_generation,
            galley_cache::fingerprint((
                row_text_color.to_array(),
                state_text_color.to_array(),
                required_text_color.to_array(),
            )),
        );
        egui::Window::new(match action.mode {
            RepositorySpaceBulkMode::RecheckAll => self.t("Recheck all repositories"),
            RepositorySpaceBulkMode::UpdateAll => self.t("Update all repositories"),
        })
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(self.color_card_bg())
                .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                .corner_radius(CornerRadius::same(10)),
        )
        .title_bar(true)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(760.0)
        .show(ctx, |ui| {
            ui.label(self.t_fmt(
                "Review repositories in {name}",
                &[("name", action.space_name.clone())],
            ));
            ui.horizontal(|ui| {
                let select_all_btn = ui.button(self.t("Select all"));
                if select_all_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if select_all_btn.clicked() {
                    select_all = true;
                }

                let deselect_all_btn = ui.button(self.t("Deselect all"));
                if deselect_all_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if deselect_all_btn.clicked() {
                    deselect_all = true;
                }
            });

            ui.add_space(8.0);
            ScrollArea::vertical()
                .id_salt(("repository_space_bulk_action_entries", &action.space_id))
                .max_height(320.0)
                .auto_shrink([false, false])
                .show_rows(ui, 34.0, action.entries.len(), |ui, row_range| {
                    ui.set_min_width(ui.available_width());
                    for row in row_range {
                        let entry = &mut action.entries[row];
                        ui.horizontal(|ui| {
                            let checkbox = ui.checkbox(&mut entry.selected, "");
                            if checkbox.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            let name_galley = galley_cache::lazy_galley_colored(
                                ui,
                                self.bulk_action_entry_galleys.slot(row, 0),
                                egui::TextStyle::Button.resolve(ui.style()),
                                row_text_color,
                                || entry.repo_name.clone(),
                            );
                            ui.label(name_galley);
                            ui.add_space(8.0);
                            let state_text = self.t_fmt(
                                "State: {state}",
                                &[("state", self.repository_state_label(entry.current_state))],
                            );
                            let state_galley = galley_cache::lazy_galley_colored(
                                ui,
                                self.bulk_action_entry_galleys.slot(row, 1),
                                egui::TextStyle::Body.resolve(ui.style()),
                                state_text_color,
                                || state_text,
                            );
                            ui.label(state_galley);
                            if entry.required {
                                let required_text = self.t("Required");
                                let required_galley = galley_cache::lazy_galley_colored(
                                    ui,
                                    self.bulk_action_entry_galleys.slot(row, 2),
                                    egui::TextStyle::Body.resolve(ui.style()),
                                    required_text_color,
                                    || required_text,
                                );
                                ui.label(required_galley);
                            }
                        });
                    }
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let selected_count = action.entries.iter().filter(|entry| entry.selected).count();
                ui.label(self.t_fmt(
                    "Selected repositories: {count}",
                    &[("count", selected_count.to_string())],
                ));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let cancel_btn = ui.button(self.t("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }

                    let confirm_btn = ui.add_enabled(
                        selected_count > 0 && self.syncing_repository.is_none(),
                        Button::new(self.t("Confirm")),
                    );
                    if confirm_btn.hovered()
                        && selected_count > 0
                        && self.syncing_repository.is_none()
                    {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_btn.clicked() {
                        confirm = true;
                    }
                });
            });
        });

        if select_all {
            for entry in &mut action.entries {
                entry.selected = true;
            }
        }
        if deselect_all {
            for entry in &mut action.entries {
                entry.selected = false;
            }
        }

        if cancel {
            self.pending_repository_space_bulk_action = None;
            return;
        }

        if confirm {
            let selected_repo_indices: Vec<usize> = action
                .entries
                .iter()
                .filter(|entry| entry.selected)
                .map(|entry| entry.repo_index)
                .collect();
            let queued = self.start_repository_space_bulk_action(
                &action.space_id,
                action.mode,
                &selected_repo_indices,
            );
            info!(
                "Queued repository space bulk action: space={} mode={:?} repos={}",
                action.space_name, action.mode, queued
            );
            self.pending_repository_space_bulk_action = None;
            return;
        }

        self.pending_repository_space_bulk_action = Some(action);
    }

    pub(super) fn render_duplicate_repository_add_confirmation(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_repository_duplicate_add.clone() else {
            return;
        };

        let mut proceed = false;
        let mut cancel = false;
        egui::Window::new(self.t("Duplicate repository detected"))
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(720.0)
            .show(ctx, |ui| {
                ui.label(self.t_fmt(
                    "A repository for {url} is already added in this folder.",
                    &[("url", pending.normalized_url.clone())],
                ));
                if let Some(space_name) = &pending.adding_to_space_name {
                    ui.label(self.t_fmt(
                        "You are adding this repository under repository space {name}.",
                        &[("name", space_name.clone())],
                    ));
                } else {
                    ui.label(self.t("You are adding this repository outside a repository space."));
                }
                ui.label(self.t(
                    "Repositories in the same repository space and folder share database and pending-update state.",
                ));

                ui.add_space(8.0);
                ui.label(RichText::new(self.t("Existing repositories")).strong());
                for (repo_name, space_name) in &pending.existing_repos {
                    let scope = space_name
                        .as_ref()
                        .map(|space| {
                            self.t_fmt("in repository space {name}", &[("name", space.clone())])
                        })
                        .unwrap_or_else(|| self.t("outside repository space"));
                    ui.label(self.t_fmt(
                        "{name} ({scope})",
                        &[("name", repo_name.clone()), ("scope", scope)],
                    ));
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let cancel_btn = ui.button(self.t("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }

                    let proceed_btn = ui.button(self.t("Proceed anyway"));
                    if proceed_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if proceed_btn.clicked() {
                        proceed = true;
                    }
                });
            });

        if cancel {
            self.pending_repository_duplicate_add = None;
            self.add_repository_input_error = None;
            return;
        }

        if !proceed {
            return;
        }

        if let Err(err) = self.confirm_pending_duplicate_repository_add(ctx) {
            warn!("Duplicate repository add confirmation failed: {}", err);
            self.show_error_toast(err);
        } else {
            let action_label = match pending.action {
                PendingRepositoryDuplicateAddAction::FromAddressInput { .. } => {
                    "manual repository add"
                }
                PendingRepositoryDuplicateAddAction::FromSpaceEntry { .. } => {
                    "repository space add"
                }
            };
            info!("Confirmed duplicate repository during {}", action_label);
        }
    }

    pub(super) fn render_repository_space_delete_confirmation(&mut self, ctx: &egui::Context) {
        let Some(space_id) = self.pending_repository_space_delete_id.clone() else {
            return;
        };

        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
            .cloned()
        else {
            self.pending_repository_space_delete_id = None;
            return;
        };

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(self.t("Confirm Repository Space Deletion"))
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(self.t_fmt(
                        "Are you sure you want to delete repository space {name}?",
                        &[(
                            "name",
                            Self::repository_space_display_name(&space).to_string(),
                        )],
                    ));
                    ui.label(self.t(
                        "Existing repositories will remain and will be detached from this repository space.",
                    ));
                    ui.add_space(20.0);

                    let delete_btn = ui.button(self.t("Delete repository space"));
                    if delete_btn.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if delete_btn.clicked() {
                        confirm = true;
                    }

                    let no_btn = ui.button(self.t("No"));
                    if no_btn.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if no_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_repository_space_delete_id = None;
            return;
        }

        if confirm {
            self.delete_repository_space_by_id(&space_id);
        }
    }

    pub(super) fn render_repository_context_confirmation(&mut self, ctx: &egui::Context) {
        let Some(confirm_action) = self.pending_repository_context_confirmation else {
            return;
        };

        let repo_idx = match confirm_action {
            RepositoryContextConfirmAction::Delete(idx)
            | RepositoryContextConfirmAction::WipeRepositoryDb(idx)
            | RepositoryContextConfirmAction::ForceRedownload(idx) => idx,
        };

        let repo = match self.repository_view_state.repositories.get(repo_idx) {
            Some(repo) => repo,
            None => {
                self.pending_repository_context_confirmation = None;
                return;
            }
        };

        let (title, message, confirm_label) = match confirm_action {
            RepositoryContextConfirmAction::Delete(_) => (
                self.t("Confirm Deletion"),
                self.t_fmt(
                    "Are you sure you want to delete {name}?",
                    &[("name", repo.name.clone())],
                ),
                self.t("Delete repository"),
            ),
            RepositoryContextConfirmAction::WipeRepositoryDb(_) => (
                self.t("Confirm Repository Database Wipe"),
                self.t_fmt(
                    "Are you sure you want to wipe database entries for {name}?",
                    &[("name", repo.name.clone())],
                ),
                self.t("Yes, Wipe Repository Database"),
            ),
            RepositoryContextConfirmAction::ForceRedownload(_) => (
                self.t("Confirm Force Redownload"),
                self.t_fmt(
                    "Force redownload {name}?\nThis will remove local files and re-download the repository.",
                    &[("name", repo.name.clone())],
                ),
                self.t("Force redownload repository"),
            ),
        };

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(540.0)
            .default_height(270.0)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(message);
                    if matches!(confirm_action, RepositoryContextConfirmAction::Delete(_)) {
                        ui.add_space(12.0);
                        let delete_files_label = self.t("Delete files");
                        let delete_files_checkbox = ui
                            .checkbox(&mut self.delete_repository_delete_files, delete_files_label);
                        if delete_files_checkbox.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if self.delete_repository_delete_files {
                            ui.label(
                                self.t(
                                    "This will also delete downloaded files for this repository.",
                                ),
                            );
                        }
                    }
                    if matches!(
                        confirm_action,
                        RepositoryContextConfirmAction::WipeRepositoryDb(_)
                    ) {
                        ui.label(self.t("This only clears cached metadata for this repository."));
                    }
                    ui.add_space(20.0);
                    let confirm_button = ui.button(confirm_label);
                    if confirm_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_button.clicked() {
                        confirm = true;
                    }

                    let cancel_button = ui.button(self.t("No"));
                    if cancel_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_button.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_repository_context_confirmation = None;
            self.delete_repository_delete_files = false;
            return;
        }

        if !confirm {
            return;
        }

        self.pending_repository_context_confirmation = None;
        match confirm_action {
            RepositoryContextConfirmAction::Delete(idx) => {
                let delete_local_files = self.delete_repository_delete_files;
                self.delete_repository_delete_files = false;
                self.delete_repository_by_index(idx, delete_local_files);
            }
            RepositoryContextConfirmAction::WipeRepositoryDb(idx) => {
                self.wipe_repository_database_entries(idx);
            }
            RepositoryContextConfirmAction::ForceRedownload(idx) => {
                self.force_redownload_repository(idx);
            }
        }
    }
}
