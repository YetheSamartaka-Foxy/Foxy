use crate::core::api;
use crate::ui::app::Foxy;
use crate::ui::search_filter::MultiEntryFilter;
use crate::ui::types::{FoxyView, HelpTab};
use crate::ui::views::galley_cache;
use arboard::Clipboard;
use eframe::egui::{
    self, Align, Button, FontId, Id, Label, Layout, Margin, Panel, PointerButton, RichText,
    ScrollArea, Sense, TextStyle, Ui, Vec2, ViewportCommand,
};
use log::{info, warn};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
struct ActivityLogHeightLimits {
    min: f32,
    max: f32,
    target: f32,
}

fn activity_log_height_limits(
    available_height: f32,
    line_height: f32,
    control_height: f32,
    modal_active: bool,
) -> ActivityLogHeightLimits {
    let available_height = available_height.max(0.0);
    let line_height = line_height.max(1.0);
    let control_height = control_height.max(line_height);
    let compact_min_height = (control_height + line_height * 3.0 + 12.0).min(available_height);
    let preferred_min_height = control_height + line_height * 6.0 + 12.0;
    let hard_max_height = control_height + line_height * 30.0 + 12.0;
    let reserved_content_target: f32 = if modal_active { 430.0 } else { 320.0 };
    let reserved_content_fraction: f32 = if modal_active { 0.68 } else { 0.58 };
    let target_fraction: f32 = if modal_active { 0.2 } else { 0.28 };

    let available_after_compact_log = (available_height - compact_min_height).max(0.0);
    let reserved_content_floor = reserved_content_target.min(available_after_compact_log);
    let reserved_content_height = (available_height * reserved_content_fraction)
        .max(reserved_content_floor)
        .min(available_after_compact_log);
    let safe_max_height = (available_height - reserved_content_height).max(compact_min_height);
    let max_height = hard_max_height.min(safe_max_height).max(compact_min_height);
    let min_height = preferred_min_height.min(max_height).max(compact_min_height);
    let target_height = (available_height * target_fraction).clamp(min_height, max_height);

    ActivityLogHeightLimits {
        min: min_height,
        max: max_height,
        target: target_height,
    }
}

impl Foxy {
    fn has_activity_log_safe_space_modal(&self) -> bool {
        self.update_modal_open
            || self.show_add_repository_modal
            || self.pending_repository_duplicate_add.is_some()
            || self.pending_mission_duplicate.is_some()
            || self.pending_mission_delete.is_some()
            || self.pending_mission_remove_dependencies.is_some()
            || self.pending_mission_editor_launch_warning.is_some()
            || self.pending_addon_destructive_confirmation.is_some()
            || self.pending_repository_space_bulk_action.is_some()
            || self.pending_repository_space_delete_id.is_some()
            || self.pending_repository_context_confirmation.is_some()
            || self.show_delete_confirmation
            || self.show_force_redownload_confirmation
            || self.show_wipe_db_confirmation
            || self.show_wipe_repo_db_confirmation
            || self.pending_renderer_fallback_notice
            || self.pending_db_schema_wipe.is_some()
            || self.db_lock_conflict.is_some()
            || self.pending_app_update_prompt
            || self.show_add_profile_window
            || self.show_rename_profile_window
            || self.pending_profile_confirm_action.is_some()
            || self.pending_settings_reset_confirmation
            || self.pending_settings_folder_removal.is_some()
            || self.addon_backup_restore_state.is_some()
            || self.backup_manager_confirm_action.is_some()
            || self.ts3_plugin_update_prompt.is_some()
            || self.show_memory_diagnostics_window
    }

    pub fn render_main_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let mut top_bar_frame = egui::containers::Frame::side_top_panel(&ui.ctx().global_style());
        top_bar_frame.fill = self.color_primary_accent();
        top_bar_frame.inner_margin = Margin {
            left: 0,
            right: 8,
            top: 0,
            bottom: 0,
        };
        Panel::top("top_bar")
            .default_size(self.header_bar_height())
            .resizable(false)
            .frame(top_bar_frame)
            .show(ui, |ui| {
                let use_custom_window_chrome = !self.main_view_state.use_window_decorations;
                let control_button_size = self.header_control_button_size();
                let control_spacing = ui.spacing().item_spacing.x;
                let control_count: usize = if use_custom_window_chrome { 4 } else { 1 };
                let (is_minimized, is_maximized, is_focused) = ui.input(|i| {
                    (
                        i.viewport().minimized.unwrap_or(false),
                        i.viewport().maximized.unwrap_or(false),
                        i.viewport().focused.unwrap_or(true),
                    )
                });
                let drag_area = {
                    let full_rect = ui.max_rect();
                    let reserved_controls_width = (control_button_size.x * control_count as f32)
                        + (control_spacing * control_count.saturating_sub(1) as f32)
                        + 16.0;
                    let drag_max_x =
                        (full_rect.max.x - reserved_controls_width).max(full_rect.min.x);
                    egui::Rect::from_min_max(full_rect.min, egui::pos2(drag_max_x, full_rect.max.y))
                };
                let top_bar_resp = ui.interact(
                    drag_area,
                    Id::new("top_bar_drag_area"),
                    Sense::click_and_drag(),
                );
                if use_custom_window_chrome && !is_minimized && top_bar_resp.double_clicked() {
                    info!("Toggling window maximized state from top bar double click");
                    ui.ctx()
                        .send_viewport_cmd(ViewportCommand::Maximized(!is_maximized));
                }
                if use_custom_window_chrome
                    && !is_minimized
                    && !is_maximized
                    && is_focused
                    && top_bar_resp.drag_started_by(PointerButton::Primary)
                {
                    ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                }
                self.render_app_header(ui);
            });

