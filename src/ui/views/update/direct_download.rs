use super::*;

impl Foxy {
    pub(super) fn render_direct_download_update_view(&mut self, ui: &mut Ui) {
        let Some(session) = self.direct_download_session.clone() else {
            self.update_modal_open = false;
            self.direct_download_update_view = false;
            return;
        };

        let running = session.is_running();
        let finished_successfully = session.finished_successfully();
        let progress_pct: f32 = self
            .download_progress
            .as_ref()
            .map(|(_, percent)| (*percent).clamp(0.0, 1.0))
            .unwrap_or(if running { 0.0 } else { 1.0 });

        let total_bytes = session.total_bytes;
        let downloaded_bytes = session.downloaded_bytes;
        let stage_label = self
            .download_progress
            .as_ref()
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| self.t("Preparing direct download"));
        let stage_text = if running {
            self.t_fmt("Downloading {name}", &[("name", stage_label.clone())])
        } else {
            stage_label.clone()
        };

        let mut set_download_paused: Option<bool> = None;
        let mut cancel_requested = false;

        let outer_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };
        Frame::NONE.inner_margin(outer_margin).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let close_icon_size =
                        self.settings_view_state.font_sizes.update_view.close_icon as f32;
                    ui.heading(RichText::new(self.t("Direct download")).size(
                        self.settings_view_state.font_sizes.update_view.page_title as f32,
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close_button = ui.add_sized(
                            Self::modal_icon_button_size(close_icon_size),
                            Button::new(
                                RichText::new("X")
                                    .color(self.color_text_normal())
                                    .size(close_icon_size),
                            )
                            .fill(self.color_main_bg()),
                        );
                        if close_button.hovered() {
                            ui.ctx()
                                .output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if close_button.clicked() {
                            self.update_modal_open = false;
                            self.direct_download_update_view = false;
                        }
                    });
                });

                ui.separator();
                ui.label(self.t_fmt("Source: {url}", &[("url", session.source_url.clone())]));
                ui.label(
                    self.t_fmt("Destination: {path}", &[("path", session.destination_folder.clone())]),
                );
                ui.label(
                    self.t_fmt("Target: {name}", &[("name", session.target_label.clone())]),
                );
                ui.label(self.t_fmt(
                    "Files: {done}/{total}",
                    &[
                        ("done", session.files_done.to_string()),
                        ("total", session.files_total.to_string()),
                    ],
                ));
                ui.label(self.t_fmt(
                    "Downloaded: {size}",
                    &[("size", fmt_bytes(downloaded_bytes))],
                ));
                if total_bytes > 0 {
                    ui.label(self.t_fmt(
                        "Total size: {size}",
                        &[("size", fmt_bytes(total_bytes))],
                    ));
                }
                ui.add_space(10.0);
                ui.label(stage_text.clone());
                ui.add(
                    ProgressBar::new(progress_pct)
                        .fill(self.color_primary_accent())
                        .show_percentage()
                        .text(format!("{:.1}%", progress_pct * 100.0)),
                );

                if let Some(error) = &session.error_message {
                    ui.add_space(8.0);
                    ui.colored_label(self.color_text_error(), error);
                }

                ui.separator();
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let can_toggle_pause = running;
                    let can_cancel = running;
                    let pause_button_font_size =
                        self.settings_view_state.font_sizes.update_view.pause_button as f32;
                    let pause_button_width = if can_toggle_pause {
                        (pause_button_font_size * 8.5).max(150.0)
                    } else {
                        0.0
                    };
                    let cancel_button_width = if can_cancel {
                        (pause_button_font_size * 6.5).max(120.0)
                    } else {
                        0.0
                    };
                    let pause_spacing = if can_toggle_pause { 8.0 } else { 0.0 };
                    let cancel_spacing = if can_cancel { 8.0 } else { 0.0 };
                    let button_width = (ui.available_width()
                        - pause_button_width
                        - pause_spacing
                        - cancel_button_width
                        - cancel_spacing)
                        .max(220.0);
                    let button_text = if running {
                        let speed_text = fmt_speed_mbps(self.download_speed_bps);
                        let elapsed_text = self
                            .download_started_at
                            .map(|t| fmt_duration(t.elapsed()))
                            .unwrap_or_else(|| "0s".to_string());
                        let remaining_text = self
                            .download_eta_remaining
                            .unwrap_or_else(|| Duration::from_secs(0));
                        self.update_download_estimate(total_bytes);
                        self.t_fmt(
                            "{stage} {percent}% - {speed} - {elapsed} elapsed / {remaining} remaining",
                            &[
                                ("stage", stage_text),
                                ("percent", format!("{:.1}", progress_pct * 100.0)),
                                ("speed", speed_text),
                                ("elapsed", elapsed_text),
                                ("remaining", fmt_duration(remaining_text)),
                            ],
                        )
                    } else if finished_successfully {
                        self.t("Download finished - click to close")
                    } else {
                        self.t("Download failed - click to close")
                    };

                    let action_button = ui.add_sized(
                        Vec2::new(button_width, 48.0),
                        Button::new(button_text).fill(if running {
                            self.color_primary_accent()
                        } else if finished_successfully {
                            self.color_success()
                        } else {
                            self.color_text_error()
                        }),
                    );
                    if action_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if action_button.clicked() && !running {
                        self.update_modal_open = false;
                        self.direct_download_update_view = false;
                    }

                    if can_toggle_pause {
                        ui.add_space(pause_spacing);
                        let pause_label = if self.download_paused {
                            self.t("Resume download")
                        } else {
                            self.t("Pause download")
                        };
                        let pause_button = ui.add_sized(
                            Vec2::new(pause_button_width, 48.0),
                            Button::new(RichText::new(pause_label).size(pause_button_font_size))
                                .fill(if self.download_paused {
                                    self.color_primary_accent()
                                } else {
                                    self.color_widget_bg()
                                }),
                        );
                        if pause_button.hovered() {
                            ui.ctx()
                                .output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if pause_button.clicked() {
                            set_download_paused = Some(!self.download_paused);
                        }
                    }

                    if can_cancel {
                        ui.add_space(cancel_spacing);
                        let cancel_label = self.t("Cancel");
                        let cancel_button = ui.add_sized(
                            Vec2::new(cancel_button_width, 48.0),
                            Button::new(
                                RichText::new(cancel_label).size(pause_button_font_size),
                            )
                            .fill(self.color_action_destructive()),
                        );
                        if cancel_button.hovered() {
                            ui.ctx()
                                .output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if cancel_button.clicked() {
                            cancel_requested = true;
                        }
                    }
                });
            });
        });

        if cancel_requested {
            self.cancel_direct_download();
        }
        if let Some(paused) = set_download_paused {
            self.set_download_paused(paused);
        }
    }
}
