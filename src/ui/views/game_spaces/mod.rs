pub mod settings;

use eframe::egui::{self, Button, Key, Label, Margin, RichText, ScrollArea, TextEdit, Ui, Vec2};
use log::{info, warn};

use crate::core::game::spaces::{self, GameSpaceEntry};
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::types::FoxyView;

#[derive(Default)]
pub struct GameSpacesViewState {
    pub entries: Vec<GameSpaceEntry>,
    pub active_space_id: String,
    pub selected_index: usize,
    pub load_error: Option<String>,
    pub show_create_modal: bool,
    pub create_name: String,
    pub create_game_id: String,
    pub focus_create_name: bool,
    pub pending_open: Option<GameSpaceEntry>,
    pub pending_remove: Option<GameSpaceEntry>,
}

impl Foxy {
    pub(crate) fn render_game_space_header(&mut self, ui: &mut Ui) {
        let display_name = spaces::active_game_space().display_name.clone();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(display_name)
                    .strong()
                    .color(self.color_text_normal()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let switch_button = ui
                    .button(tr("Switch"))
                    .on_hover_text(tr("Switch game space"));
                if switch_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if switch_button.clicked() {
                    self.open_game_spaces_view();
                }
                let settings_button = ui
                    .button("\u{2699}")
                    .on_hover_text(tr("Game space settings"));
                if settings_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if settings_button.clicked() {
                    self.open_active_game_space_settings();
                }
            });
        });
        ui.add_space(4.0);
    }

    pub fn open_game_spaces_view(&mut self) {
        if self.current_view == FoxyView::GameSpaces {
            self.restore_last_view_or_default();
            return;
        }
        self.last_view = self.current_view;
        self.current_view = FoxyView::GameSpaces;
        self.reload_game_spaces();
        info!("Opened game spaces view");
    }

    fn reload_game_spaces(&mut self) {
        let state = &mut self.game_spaces_view_state;
        match spaces::load_registry() {
            Ok(registry) => {
                state.active_space_id = registry.active_game_space_id.clone();
                state.entries = registry.game_spaces;
                state.load_error = None;
                state.selected_index = state
                    .entries
                    .iter()
                    .position(|entry| entry.id == state.active_space_id)
                    .unwrap_or(0);
            }
            Err(err) => {
                warn!("Failed to load the games registry: {}", err);
                state.entries.clear();
                state.load_error = Some(err);
            }
        }
        state.pending_open = None;
        state.pending_remove = None;
        state.show_create_modal = false;
    }

    pub fn render_game_spaces_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };
        egui::Frame::NONE.inner_margin(margin).show(ui, |ui| {
            ui.vertical(|ui| {
                self.render_game_spaces_header(ui);
                ui.separator();
                self.render_game_spaces_body(ui);
            });
        });
        self.render_game_space_open_confirmation(ui);
        self.render_game_space_remove_confirmation(ui);
        self.render_game_space_create_modal(ui);
    }

    fn render_game_spaces_header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading(
                RichText::new(tr("Game spaces"))
                    .size(self.settings_view_state.font_sizes.settings_view.page_title as f32),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    info!("Closing game spaces view");
                    self.restore_last_view_or_default();
                }
            });
        });
    }

    fn render_game_spaces_body(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let width = (ui.available_width() - horizontal_padding).max(0.0);
            ui.add_sized(
                Vec2::new(width, 0.0),
                Label::new(
                    RichText::new(format!(
                        "{} {}",
                        '\u{2139}',
                        tr("A game space is a separate workspace with its own repositories, settings, and database. Opening another game space switches to it right away, without restarting Foxy.")
                    ))
                    .italics()
                    .color(self.color_text_dim()),
                )
                .wrap(),
            );
        });
        ui.separator();

        if let Some(error) = self.game_spaces_view_state.load_error.clone() {
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(RichText::new(error).color(self.color_text_error()));
            });
            return;
        }

        self.handle_game_spaces_keyboard(ui);

        let entries = self.game_spaces_view_state.entries.clone();
        let selected_index = self
            .game_spaces_view_state
            .selected_index
            .min(entries.len().saturating_sub(1));
        self.game_spaces_view_state.selected_index = selected_index;
        let switch_block_reason = self.game_space_switch_block_reason();
        let switch_blocked = switch_block_reason.is_some();

        let mut open_requested: Option<GameSpaceEntry> = None;
        let mut remove_requested: Option<GameSpaceEntry> = None;
        let mut settings_requested: Option<GameSpaceEntry> = None;
        let mut select_requested: Option<usize> = None;

        ScrollArea::vertical()
            .id_salt("game_spaces_list")
            .show(ui, |ui| {
                for (index, entry) in entries.iter().enumerate() {
                    let is_active = entry.id == self.game_spaces_view_state.active_space_id;
                    let is_selected = index == selected_index;
                    let module_name = crate::core::game::registry()
                        .get(&entry.game_id)
                        .map(|module| module.display_name().to_string());
                    let mut action_rect: Option<egui::Rect> = None;

                    let row_fill = if is_selected {
                        self.color_card_bg()
                    } else {
                        self.color_main_bg()
                    };
                    let row_stroke = if is_selected {
                        egui::Stroke::new(1.0, self.color_text_normal())
                    } else {
                        egui::Stroke::new(1.0, self.color_text_gray())
                    };
                    let frame_response = egui::Frame::NONE
                        .fill(row_fill)
                        .stroke(row_stroke)
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(&entry.display_name)
                                                .strong()
                                                .color(self.color_text_normal()),
                                        );
                                        if is_active {
                                            ui.label(
                                                RichText::new(tr("Active"))
                                                    .color(self.color_success()),
                                            );
                                        }
                                    });
                                    let detail = match module_name {
                                        Some(name) => format!("{} - {}", name, entry.id),
                                        None => self.t_fmt(
                                            "Unknown game {game_id}",
                                            &[("game_id", entry.game_id.clone())],
                                        ),
                                    };
                                    ui.label(
                                        RichText::new(detail).small().color(self.color_text_dim()),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if !is_active {
                                            let open_button = ui
                                                .add_enabled(
                                                    !switch_blocked,
                                                    Button::new(tr("Open")),
                                                )
                                                .on_disabled_hover_text(tr(
                                                    switch_block_reason.unwrap_or(
                                                        "Finish downloads and scans before switching game spaces.",
                                                    ),
                                                ));
                                            if open_button.hovered() {
                                                ui.ctx().output_mut(
                                                    Foxy::set_pointing_cursor_output,
                                                );
                                            }
                                            if open_button.clicked() {
                                                open_requested = Some(entry.clone());
                                            }
                                            action_rect = Some(open_button.rect);

                                            let remove_button = ui.button(tr("Remove"));
                                            if remove_button.hovered() {
                                                ui.ctx().output_mut(
                                                    Foxy::set_pointing_cursor_output,
                                                );
                                            }
                                            if remove_button.clicked() {
                                                remove_requested = Some(entry.clone());
                                            }
                                            action_rect = Some(
                                                action_rect
                                                    .unwrap_or(remove_button.rect)
                                                    .union(remove_button.rect),
                                            );
                                        }

                                        let settings_button = ui
                                            .button("\u{2699}")
                                            .on_hover_text(tr("Game space settings"));
                                        if settings_button.hovered() {
                                            ui.ctx().output_mut(
                                                Foxy::set_pointing_cursor_output,
                                            );
                                        }
                                        if settings_button.clicked() {
                                            settings_requested = Some(entry.clone());
                                        }
                                        action_rect = Some(
                                            action_rect
                                                .unwrap_or(settings_button.rect)
                                                .union(settings_button.rect),
                                        );
                                    },
                                );
                            });
                        })
                        .response;
                    let mut row_click_rect = frame_response.rect;
                    if let Some(action_rect) = action_rect {
                        row_click_rect.max.x = row_click_rect
                            .max
                            .x
                            .min(action_rect.min.x - ui.spacing().item_spacing.x);
                    }
                    if row_click_rect.width() > 0.0 {
                        let row_response = ui.interact(
                            row_click_rect,
                            ui.make_persistent_id(("game_space_row", &entry.id)),
                            egui::Sense::click(),
                        );
                        if row_response.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if row_response.clicked() {
                            select_requested = Some(index);
                        }
                    }
                    ui.add_space(6.0);
                }
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let create_button = ui.add_sized(
                Vec2::new(
                    (ui.available_width() - 2.0 * horizontal_padding).max(0.0),
                    30.0,
                ),
                Button::new(format!("+ {}", tr("Create game space"))),
            );
            if create_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if create_button.clicked() {
                let state = &mut self.game_spaces_view_state;
                state.show_create_modal = true;
                state.create_name.clear();
                state.create_game_id = crate::core::game::registry()
                    .available()
                    .next()
                    .map(|module| module.id().to_string())
                    .unwrap_or_default();
                state.focus_create_name = true;
            }
            ui.add_space(horizontal_padding);
        });

        if let Some(index) = select_requested {
            self.game_spaces_view_state.selected_index = index;
        }
        if let Some(entry) = open_requested {
            self.game_spaces_view_state.pending_open = Some(entry);
        }
        if let Some(entry) = remove_requested {
            self.game_spaces_view_state.pending_remove = Some(entry);
        }
        if let Some(entry) = settings_requested {
            self.open_game_space_settings(&entry);
        }
    }

    fn handle_game_spaces_keyboard(&mut self, ui: &Ui) {
        let state = &self.game_spaces_view_state;
        if state.show_create_modal
            || state.pending_open.is_some()
            || state.pending_remove.is_some()
            || state.entries.is_empty()
            || ui.ctx().egui_wants_keyboard_input()
        {
            return;
        }

        let (down, up, enter) = ui.ctx().input(|input| {
            (
                input.key_pressed(Key::ArrowDown),
                input.key_pressed(Key::ArrowUp),
                input.key_pressed(Key::Enter),
            )
        });
        let last_index = self.game_spaces_view_state.entries.len() - 1;
        if down {
            let state = &mut self.game_spaces_view_state;
            state.selected_index = (state.selected_index + 1).min(last_index);
        }
        if up {
            let state = &mut self.game_spaces_view_state;
            state.selected_index = state.selected_index.saturating_sub(1);
        }
        if enter {
            let state = &mut self.game_spaces_view_state;
            if let Some(entry) = state.entries.get(state.selected_index).cloned()
                && entry.id != state.active_space_id
            {
                state.pending_open = Some(entry);
            }
        }
    }

    /// Enter = default action, Escape = cancel, for the game-space modals
    /// (keyboard parity per the accessibility conventions). Consumes the keys
    /// so the list navigation underneath never sees them.
    fn game_space_modal_keys(ui: &Ui) -> (bool, bool) {
        ui.ctx().input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, Key::Enter),
                input.consume_key(egui::Modifiers::NONE, Key::Escape),
            )
        })
    }

    fn render_game_space_open_confirmation(&mut self, ui: &mut Ui) {
        let Some(entry) = self.game_spaces_view_state.pending_open.clone() else {
            return;
        };
        let (confirm_key, cancel_key) = Self::game_space_modal_keys(ui);
        let mut close_modal = cancel_key;
        egui::Window::new(tr("Open game space"))
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(egui::CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(500.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(self.t_fmt(
                        "Open game space {name}?",
                        &[("name", entry.display_name.clone())],
                    ));
                    ui.label(tr(
                        "Foxy saves any pending changes and switches to the selected game space.",
                    ));
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(
                            egui::Layout::centered_and_justified(egui::Direction::TopDown),
                            |ui| {
                                let open_button = ui.button(tr("Yes, Open"));
                                if open_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if open_button.clicked() || confirm_key {
                                    close_modal = true;
                                    self.start_game_space_switch(&entry);
                                }
                                let cancel_button = ui.button(tr("Cancel"));
                                if cancel_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if cancel_button.clicked() {
                                    close_modal = true;
                                }
                            },
                        );
                    });
                });
            });
        if close_modal {
            self.game_spaces_view_state.pending_open = None;
        }
    }

    fn render_game_space_remove_confirmation(&mut self, ui: &mut Ui) {
        let Some(entry) = self.game_spaces_view_state.pending_remove.clone() else {
            return;
        };
        let (confirm_key, cancel_key) = Self::game_space_modal_keys(ui);
        let mut close_modal = cancel_key;
        egui::Window::new(tr("Remove game space"))
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(egui::CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(500.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(self.t_fmt(
                        "Remove game space {name}?",
                        &[("name", entry.display_name.clone())],
                    ));
                    ui.label(tr(
                        "This deletes the Foxy workspace for this game space: its repository list, game settings, and local database. Game installs and downloaded mods are not deleted.",
                    ));
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(
                            egui::Layout::centered_and_justified(egui::Direction::TopDown),
                            |ui| {
                                let remove_button = ui.button(tr("Yes, Remove"));
                                if remove_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if remove_button.clicked() || confirm_key {
                                    close_modal = true;
                                    match spaces::remove_game_space(&entry.id) {
                                        Ok(removed) => {
                                            info!("Removed game space {}", removed.id);
                                            self.show_success_toast(self.t_fmt(
                                                "Game space {name} removed.",
                                                &[("name", removed.display_name)],
                                            ));
                                        }
                                        Err(err) => {
                                            warn!("Failed to remove game space: {}", err);
                                            self.show_error_toast(self.t_fmt(
                                                "Could not remove game space: {error}",
                                                &[("error", err)],
                                            ));
                                        }
                                    }
                                    self.reload_game_spaces();
                                }
                                let cancel_button = ui.button(tr("Cancel"));
                                if cancel_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if cancel_button.clicked() {
                                    close_modal = true;
                                }
                            },
                        );
                    });
                });
            });
        if close_modal {
            self.game_spaces_view_state.pending_remove = None;
        }
    }

    fn render_game_space_create_modal(&mut self, ui: &mut Ui) {
        if !self.game_spaces_view_state.show_create_modal {
            return;
        }
        let mut close_modal = ui
            .ctx()
            .input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape));
        let mut create_requested = false;
        egui::Window::new(tr("Create game space"))
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(egui::CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(420.0)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("Game"));
                    let selected_label = crate::core::game::registry()
                        .get(&self.game_spaces_view_state.create_game_id)
                        .map(|module| module.display_name().to_string())
                        .unwrap_or_default();
                    egui::ComboBox::from_id_salt("game_space_create_game")
                        .selected_text(selected_label)
                        .show_ui(ui, |ui| {
                            for module in crate::core::game::registry().available() {
                                ui.selectable_value(
                                    &mut self.game_spaces_view_state.create_game_id,
                                    module.id().to_string(),
                                    module.display_name().to_string(),
                                );
                            }
                        });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(tr("Name"));
                    let name_edit = ui.add(
                        TextEdit::singleline(&mut self.game_spaces_view_state.create_name)
                            .id_salt("game_space_create_name")
                            .desired_width(260.0),
                    );
                    if self.game_spaces_view_state.focus_create_name {
                        name_edit.request_focus();
                        self.game_spaces_view_state.focus_create_name = false;
                    }
                    if name_edit.lost_focus() && ui.ctx().input(|i| i.key_pressed(Key::Enter)) {
                        create_requested = true;
                    }
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            let create_button = ui.add_enabled(
                                !self.game_spaces_view_state.create_name.trim().is_empty(),
                                Button::new(tr("Create")),
                            );
                            if create_button.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if create_button.clicked() {
                                create_requested = true;
                            }
                            let cancel_button = ui.button(tr("Cancel"));
                            if cancel_button.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if cancel_button.clicked() {
                                close_modal = true;
                            }
                        },
                    );
                });
            });
        if create_requested && !self.game_spaces_view_state.create_name.trim().is_empty() {
            close_modal = true;
            self.create_game_space_from_input();
        }
        if close_modal {
            self.game_spaces_view_state.show_create_modal = false;
        }
    }

    fn create_game_space_from_input(&mut self) {
        let game_id = self.game_spaces_view_state.create_game_id.clone();
        let name = self.game_spaces_view_state.create_name.trim().to_string();
        match spaces::create_game_space(&game_id, &name) {
            Ok(entry) => {
                info!("Created game space {} for game {}", entry.id, entry.game_id);
                spaces::seed_new_game_space_settings(
                    &entry,
                    &self.settings_view_state.steam_directory,
                );
                self.show_success_toast(self.t_fmt(
                    "Game space {name} created.",
                    &[("name", entry.display_name.clone())],
                ));
                self.reload_game_spaces();
            }
            Err(err) => {
                warn!("Failed to create game space: {}", err);
                self.show_error_toast(
                    self.t_fmt("Could not create game space: {error}", &[("error", err)]),
                );
            }
        }
    }
}
