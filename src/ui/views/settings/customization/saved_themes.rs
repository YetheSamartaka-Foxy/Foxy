use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::theme::Theme;
use eframe::egui::{Button, Key, RichText, ScrollArea, TextEdit, Ui, Vec2};

impl Foxy {
    pub(super) fn render_saved_themes(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Saved custom themes"));
        });

        if self.settings_view_state.selected_saved_theme.is_some() {
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.add(
                    TextEdit::singleline(&mut self.settings_view_state.saved_theme_name_draft)
                        .hint_text(tr("Theme name"))
                        .desired_width(ui.available_width() - horizontal_padding),
                );
                ui.add_space(horizontal_padding);
            });
        }

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let list_width = ui.available_width() - horizontal_padding;
            let theme_count = self.settings_view_state.saved_themes.len();
            let list_height = if theme_count == 0 {
                24.0
            } else {
                (theme_count as f32 * 22.0).clamp(28.0, 96.0)
            };
            ui.allocate_ui(Vec2::new(list_width, list_height), |ui| {
                ScrollArea::vertical()
                    .id_salt("saved_custom_themes")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.settings_view_state.saved_themes.is_empty() {
                            ui.label(
                                RichText::new(tr("No custom themes saved yet."))
                                    .italics()
                                    .color(self.color_text_dim()),
                            );
                            return;
                        }

                        let names = self
                            .settings_view_state
                            .saved_themes
                            .iter()
                            .map(|theme| theme.name.clone())
                            .collect::<Vec<_>>();
                        let mut focused_index = None;
                        let mut response_ids = Vec::with_capacity(names.len());
                        for (index, name) in names.iter().enumerate() {
                            let selected =
                                self.settings_view_state.selected_saved_theme == Some(index);
                            let response = ui
                                .selectable_label(selected, name)
                                .on_hover_text(tr("Select this theme for loading or editing."));
                            if response.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response.clicked() {
                                self.select_saved_theme(index);
                            }
                            if response.has_focus() {
                                focused_index = Some(index);
                            }
                            response_ids.push(response.id);
                        }

                        if let Some(index) = focused_index {
                            let move_up = ui.input(|input| input.key_pressed(Key::ArrowUp));
                            let move_down = ui.input(|input| input.key_pressed(Key::ArrowDown));
                            let target = if move_up {
                                index.checked_sub(1)
                            } else if move_down && index + 1 < names.len() {
                                Some(index + 1)
                            } else {
                                None
                            };
                            if let Some(target) = target {
                                self.select_saved_theme(target);
                                ui.memory_mut(|memory| memory.request_focus(response_ids[target]));
                            }
                        }
                    });
            });
            ui.add_space(horizontal_padding);
        });

        ui.horizontal_wrapped(|ui| {
            ui.add_space(horizontal_padding);
            let has_selection = self.settings_view_state.selected_saved_theme.is_some();
            let add_new = ui
                .add(Button::new(tr("Add new theme")).fill(self.color_main_bg()))
                .on_hover_text(tr(
                    "Create a named theme from the current font sizes and colors.",
                ));
            let load = ui
                .add_enabled(
                    has_selection,
                    Button::new(tr("Load selected theme")).fill(self.color_main_bg()),
                )
                .on_hover_text(tr("Apply the selected saved theme."));
            let overwrite = ui
                .add_enabled(
                    has_selection,
                    Button::new(tr("Save changes")).fill(self.color_main_bg()),
                )
                .on_hover_text(tr(
                    "Overwrite the selected theme with the current font sizes and colors.",
                ));
            let delete = ui
                .add_enabled(
                    has_selection,
                    Button::new(tr("Delete theme")).fill(self.color_action_destructive()),
                )
                .on_hover_text(tr("Delete the selected saved theme."));
            for response in [&add_new, &load, &overwrite, &delete] {
                if response.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
            }

            if add_new.clicked() {
                self.settings_view_state.new_theme_name_draft.clear();
                self.settings_view_state.show_add_theme_modal = true;
                self.settings_view_state.focus_new_theme_name = true;
            }
            if load.clicked() {
                self.load_selected_theme();
            }
            if overwrite.clicked() {
                self.save_selected_theme();
            }
            if delete.clicked() {
                self.delete_selected_theme();
            }
            ui.add_space(horizontal_padding);
        });

        self.render_add_theme_modal(ui);
    }

    fn select_saved_theme(&mut self, index: usize) {
        let Some(theme) = self.settings_view_state.saved_themes.get(index) else {
            return;
        };
        self.settings_view_state.selected_saved_theme = Some(index);
        self.settings_view_state.saved_theme_name_draft = theme.name.clone();
    }

    fn save_selected_theme(&mut self) {
        let name = self
            .settings_view_state
            .saved_theme_name_draft
            .trim()
            .to_string();
        if name.is_empty() {
            self.show_error_toast(self.t("Enter a theme name first."));
            return;
        }

        let selected = self.settings_view_state.selected_saved_theme;
        if self
            .settings_view_state
            .saved_themes
            .iter()
            .enumerate()
            .any(|(index, theme)| theme.name.eq_ignore_ascii_case(&name) && Some(index) != selected)
        {
            self.show_error_toast(self.t("A saved theme with this name already exists."));
            return;
        }

        let theme = Theme::from_current(
            name.clone(),
            self.settings_view_state.font_sizes.clone(),
            self.settings_view_state.palette_colors.clone(),
        );
        let Some(index) = selected else {
            return;
        };
        let Some(saved_theme) = self.settings_view_state.saved_themes.get_mut(index) else {
            self.settings_view_state.selected_saved_theme = None;
            return;
        };
        *saved_theme = theme;
        self.show_success_toast(self.t("Saved theme updated."));
        self.settings_view_state.saved_theme_name_draft = name;
        self.save_settings();
    }

    fn render_add_theme_modal(&mut self, ui: &mut Ui) {
        if !self.settings_view_state.show_add_theme_modal {
            return;
        }

        let mut save_requested = false;
        let mut cancel_requested = ui.input(|input| input.key_pressed(Key::Escape));
        eframe::egui::Window::new(tr("Add new theme"))
            .frame(
                eframe::egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(eframe::egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(eframe::egui::CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(eframe::egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(360.0)
            .show(ui.ctx(), |ui| {
                ui.label(tr("Theme name"));
                let name_edit = ui.add(
                    TextEdit::singleline(&mut self.settings_view_state.new_theme_name_draft)
                        .desired_width(ui.available_width()),
                );
                if self.settings_view_state.focus_new_theme_name {
                    name_edit.request_focus();
                    self.settings_view_state.focus_new_theme_name = false;
                }
                if name_edit.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                    save_requested = true;
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let save = ui.button(tr("Save"));
                    let cancel = ui.button(tr("Cancel"));
                    for response in [&save, &cancel] {
                        if response.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                    }
                    save_requested |= save.clicked();
                    cancel_requested |= cancel.clicked();
                });
            });

        let should_close = cancel_requested || (save_requested && self.save_new_theme());
        if should_close {
            self.close_add_theme_modal();
        }
    }

    fn save_new_theme(&mut self) -> bool {
        let name = self
            .settings_view_state
            .new_theme_name_draft
            .trim()
            .to_string();
        if name.is_empty() {
            self.show_error_toast(self.t("Enter a theme name first."));
            return false;
        }
        if self
            .settings_view_state
            .saved_themes
            .iter()
            .any(|theme| theme.name.eq_ignore_ascii_case(&name))
        {
            self.show_error_toast(self.t("A saved theme with this name already exists."));
            return false;
        }

        self.settings_view_state
            .saved_themes
            .push(Theme::from_current(
                name.clone(),
                self.settings_view_state.font_sizes.clone(),
                self.settings_view_state.palette_colors.clone(),
            ));
        self.settings_view_state.selected_saved_theme =
            Some(self.settings_view_state.saved_themes.len() - 1);
        self.settings_view_state.saved_theme_name_draft = name;
        self.save_settings();
        self.show_success_toast(self.t("Custom theme saved."));
        true
    }

    fn close_add_theme_modal(&mut self) {
        self.settings_view_state.show_add_theme_modal = false;
        self.settings_view_state.new_theme_name_draft.clear();
        self.settings_view_state.focus_new_theme_name = false;
    }

    fn load_selected_theme(&mut self) {
        let Some(index) = self.settings_view_state.selected_saved_theme else {
            return;
        };
        let Some(theme) = self.settings_view_state.saved_themes.get(index).cloned() else {
            self.settings_view_state.selected_saved_theme = None;
            return;
        };
        self.apply_theme(theme);
        self.show_success_toast(self.t("Saved theme loaded."));
    }

    fn delete_selected_theme(&mut self) {
        let Some(index) = self.settings_view_state.selected_saved_theme else {
            return;
        };
        if index >= self.settings_view_state.saved_themes.len() {
            self.settings_view_state.selected_saved_theme = None;
            return;
        }

        self.settings_view_state.saved_themes.remove(index);
        if self.settings_view_state.saved_themes.is_empty() {
            self.settings_view_state.selected_saved_theme = None;
            self.settings_view_state.saved_theme_name_draft.clear();
        } else {
            self.select_saved_theme(index.min(self.settings_view_state.saved_themes.len() - 1));
        }
        self.save_settings();
        self.show_success_toast(self.t("Saved theme deleted."));
    }
}
