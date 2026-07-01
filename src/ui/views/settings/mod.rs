mod app_backup;
mod app_general;
mod app_paths;
mod application;
mod backup;
mod customization;
mod scheduling;
mod tools;
mod ts3_plugins;

use crate::ui::app::Foxy;
use crate::ui::fonts::{self};
use crate::ui::i18n::tr;
use crate::ui::palette::RgbColor;
use eframe::egui::{self, Align, Button, Label, Layout, Margin, RichText, Slider, Ui, Vec2};
use log::info;

impl Foxy {
    pub fn render_settings_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let settings_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };

        let settings_frame = egui::Frame::NONE.inner_margin(settings_margin);

        settings_frame.show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.heading(
                        RichText::new(tr("Settings")).size(
                            self.settings_view_state.font_sizes.settings_view.page_title as f32,
                        ),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close_icon_size =
                            self.settings_view_state.font_sizes.settings_view.close_icon as f32;
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
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }

                        if close_button.clicked() {
                            info!("Closing settings view");
                            self.restore_last_view_or_default();
                        }
                    });
                });

                ui.separator();

                let tabs = [
                    "Application",
                    "Additional search folders",
                    "Cleanup",
                    "Direct download",
                    "Backup Manager",
                    "Scheduling",
                    "TS3 Plugin",
                    "Customization",
                ];
                let selected = tabs
                    .iter()
                    .position(|t| *t == self.settings_view_state.current_tab.as_str())
                    .unwrap_or(0);
                if let Some(idx) = self.render_adaptive_tab_bar(ui, &tabs, selected) {
                    self.settings_view_state.current_tab = tabs[idx].to_string();
                    info!("Switched settings tab to {}", tabs[idx]);
                }

                ui.separator();

                let available_rect = ui.available_rect_before_wrap();
                let card_horizontal_inset = 9.0;
                let card_size = Vec2::new(
                    (available_rect.width() - (card_horizontal_inset * 2.0)).max(0.0),
                    available_rect.height().max(120.0),
                );
                let card_rect = egui::Rect::from_min_size(
                    available_rect.min + egui::vec2(card_horizontal_inset, 0.0),
                    card_size,
                );
                let frame_rect = card_rect.shrink(1.0);
                ui.allocate_rect(card_rect, egui::Sense::hover());
                let mut card_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt((
                            "settings_card",
                            self.settings_view_state.current_tab.as_str(),
                        ))
                        .max_rect(frame_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                card_ui.set_clip_rect(card_rect.expand(2.0));

                egui::Frame::NONE
                    .fill(self.color_card_bg())
                    .corner_radius(eframe::egui::CornerRadius::same(10))
                    .inner_margin(Margin::same(15))
                    .show(&mut card_ui, |ui| {
                        match self.settings_view_state.current_tab.as_str() {
                            "Application" => self.render_application_settings(ui),
                            "Scheduling" => self.render_scheduling_settings(ui),
                            "Backup Manager" => self.render_backup_manager_settings(ui),
                            "Additional search folders" => {
                                self.render_additional_search_folders(ui)
                            }
                            "Cleanup" => self.render_cleanup_settings(ui),
                            "Direct download" => self.render_direct_download_settings(ui),
                            "TS3 Plugin" | "TS3 Plugins" => self.render_ts3_plugins_settings(ui),
                            "Customization" => {
                                self.render_customization_settings(ui, card_size.y - 30.0)
                            }
                            _ => {}
                        }
                    });
                ui.painter().rect_stroke(
                    frame_rect,
                    egui::CornerRadius::same(10),
                    egui::Stroke::new(1.0, self.color_text_gray()),
                    egui::StrokeKind::Inside,
                );
            });
        });

        self.render_settings_folder_removal_confirmation(ui);
    }
}

pub(super) fn render_wrapped_info_row(ui: &mut Ui, horizontal_padding: f32, text: RichText) {
    ui.horizontal(|ui| {
        ui.add_space(horizontal_padding);
        let width = (ui.available_width() - horizontal_padding).max(0.0);
        ui.add_sized(Vec2::new(width, 0.0), Label::new(text).wrap());
    });
}

fn render_font_size_slider(
    ui: &mut Ui,
    label: String,
    value: &mut u16,
    range: fonts::FontSizeRange,
    horizontal_padding: f32,
) -> bool {
    ui.horizontal(|ui| {
        ui.add_space(horizontal_padding);
        ui.label(label);
        let response = ui.add(
            Slider::new(value, range.min..=range.max)
                .show_value(true)
                .trailing_fill(true),
        );
        if response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        ui.add_space(horizontal_padding);
        response.changed()
    })
    .inner
}

fn render_palette_color_picker(
    ui: &mut Ui,
    label: String,
    value: &mut RgbColor,
    horizontal_padding: f32,
) -> bool {
    ui.horizontal(|ui| {
        ui.add_space(horizontal_padding);
        let row_width = (ui.available_width() - horizontal_padding).max(0.0);
        ui.allocate_ui_with_layout(
            Vec2::new(row_width, 0.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(label);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Small gap so the color button doesn't sit flush against the
                    // card's right border.
                    ui.add_space(3.0);
                    let mut color = value.to_color32();
                    let response = ui.color_edit_button_srgba(&mut color);
                    if response.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if response.changed() {
                        *value = RgbColor::from_color32(color);
                        return true;
                    }
                    false
                })
                .inner
            },
        )
        .inner
    })
    .inner
}
