use super::gradient::{blend_header_color, paint_header_gradient};
use crate::ui::app::Foxy;
use crate::ui::i18n::is_rtl;
use eframe::egui::{
    self, Align, Button, Frame, Layout, Margin, RichText, Sense, TextStyle, Ui, Vec2,
    ViewportCommand,
};
use log::info;

impl Foxy {
    pub fn render_app_header(&mut self, ui: &mut Ui) {
        const LOGO_ICON_SIZE: f32 = 24.0;
        const LOGO_PADDING_NEAR: f32 = 10.0;
        const LOGO_TEXT_SPACING: f32 = 12.0;
        const TITLE_PADDING_FAR: f32 = 16.0;
        const REPOSITORY_LIST_PANEL_WIDTH: f32 = 250.0;
        const HEADER_FADE_END_RATIO: f32 = 0.5;
        const HEADER_FADE_END_PADDING: f32 = 8.0;
        const MIN_FADE_WIDTH: f32 = 32.0;
        const HEADER_FADE_START_OFFSET: f32 = 8.0;

        ui.horizontal(|ui| {
            let title = "Foxy";
            if let Some(icon) = &self.app_icon {
                // In RTL mode, swap leading/trailing padding so the logo starts
                // from the right edge and the title trails toward the left.
                let (logo_leading, title_trailing) = if is_rtl() {
                    (TITLE_PADDING_FAR, LOGO_PADDING_NEAR)
                } else {
                    (LOGO_PADDING_NEAR, TITLE_PADDING_FAR)
                };

                let title_color = self.color_text_normal();
                let title_font = TextStyle::Heading.resolve(ui.style());
                let title_galley =
                    ui.painter()
                        .layout_no_wrap(title.to_owned(), title_font.clone(), title_color);
                let title_start_x = logo_leading + LOGO_ICON_SIZE + LOGO_TEXT_SPACING;
                let title_end_x = title_start_x + title_galley.size().x;
                let fade_start_x = (title_start_x - HEADER_FADE_START_OFFSET).max(logo_leading);
                let fade_target_x =
                    (REPOSITORY_LIST_PANEL_WIDTH * HEADER_FADE_END_RATIO) + HEADER_FADE_END_PADDING;
                let fade_width = (fade_target_x - fade_start_x).max(MIN_FADE_WIDTH);
                let brand_size = Vec2::new(
                    (fade_start_x + fade_width).max(title_end_x + title_trailing),
                    ui.available_height().max(LOGO_ICON_SIZE + 12.0),
                );
                let (brand_rect, _) = ui.allocate_exact_size(brand_size, Sense::hover());
                let dark_start = self.color_main_bg();
                let dark_body = dark_start;
                let accent_color = self.color_primary_accent();
                let fade_start_ratio = (fade_start_x / brand_rect.width()).clamp(0.0, 1.0);
                let warm_early = blend_header_color(dark_body, accent_color, 0.10);
                let warm_mid = blend_header_color(dark_body, accent_color, 0.24);
                let warm_late = blend_header_color(dark_body, accent_color, 0.46);
                let fade_early_ratio = fade_start_ratio + (1.0 - fade_start_ratio) * 0.22;
                let fade_mid_ratio = fade_start_ratio + (1.0 - fade_start_ratio) * 0.52;
                let fade_late_ratio = fade_start_ratio + (1.0 - fade_start_ratio) * 0.80;
                paint_header_gradient(
                    ui,
                    brand_rect,
                    &[
                        (0.0, dark_start),
                        (fade_start_ratio * 0.60, dark_body),
                        (fade_start_ratio, dark_body),
                        (fade_early_ratio, warm_early),
                        (fade_mid_ratio, warm_mid),
                        (fade_late_ratio, warm_late),
                        (1.0, accent_color),
                    ],
                );

                let icon_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        brand_rect.left() + logo_leading + (LOGO_ICON_SIZE / 2.0),
                        brand_rect.center().y,
                    ),
                    Vec2::splat(LOGO_ICON_SIZE),
                );
                ui.put(icon_rect, egui::Image::new((icon.id(), icon_rect.size())))
                    .on_hover_text(self.t(
                        "You're pulling the fox by its tail, and that's how you download addons - Foxy's motto",
                    ));
                let title_pos = egui::pos2(
                    icon_rect.right() + LOGO_TEXT_SPACING,
                    brand_rect.center().y - (title_galley.size().y / 2.0),
                );
                ui.painter().galley(title_pos, title_galley, title_color);
            } else {
                ui.heading(title);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let size = self.header_control_button_size();

                let button_frame = Frame {
                    inner_margin: Margin::same(0),
                    ..Default::default()
                };

                if !self.main_view_state.use_window_decorations {
                    if button_frame
                        .show(ui, |ui| {
                            let close_icon = RichText::new("X").size(
                                self.settings_view_state
                                    .font_sizes
                                    .main_view
                                    .window_control_icons as f32,
                            );
                            let button = ui.add_sized(size, Button::new(close_icon).frame(false));
                            if button.hovered() && button.sense.senses_click() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            button.on_hover_text(self.t("Close")).clicked()
                        })
                        .inner
                    {
                        self.request_app_close(ui.ctx(), "app header");
                    }

                    if button_frame
                        .show(ui, |ui| {
                            let maximize_icon = RichText::new("\u{25A1}").size(
                                self.settings_view_state
                                    .font_sizes
                                    .main_view
                                    .window_control_icons as f32,
                            );
                            let button =
                                ui.add_sized(size, Button::new(maximize_icon).frame(false));
                            if button.hovered() && button.sense.senses_click() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            button.on_hover_text(self.t("Maximize")).clicked()
                        })
                        .inner
                    {
                        let (is_minimized, is_maximized, is_focused) = ui.input(|i| {
                            (
                                i.viewport().minimized.unwrap_or(false),
                                i.viewport().maximized.unwrap_or(false),
                                i.viewport().focused.unwrap_or(true),
                            )
                        });
                        if !is_minimized && is_focused {
                            info!("Toggling window maximized state from app header");
                            ui.ctx()
                                .send_viewport_cmd(ViewportCommand::Maximized(!is_maximized));
                        }
                    }

                    if button_frame
                        .show(ui, |ui| {
                            let minimize_icon = RichText::new("\u{2212}").size(
                                self.settings_view_state
                                    .font_sizes
                                    .main_view
                                    .window_control_icons as f32,
                            );
                            let button =
                                ui.add_sized(size, Button::new(minimize_icon).frame(false));
                            if button.hovered() && button.sense.senses_click() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            button.on_hover_text(self.t("Minimize")).clicked()
                        })
                        .inner
                    {
                        let (is_minimized, is_focused) = ui.input(|i| {
                            (
                                i.viewport().minimized.unwrap_or(false),
                                i.viewport().focused.unwrap_or(true),
                            )
                        });
                        if !is_minimized && is_focused {
                            info!("Window minimize requested from app header");
                            ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                        }
                    }
                }

                if button_frame
                    .show(ui, |ui| {
                        let settings_icon = RichText::new("\u{2699}").size(
                            self.settings_view_state
                                .font_sizes
                                .main_view
                                .window_control_icons as f32,
                        );
                        let button = ui.add_sized(size, Button::new(settings_icon).frame(false));
                        if button.hovered() && button.sense.senses_click() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        button
                            .on_hover_text(format!("{} (F2)", self.t("Settings")))
                            .clicked()
                    })
                    .inner
                {
                    info!("Toggling settings view from app header");
                    self.open_settings_view();
                }
            });
        });
    }
}