        let mut bottom_bar_frame =
            egui::containers::Frame::side_top_panel(&ui.ctx().global_style());
        bottom_bar_frame.fill = self.color_main_bg();
        Panel::bottom("bottom_bar")
            .default_size(self.footer_bar_height())
            .resizable(false)
            .frame(bottom_bar_frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let footer_icon_size = self
                        .settings_view_state
                        .font_sizes
                        .main_view
                        .activity_log_toggle_icon as f32;
                    let footer_icon_button_size = self.activity_log_toggle_button_size();
                    let footer_text_size = (footer_icon_size - 1.0).max(11.0);

                    let version_text = crate::build_info::version_label();
                    let version_button_width =
                        (version_text.chars().count() as f32 * (footer_text_size * 0.75)).max(44.0)
                            + 8.0;
                    let version_button = ui.add_sized(
                        Vec2::new(version_button_width, footer_icon_button_size.y),
                        Button::new(RichText::new(version_text).size(footer_text_size))
                            .frame(false),
                    );
                    if version_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if version_button
                        .on_hover_text(self.t("Open changelog"))
                        .clicked()
                    {
                        info!("Opening changelog view from bottom bar");
                        self.open_reference_view(FoxyView::Changelog);
                    }

                    // Update available badge
                    if matches!(
                        &self.app_update_status,
                        crate::core::tasks::app_update::UpdateCheckStatus::Available(_)
                    ) {
                        let update_btn = ui.add_sized(
                            Vec2::new(footer_icon_button_size.x + 20.0, footer_icon_button_size.y),
                            Button::new(
                                RichText::new(format!("\u{2B06} {}", self.t("Update")))
                                    .size(footer_text_size)
                                    .color(self.color_primary_accent()),
                            )
                            .frame(false),
                        );
                        if update_btn.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if update_btn
                            .on_hover_text(self.t("App update available"))
                            .clicked()
                        {
                            info!("Opening app update view from footer badge");
                            self.open_reference_view(FoxyView::AppUpdate);
                        }
                    }

                    ui.separator();

                    let about_button = ui.add_sized(
                        footer_icon_button_size,
                        Button::new(RichText::new("\u{2139}").size(footer_icon_size)).frame(false),
                    );
                    if about_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if about_button.on_hover_text(self.t("Open about")).clicked() {
                        info!("Opening about view from bottom bar");
                        self.open_reference_view(FoxyView::About);
                    }

                    ui.separator();

                    let help_button = ui.add_sized(
                        footer_icon_button_size,
                        Button::new(RichText::new("\u{1F4D6}").size(footer_icon_size)).frame(false),
                    );
                    if help_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if help_button
                        .on_hover_text(format!("{} (F1)", self.t("Open help")))
                        .clicked()
                    {
                        info!("Opening help view from bottom bar");
                        self.current_help_tab = HelpTab::Overview;
                        self.open_reference_view(FoxyView::Help);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let toggle_button_size = self.activity_log_toggle_button_size();
                        let toggle_log_button = ui.add_sized(
                            toggle_button_size,
                            Button::new(
                                RichText::new("\u{1F4DD}").size(
                                    self.settings_view_state
                                        .font_sizes
                                        .main_view
                                        .activity_log_toggle_icon
                                        as f32,
                                ),
                            )
                            .frame(false),
                        );
                        if toggle_log_button.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if toggle_log_button
                            .on_hover_text(format!("{} (F3)", self.t("Show activity log")))
                            .clicked()
                        {
                            self.set_activity_log_visibility(
                                ui.ctx(),
                                !self.settings_view_state.show_activity_log,
                                "footer button",
                            );
                        }

                        // Optional FPS readout, sitting just left of the activity-log
                        // toggle. Lives in the footer rather than as a floating overlay.
                        if self.settings_view_state.show_fps_counter {
                            ui.add_space(6.0);
                            let fps = self.fps_ema.round().max(0.0) as i32;
                            ui.label(
                                RichText::new(format!("{} {}", fps, self.t("FPS")))
                                    .monospace()
                                    .size(footer_text_size)
                                    .color(self.fps_readout_color()),
                            );
                            ui.add_space(6.0);
                        }

                        if self.settings_view_state.show_memory_diagnostics_icon {
                            ui.separator();

                            let toggle_memory_button = ui.add_sized(
                                toggle_button_size,
                                Button::new(
                                    RichText::new("\u{2665}").size(
                                        self.settings_view_state
                                            .font_sizes
                                            .main_view
                                            .activity_log_toggle_icon
                                            as f32,
                                    ),
                                )
                                .frame(false),
                            );
                            if toggle_memory_button.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if toggle_memory_button
                                .on_hover_text(format!(
                                    "{} (F4)",
                                    self.t("Open memory diagnostics")
                                ))
                                .clicked()
                            {
                                self.show_memory_diagnostics_window =
                                    !self.show_memory_diagnostics_window;
                                if self.show_memory_diagnostics_window {
                                    self.capture_memory_diagnostics_snapshot("window opened", true);
                                }
                                info!(
                                    "Memory diagnostics window visibility set to {}",
                                    self.show_memory_diagnostics_window
                                );
                            }
                        }
                    });
                });
            });

        if self.settings_view_state.show_activity_log {
            let text_style = TextStyle::Small;
            let line_height = ui.text_style_height(&text_style).max(1.0);
            let control_height = ui.spacing().interact_size.y.max(line_height + 8.0);
            let height_limits = activity_log_height_limits(
                ui.available_height(),
                line_height,
                control_height,
                self.has_activity_log_safe_space_modal(),
            );
            let font_size = ui
                .style()
                .text_styles
                .get(&text_style)
                .map(|style| (style.size - 1.0).max(9.0))
                .unwrap_or(11.0);

            let mut log_frame = egui::containers::Frame::side_top_panel(&ui.ctx().global_style());
            log_frame.fill = self.color_card_bg();
            Panel::bottom("activity_log_panel")
                .default_size(height_limits.target)
                .min_size(height_limits.min)
                .max_size(height_limits.max)
                .resizable(true)
                .frame(log_frame)
                .show(ui, |ui| {
                    let now = Instant::now();
                    let should_poll = self.activity_log_last_poll_at.is_none_or(|last_poll_at| {
                        now.duration_since(last_poll_at) >= Duration::from_millis(250)
                    });
                    if should_poll {
                        self.activity_log_last_poll_at = Some(now);
                        let generation = api::activity_log_generation();
                        if generation != self.activity_log_generation {
                            self.activity_log_cache = api::activity_log_snapshot();
                            self.activity_log_generation = generation;
                        }
                    }

                    let label_error = self.t("Error");
                    let label_warn = self.t("Warn");
                    let label_info = self.t("Info");
                    let label_debug = self.t("Debug");
                    let label_trace = self.t("Trace");
                    let label_search = self.t("Search...");
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.checkbox(&mut self.activity_log_filter_error, label_error);
                        ui.checkbox(&mut self.activity_log_filter_warn, label_warn);
                        ui.checkbox(&mut self.activity_log_filter_info, label_info);
                        ui.checkbox(&mut self.activity_log_filter_debug, label_debug);
                        ui.checkbox(&mut self.activity_log_filter_trace, label_trace);
                        ui.separator();
                        ui.add(
                            egui::TextEdit::singleline(&mut self.activity_log_search)
                                .id_salt("activity_log_search")
                                .hint_text(label_search)
                                .desired_width(150.0),
                        );
                        ui.separator();
                        let copy_button = ui.add(Button::new(
                            RichText::new(self.t("Copy all")).size(font_size),
                        ));
                        if copy_button.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if copy_button.clicked() {
                            let content = self
                                .activity_log_cache
                                .iter()
                                .map(|e| {
                                    format!(
                                        "{} [{}] [{}] {}",
                                        e.timestamp, e.level, e.source, e.message
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            match Clipboard::new()
                                .and_then(|mut clipboard| clipboard.set_text(content))
                            {
                                Ok(_) => {
                                    info!("Activity log copied to clipboard.");
                                    self.show_success_toast(
                                        self.t("Activity log copied to clipboard."),
                                    );
                                }
                                Err(err) => {
                                    warn!("Failed to copy activity log: {}", err);
                                    self.show_error_toast(self.t("Failed to copy activity log."));
                                }
                            }
                        }
                        let export_button = ui.add(Button::new(
                            RichText::new(format!("\u{1F4E6}  {}", self.t("Export logs to ZIP")))
                                .size(font_size),
                        ));
                        if export_button.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if export_button.clicked() {
                            info!("Export logs to ZIP triggered from activity log");
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
                        let open_folder_button = ui.add(Button::new(
                            RichText::new(self.t("Open log folder")).size(font_size),
                        ));
                        if open_folder_button.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if open_folder_button.clicked() {
                            info!("Opening log folder from activity log");
                            if !self.open_log_folder() {
                                self.show_error_toast(self.t("Failed to open log folder."));
                            }
                        }
                    });

                    if self.activity_log_cache.is_empty() {
                        ui.add(
                            Label::new(
                                RichText::new(self.t("No activity yet."))
                                    .size(font_size)
                                    .color(self.color_text_dim()),
                            )
                            .wrap(),
                        );
                        return;
                    }

                    let search_filter = MultiEntryFilter::parse(&self.activity_log_search);
                    let filtered_indices: Vec<usize> = self
                        .activity_log_cache
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| {
                            let level_ok = match e.level.as_str() {
                                "ERROR" => self.activity_log_filter_error,
                                "WARN" => self.activity_log_filter_warn,
                                "INFO" => self.activity_log_filter_info,
                                "DEBUG" => self.activity_log_filter_debug,
                                "TRACE" => self.activity_log_filter_trace,
                                _ => true,
                            };
                            let search_ok =
                                search_filter.matches_any(&[e.source.as_str(), e.message.as_str()]);
                            level_ok && search_ok
                        })
                        .map(|(i, _)| i)
                        .collect();

                    let total = self.activity_log_cache.len();
                    let shown = filtered_indices.len();
                    if shown < total {
                        ui.add(
                            Label::new(
                                RichText::new(self.t_fmt(
                                    "Showing {shown} / {total} log entries",
                                    &[("shown", shown.to_string()), ("total", total.to_string())],
                                ))
                                .size(font_size)
                                .color(self.color_text_dim()),
                            )
                            .wrap(),
                        );
                    }

                    // Cache each entry's shaped line keyed by its absolute index
                    // in the (append-only between snapshots) log: filtering only
                    // selects which indices render, so toggling filters never
                    // re-shapes, and a new snapshot bumps `activity_log_generation`
                    // to rebuild. Colors are baked in (folded into the
                    // fingerprint) so the line stays a selectable `Label`.
                    let color_error = self.color_error();
                    let color_warn = self.color_warn();
                    let color_debug = self.color_debug();
                    let color_info = self.color_text_normal();
                    let color_default = self.color_text_dim();
                    let line_font =
                        FontId::new(font_size, TextStyle::Body.resolve(ui.style()).family);
                    let log_fingerprint = galley_cache::fingerprint((
                        font_size.to_bits(),
                        color_error.to_array(),
                        color_warn.to_array(),
                        color_debug.to_array(),
                        color_info.to_array(),
                        color_default.to_array(),
                    ));
                    self.activity_log_galleys.ensure(
                        self.activity_log_cache.len(),
                        1,
                        self.activity_log_generation,
                        log_fingerprint,
                    );

                    ScrollArea::vertical()
                        .id_salt("activity_log_entries")
                        .auto_shrink([false; 2])
                        .stick_to_bottom(true)
                        .show_rows(ui, line_height, filtered_indices.len(), |ui, row_range| {
                            for row_index in row_range {
                                let entry_index = filtered_indices[row_index];
                                let entry = &self.activity_log_cache[entry_index];
                                let color = match entry.level.as_str() {
                                    "ERROR" => color_error,
                                    "WARN" => color_warn,
                                    "DEBUG" => color_debug,
                                    "INFO" => color_info,
                                    _ => color_default,
                                };
                                let galley = galley_cache::lazy_galley_colored(
                                    ui,
                                    self.activity_log_galleys.slot(entry_index, 0),
                                    line_font.clone(),
                                    color,
                                    || {
                                        format!(
                                            "{} [{}] [{}] {}",
                                            entry.timestamp,
                                            entry.level,
                                            entry.source,
                                            entry.message
                                        )
                                    },
                                );
                                ui.add(Label::new(galley));
                            }
                        });
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::activity_log_height_limits;

    #[test]
    fn activity_log_height_limits_reserve_more_space_for_modals() {
        let normal = activity_log_height_limits(800.0, 12.0, 28.0, false);
        let modal = activity_log_height_limits(800.0, 12.0, 28.0, true);

        assert!(modal.max < normal.max);
        assert!(modal.target < normal.target);
    }

    #[test]
    fn activity_log_height_limits_keep_panel_within_available_height() {
        let limits = activity_log_height_limits(180.0, 12.0, 28.0, true);

        assert!(limits.min <= limits.max);
        assert!(limits.target >= limits.min);
        assert!(limits.target <= limits.max);
        assert!(limits.max <= 180.0);
    }
}
