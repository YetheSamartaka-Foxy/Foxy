use super::{RepositoryActionBannerAction, RepositoryBannerResponse, RepositoryUiAction};
use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::palette;
use crate::ui::types::{FoxyView, RepositorySettingsTab};
use eframe::egui::{
    self, Align, Button, CornerRadius, Frame, Image, Layout, Margin, RichText, TextStyle, Ui, Vec2,
};
use log::info;

impl Foxy {
    /// Fixed height (in points) reserved for the repository banner image. Kept
    /// constant so the layout does not shift when switching between repositories
    /// whose images have different dimensions.
    pub(crate) const REPO_BANNER_HEIGHT: f32 = 160.0;

    /// Renders the standardized repository banner image into a fixed-height
    /// region. The image is scaled to fit within the banner height while
    /// preserving its aspect ratio and centered horizontally. When `hidden` is
    /// true, nothing is drawn and no vertical space is reserved.
    pub(crate) fn render_repository_banner_image(&self, ui: &mut Ui, checksum: &str, hidden: bool) {
        if hidden {
            return;
        }

        let banner_height = Self::REPO_BANNER_HEIGHT;
        let available_width = ui.available_width();
        let repo_image = self
            .cached_repo_images
            .get(checksum)
            .or(self.default_repo_image.as_ref())
            .cloned();

        ui.allocate_ui_with_layout(
            Vec2::new(available_width, banner_height),
            Layout::top_down(Align::Center),
            |ui| {
                if let Some(tex) = repo_image {
                    let (w, h) = (tex.size()[0] as f32, tex.size()[1] as f32);
                    let ratio = if h > 0.0 { w / h } else { 1.0 };
                    let mut height = banner_height;
                    let mut width = height * ratio;
                    if width > available_width {
                        width = available_width;
                        height = width / ratio;
                    }
                    ui.add(Image::new((tex.id(), Vec2::new(width, height))));
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label(tr("[Image Placeholder]"));
                    });
                }
            },
        );
        ui.add_space(10.0);
    }

    pub fn render_repository_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.handle_repository_view_keyboard_navigation(ui.ctx());

        self.render_repository_sidebar(ui);

        egui::CentralPanel::default().show(ui, |ui| {
            let central_margin = Margin {
                left: 10,
                right: 10,
                top: 0,
                bottom: 0,
            };
            let central_frame = egui::containers::Frame::side_top_panel(&ui.ctx().global_style())
                .inner_margin(central_margin);

            let mut recheck_action: Option<RepositoryUiAction> = None;
            let mut quick_check_action: Option<RepositoryUiAction> = None;
            let mut refresh_servers_action: Option<RepositoryUiAction> = None;
            let mut open_summary_action: Option<RepositoryUiAction> = None;
            let mut open_pending_update_action: Option<RepositoryUiAction> = None;
            let mut open_update_view_action: Option<RepositoryUiAction> = None;
            let mut settings_action: Option<RepositoryUiAction> = None;
            let mut profile_changed = false;
            let mut dismiss_completed_check_banner = false;
            let mut cancel_sync_requested = false;

            central_frame.show(ui, |ui| {
                if let Some(space_id) = self.selected_repository_space_id.clone() {
                    self.render_repository_space_detail(ui, &space_id);
                } else if let Some(folder_id) = self.selected_repository_visual_folder_id.clone() {
                    let folder = self
                        .repository_visual_folders
                        .iter()
                        .find(|folder| folder.id == folder_id)
                        .cloned();
                    ui.vertical_centered_justified(|ui| {
                        if let Some(folder) = folder {
                            ui.heading(folder.name);
                            ui.label(self.t_fmt(
                                "Repositories in folder: {count}",
                                &[("count", folder.repository_keys.len().to_string())],
                            ));
                        } else {
                            self.selected_repository_visual_folder_id = None;
                            ui.heading(tr("Selected repository"));
                            ui.label(tr("No repository selected"));
                        }
                    });
                } else if let Some(selected_idx) = self.repository_view_state.selected_repository {
                    let color_primary_accent = self.color_primary_accent();
                    let (
                        repo_name,
                        repo_address,
                        repo_path,
                        repo_image_checksum,
                        hide_repo_image_override,
                    ) = {
                        let repo = &self.repository_view_state.repositories[selected_idx];
                        (
                            repo.name.clone(),
                            repo.address.clone(),
                            repo.path.clone(),
                            repo.repo_image_checksum.clone(),
                            repo.hide_repo_image,
                        )
                    };
                    let hide_repo_image = hide_repo_image_override
                        .unwrap_or(self.settings_view_state.hide_repository_image);
                    let repo_state = self.repo_state_for_address(&repo_address, &repo_path);
                    let status_banner = self
                        .active_repository_check_banner(selected_idx)
                        .or_else(|| self.active_repository_db_wipe_banner(selected_idx));
                    let completed_check_banner = self
                        .completed_repository_db_wipe_banner(selected_idx)
                        .or_else(|| self.completed_repository_check_banner(selected_idx));
                    let action_banner = self.repository_action_banner(selected_idx, repo_state);
                    let status_banner_elapsed_label = status_banner.as_ref().map(|banner| {
                        self.t_fmt(
                            "Elapsed {seconds}s",
                            &[("seconds", banner.elapsed_seconds.to_string())],
                        )
                    });
                    let status_banner_fill = self.color_widget_bg();
                    let status_banner_stroke = self.color_primary_accent();
                    let status_banner_text = self.color_text_normal();
                    let status_banner_dim = self.color_text_dim();
                    let dismiss_label = self.t("Dismiss");
                    self.render_repository_banner_image(
                        ui,
                        &repo_image_checksum,
                        hide_repo_image,
                    );

                    {
                        let profile_combo_tooltip = self.t("Select a launch profile for this repository. Profiles store addon selections, parameters, and DLC toggles.");
                        let arma3_profile_combo_tooltip = self.t("Select which Arma 3 profile to use when launching. Controls character name, settings, and keybinds.");
                        let repo = &mut self.repository_view_state.repositories[selected_idx];
                        let mut hf =
                            egui::containers::Frame::side_top_panel(&ui.ctx().global_style());
                        hf.fill = color_primary_accent;
                        hf.inner_margin = Margin::same(10);
                        hf.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let toolbar_icon_size = self
                                    .settings_view_state
                                    .font_sizes
                                    .repository_view
                                    .toolbar_icons as f32;
                                let toolbar_btn_width =
                                    Self::toolbar_icon_button_size(toolbar_icon_size).x;
                                let toolbar_count = 4.0;
                                let toolbar_total = toolbar_count * toolbar_btn_width
                                    + (toolbar_count - 1.0) * ui.spacing().item_spacing.x;
                                let heading_max_width = (ui.available_width()
                                    - toolbar_total
                                    - ui.spacing().item_spacing.x)
                                    .max(0.0);

                                ui.scope(|ui| {
                                    ui.set_max_width(heading_max_width);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&repo.name)
                                                .text_style(TextStyle::Heading),
                                        )
                                        .truncate(),
                                    );
                                    if !repo.profiles.is_empty() {
                                        ui.add_space(10.0);
                                        let default_profile_text = tr("Default");
                                        let combo = egui::ComboBox::from_label("")
                                            .selected_text(
                                                repo.selected_profile
                                                    .clone()
                                                    .unwrap_or_else(|| {
                                                        default_profile_text.clone()
                                                    }),
                                            )
                                            .show_ui(ui, |ui| {
                                                let default_response = ui.selectable_label(
                                                    repo.selected_profile.is_none(),
                                                    default_profile_text.clone(),
                                                );
                                                if default_response.hovered() {
                                                    ui.ctx().output_mut(
                                                        Foxy::set_pointing_cursor_output,
                                                    );
                                                }
                                                if default_response.clicked() {
                                                    repo.selected_profile = None;
                                                    profile_changed = true;
                                                }
                                                for p in &repo.profiles {
                                                    let profile_response = ui.selectable_label(
                                                        repo.selected_profile.as_deref()
                                                            == Some(&p.name),
                                                        &p.name,
                                                    );
                                                    if profile_response.hovered() {
                                                        ui.ctx().output_mut(
                                                            Foxy::set_pointing_cursor_output,
                                                        );
                                                    }
                                                    if profile_response.clicked() {
                                                        repo.selected_profile =
                                                            Some(p.name.clone());
                                                        profile_changed = true;
                                                    }
                                                }
                                            });
                                        let combo_response = combo.response.on_hover_text(profile_combo_tooltip.clone());
                                        if combo_response.hovered() {
                                            ui.ctx().output_mut(|o| {
                                                Foxy::set_pointing_cursor_output(o)
                                            });
                                        }
                                    }

                                    // Arma 3 profile dropdown
                                    if !self.detected_arma3_profiles.is_empty() {
                                        ui.add_space(10.0);
                                        let auto_label = tr("Auto-detect");
                                        let selected_text = repo
                                            .arma3_profile
                                            .clone()
                                            .unwrap_or_else(|| auto_label.clone());
                                        let a3combo = egui::ComboBox::from_id_salt(
                                            "arma3_profile_combo",
                                        )
                                        .selected_text(format!(
                                            "{}: {}",
                                            tr("Arma 3 Profile"),
                                            selected_text
                                        ))
                                        .show_ui(ui, |ui| {
                                            let auto_response = ui.selectable_label(
                                                repo.arma3_profile.is_none(),
                                                &auto_label,
                                            );
                                            if auto_response.hovered() {
                                                ui.ctx().output_mut(
                                                    Foxy::set_pointing_cursor_output,
                                                );
                                            }
                                            if auto_response.clicked() {
                                                repo.arma3_profile = None;
                                                self.cached_missions = None;
                                                self.repository_selection = None;
                                                self.pending_mission_duplicate = None;
                                                self.pending_mission_delete = None;
                                                self.pending_mission_remove_dependencies = None;
                                                self.editor_mission_search.clear();
                                                self.editor_mission_folder.clear();
                                                self.editor_mission_terrain_filter.clear();
                                                profile_changed = true;
                                            }
                                            for p in &self.detected_arma3_profiles {
                                                let profile_response = ui.selectable_label(
                                                    repo.arma3_profile.as_deref()
                                                        == Some(&p.name),
                                                    &p.name,
                                                );
                                                if profile_response.hovered() {
                                                    ui.ctx().output_mut(
                                                        Foxy::set_pointing_cursor_output,
                                                    );
                                                }
                                                if profile_response.clicked() {
                                                    repo.arma3_profile = Some(p.name.clone());
                                                    self.cached_missions = None;
                                                    self.repository_selection = None;
                                                    self.pending_mission_duplicate = None;
                                                    self.pending_mission_delete = None;
                                                    self.pending_mission_remove_dependencies = None;
                                                    self.editor_mission_search.clear();
                                                    self.editor_mission_folder.clear();
                                                    self.editor_mission_terrain_filter.clear();
                                                    profile_changed = true;
                                                }
                                            }
                                        });
                                        let a3combo_response = a3combo.response.on_hover_text(arma3_profile_combo_tooltip.clone());
                                        if a3combo_response.hovered() {
                                            ui.ctx().output_mut(|o| {
                                                Foxy::set_pointing_cursor_output(o)
                                            });
                                        }
                                    }
                                });

                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    let toolbar_icon_size = self
                                        .settings_view_state
                                        .font_sizes
                                        .repository_view
                                        .toolbar_icons as f32;
                                    let settings_button = Self::repository_toolbar_icon_button(
                                        ui,
                                        "\u{2699}",
                                        toolbar_icon_size,
                                        tr("Repository Settings").as_str(),
                                        true,
                                        None,
                                    );
                                    if settings_button.hovered() {
                                        ui.ctx().output_mut(|o| {
                                            Foxy::set_pointing_cursor_output(o)
                                        });
                                    }
                                    if settings_button.clicked() {
                                        let idx = selected_idx;
                                        info!("Opening repository settings for {}", repo_name);
                                        settings_action = Some(Box::new(move |app| {
                                            app.selected_repository_for_settings = Some(idx);
                                            app.current_repository_settings_tab =
                                                RepositorySettingsTab::Configuration;
                                            app.last_view = app.current_view;
                                            app.current_view = FoxyView::RepositorySettings;
                                            app.preload_repository_settings_addon_caches(idx);
                                        }));
                                    }

                                    let is_syncing_anything = self.syncing_repository.is_some();
                                    let recheck_disabled = is_syncing_anything;
                                    let quick_check_disabled =
                                        self.current_sync_mode == Some(SyncMode::QuickCheckOnly);

                                    let recheck_button = Self::repository_toolbar_icon_button(
                                        ui,
                                        "\u{21bb}",
                                        toolbar_icon_size,
                                        tr("Remote data recheck.\nIt checks for possible pending changes from a remote host and local changes to it.").as_str(),
                                        !recheck_disabled,
                                        None,
                                    );
                                    if recheck_button.hovered() && !recheck_disabled {
                                        ui.ctx().output_mut(|o| {
                                            Foxy::set_pointing_cursor_output(o)
                                        });
                                    }
                                    if recheck_button.clicked() {
                                        let idx = selected_idx;
                                        info!(
                                            "Manual remote data recheck requested for {}",
                                            repo_name
                                        );
                                        recheck_action = Some(Box::new(move |app| {
                                            app.start_remote_recheck_with_plan(idx);
                                        }));
                                    }

                                    let quick_check_button =
                                        Self::repository_toolbar_icon_button(
                                            ui,
                                            "\u{1F4DA}",
                                            toolbar_icon_size,
                                            tr("Quick local check.\nIt checks local files for integrity issues or changes.").as_str(),
                                            !quick_check_disabled,
                                            None,
                                        );
                                    if quick_check_button.hovered()
                                        && !quick_check_disabled
                                    {
                                        ui.ctx().output_mut(|o| {
                                            Foxy::set_pointing_cursor_output(o)
                                        });
                                    }
                                    if quick_check_button.clicked() {
                                        let idx = selected_idx;
                                        info!(
                                            "Manual quick local check requested for {}",
                                            repo_name
                                        );
                                        quick_check_action = Some(Box::new(move |app| {
                                            app.start_core_sync(idx, SyncMode::QuickCheckOnly);
                                        }));
                                    }

                                    let refresh_server_button = Self::repository_toolbar_icon_button(
                                        ui,
                                        "\u{1F4E1}",
                                        toolbar_icon_size,
                                        tr("Refresh server status").as_str(),
                                        true,
                                        None,
                                    );
                                    if refresh_server_button.hovered() {
                                        ui.ctx().output_mut(|o| {
                                            Foxy::set_pointing_cursor_output(o)
                                        });
                                    }
                                    if refresh_server_button.clicked() {
                                        let idx = selected_idx;
                                        info!(
                                            "Manual server status refresh requested for {}",
                                            repo_name
                                        );
                                        refresh_servers_action = Some(Box::new(move |app| {
                                            // Re-fetch repo.json to update servers list first.
                                            app.pending_repo_metadata_refresh.push(idx);
                                            if let Some(repo) =
                                                app.repository_view_state.repositories.get(idx)
                                            {
                                                let servers = repo.servers.clone();
                                                for server in &servers {
                                                    app.force_refresh_server_status(server);
                                                }
                                            }
                                        }));
                                    }
                                });
                            });
                        });
                    }

                    ui.separator();
                    ui.add_space(10.0);

                    // Legacy protocol warning - shown for repos not using FoxyMode (BLAKE3).
                    if self
                        .repo_foxy_mode_for_address(&repo_address)
                        .is_some_and(|is_foxy| !is_foxy)
                    {
                        self.render_repository_message_banner(
                            ui,
                            &self.t("Legacy Protocol (MD5)"),
                            &self.t("This repository is using the old Swifty protocol (MD5) and should be migrated to the Foxy protocol (BLAKE3). Contact your server administrator to perform the migration - hybrid mode is supported for a smooth transition."),
                            palette::WARN,
                            None,
                            None,
                        );
                        ui.add_space(10.0);
                    }

                    if let Some(status_banner) = status_banner {
                        let banner_font_size = self
                            .settings_view_state
                            .font_sizes
                            .repository_view
                            .status_banner as f32;
                        let detail_font_size = (banner_font_size - 3.0).max(14.0);
                        let hint_font_size = (banner_font_size - 7.0).max(12.0);
                        let title = status_banner.title;
                        let detail = status_banner.detail;
                        let hint = status_banner.hint;
                        let progress = status_banner.progress;
                        let elapsed_label = status_banner_elapsed_label.unwrap_or_default();

                        Frame::NONE
                            .fill(status_banner_fill)
                            .stroke(egui::Stroke::new(1.0, status_banner_stroke))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(detail_font_size.max(16.0)));
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(title.as_str())
                                            .size(banner_font_size)
                                            .strong(),
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let cancel_btn = ui.add(
                                            Button::new(
                                                RichText::new(self.t("Cancel"))
                                                    .size((detail_font_size - 1.0).max(13.0)),
                                            )
                                            .fill(self.color_action_destructive()),
                                        );
                                        if cancel_btn.hovered() {
                                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                        }
                                        if cancel_btn.clicked() {
                                            cancel_sync_requested = true;
                                        }
                                        ui.add_space(8.0);
                                        ui.label(
                                            RichText::new(elapsed_label.as_str())
                                            .size(hint_font_size)
                                            .color(status_banner_dim),
                                        );
                                    });
                                });
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(detail.as_str())
                                        .size(detail_font_size)
                                        .color(status_banner_text),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(hint.as_str())
                                        .size(hint_font_size)
                                        .color(status_banner_dim),
                                );
                                if let Some(progress) = progress {
                                    ui.add_space(8.0);
                                    ui.add(
                                        egui::ProgressBar::new(progress)
                                            .desired_width(ui.available_width())
                                            .fill(status_banner_stroke),
                                    );
                                }
                            });
                        ui.add_space(10.0);
                    } else if let Some(completed_check_banner) = completed_check_banner {
                        let update_ready_label = tr("Update ready - click here");
                        let response = self.render_repository_message_banner(
                            ui,
                            completed_check_banner.title.as_str(),
                            completed_check_banner.detail.as_str(),
                            completed_check_banner.stroke_color,
                            completed_check_banner.show_pending_action.then_some((
                                update_ready_label.as_str(),
                                self.color_action_destructive(),
                            )),
                            (!completed_check_banner.show_pending_action)
                                .then_some(dismiss_label.as_str()),
                        );
                        match response {
                            RepositoryBannerResponse::ActionClicked => {
                                Self::queue_open_pending_update_action(
                                    &mut open_pending_update_action,
                                    selected_idx,
                                );
                            }
                            RepositoryBannerResponse::DismissClicked => {
                                dismiss_completed_check_banner = true;
                            }
                            RepositoryBannerResponse::None => {}
                        }
                        ui.add_space(10.0);
                    } else if let Some(action_banner) = action_banner {
                        // The download summary banner can be dismissed directly,
                        // without first opening (and then closing) the summary.
                        let allow_dismiss = action_banner.action
                            == RepositoryActionBannerAction::UpdateSummary;
                        let response = self.render_repository_message_banner(
                            ui,
                            action_banner.title.as_str(),
                            action_banner.detail.as_str(),
                            action_banner.stroke_color,
                            Some((action_banner.button_label.as_str(), action_banner.button_fill)),
                            allow_dismiss.then_some(dismiss_label.as_str()),
                        );
                        if response == RepositoryBannerResponse::DismissClicked {
                            let idx = selected_idx;
                            open_summary_action = Some(Box::new(move |app| {
                                app.acknowledge_update_summary_for_repo(idx);
                            }));
                        } else if response == RepositoryBannerResponse::ActionClicked {
                            match action_banner.action {
                                RepositoryActionBannerAction::PendingUpdate => {
                                    Self::queue_open_pending_update_action(
                                        &mut open_pending_update_action,
                                        selected_idx,
                                    );
                                }
                                RepositoryActionBannerAction::UpdateView => {
                                    let repo_name_for_log = repo_name.clone();
                                    open_update_view_action = Some(Box::new(move |app| {
                                        app.direct_download_update_view = false;
                                        app.update_modal_open = true;
                                        info!("Opened update modal for {}", repo_name_for_log);
                                    }));
                                }
                                RepositoryActionBannerAction::UpdateSummary => {
                                    let idx = selected_idx;
                                    let repo_name_for_log = repo_name.clone();
                                    open_summary_action = Some(Box::new(move |app| {
                                        info!("Opening update summary for {}", repo_name_for_log);
                                        if app.open_update_summary_for_repo(idx)
                                            && let Some(repo) =
                                                app.repository_view_state.repositories.get(idx)
                                        {
                                            info!("Opened update summary for {}", repo.name);
                                        }
                                    }));
                                }
                            }
                        }
                        ui.add_space(10.0);
                    }

                    let (enable_server_list, enable_editor_mission_list) = {
                        let repo = &self.repository_view_state.repositories[selected_idx];
                        (
                            self.repo_enable_server_list(repo),
                            self.repo_enable_editor_mission_list(repo),
                        )
                    };
                    let available_rect = ui.available_rect_before_wrap();
                    let available_height = available_rect.height().max(0.0);
                    let action_area_height =
                        self.repository_launch_join_area_height().min(available_height);
                    let list_area_height = (available_height - action_area_height).max(0.0);
                    if list_area_height > 0.0 && (enable_server_list || enable_editor_mission_list) {
                        let list_rect = egui::Rect::from_min_size(
                            available_rect.min,
                            Vec2::new(ui.available_width(), list_area_height),
                        );
                        let mut list_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt(("repository_detail_lists", selected_idx))
                                .max_rect(list_rect)
                                .layout(Layout::top_down(Align::Min)),
                        );
                        list_ui.set_clip_rect(list_rect);

                        let mission_entry_count = if enable_editor_mission_list {
                            self.visible_editor_mission_entry_count(selected_idx)
                        } else {
                            None
                        };
                        let server_min_height = if enable_server_list {
                            self.repository_server_min_section_height(&list_ui, selected_idx)
                        } else {
                            0.0
                        };
                        let server_full_height = if enable_server_list {
                            self.repository_server_full_section_height(&list_ui, selected_idx)
                        } else {
                            0.0
                        };
                        let mission_min_height = if enable_editor_mission_list {
                            self.repository_editor_mission_min_section_height(
                                &list_ui,
                                mission_entry_count,
                            )
                        } else {
                            0.0
                        };
                        let server_section_height = if enable_editor_mission_list
                            && mission_entry_count.is_some()
                        {
                            let min_total_height = server_min_height + mission_min_height;
                            if list_area_height <= min_total_height {
                                (list_area_height * 0.45)
                                    .max(server_min_height.min(list_area_height))
                                    .min(list_area_height)
                            } else {
                                let extra_height = list_area_height - min_total_height;
                                (server_min_height + extra_height * 0.5)
                                    .min(server_full_height)
                                    .min(list_area_height)
                            }
                        } else {
                            server_full_height.min(list_area_height)
                        };

                        if enable_server_list {
                            self.render_repository_servers_section(
                                &mut list_ui,
                                selected_idx,
                                &repo_name,
                                Some(server_section_height),
                            );
                        }
                        if enable_editor_mission_list {
                            let mission_section_height = list_ui.available_height().max(0.0);
                            self.render_editor_missions_section(
                                &mut list_ui,
                                selected_idx,
                                Some(mission_section_height),
                            );
                        }
                    }
                    if action_area_height > 0.0 {
                        let action_rect = egui::Rect::from_min_size(
                            egui::pos2(
                                available_rect.left(),
                                available_rect.bottom() - action_area_height,
                            ),
                            Vec2::new(ui.available_width(), action_area_height),
                        );
                        let mut action_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt(("repository_launch_join_area", selected_idx))
                                .max_rect(action_rect)
                                .layout(Layout::top_down(Align::Min)),
                        );
                        action_ui.set_clip_rect(action_rect);
                        self.render_launch_join_buttons(&mut action_ui, selected_idx, &repo_name);
                    }
                    ui.advance_cursor_after_rect(available_rect);
                } else {
                    ui.vertical_centered_justified(|ui| {
                        ui.heading(tr("Selected repository"));
                        ui.label(tr("No repository selected"));
                    });
                }
            });

            if let Some(act) = recheck_action {
                act(self);
            }
            if let Some(act) = quick_check_action {
                act(self);
            }
            if let Some(act) = refresh_servers_action {
                act(self);
            }
            if let Some(act) = open_summary_action {
                act(self);
            }
            if let Some(act) = open_pending_update_action {
                act(self);
            }
            if let Some(act) = open_update_view_action {
                act(self);
            }
            if let Some(act) = settings_action {
                act(self);
            }
            if dismiss_completed_check_banner {
                self.dismiss_completed_repository_check_banner();
            }
            if cancel_sync_requested {
                self.cancel_sync();
            }
            if profile_changed {
                self.save_repositories();
            }
        });

        self.render_repository_context_confirmation(ui.ctx());
        self.render_repository_space_delete_confirmation(ui.ctx());
        self.render_repository_visual_folder_edit_modal(ui.ctx());
        self.render_repository_visual_folder_delete_confirmation(ui.ctx());
        self.render_repository_space_bulk_action_modal(ui.ctx());
        self.render_delete_mission_modal(ui.ctx());
        self.render_remove_mission_dependencies_modal(ui.ctx());
        self.render_duplicate_mission_modal(ui.ctx());
        self.render_editor_mission_external_addons_warning_modal(ui.ctx());
        self.render_join_preflight_modal(ui.ctx());
        self.render_add_repository_modal(ui.ctx());
        self.render_duplicate_repository_add_confirmation(ui.ctx());
    }
}
