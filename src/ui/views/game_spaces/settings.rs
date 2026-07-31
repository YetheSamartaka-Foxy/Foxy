use eframe::egui::{self, Align, Button, Layout, Margin, RichText, Ui, Vec2};
use log::{info, warn};

use crate::core::game::spaces::{self, GameSpaceEntry};
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::types::{FoxyView, SettingsViewState};

use super::super::settings::render_wrapped_info_row;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameSpaceSettingsTab {
    #[default]
    Game,
    Profiles,
    SearchFolders,
    Ts3Plugin,
}

/// State for the per-game-space settings view. When the target space is not
/// the active one, edits go through `snapshot` and are written straight to
/// that space's `game_settings.json`; the active space edits the live
/// settings state and saves through the normal persistence queue.
#[derive(Default)]
pub struct GameSpaceSettingsViewState {
    pub space: Option<GameSpaceEntry>,
    pub is_active_space: bool,
    pub snapshot: SettingsViewState,
    pub current_tab: GameSpaceSettingsTab,
}

impl Foxy {
    pub fn open_game_space_settings(&mut self, entry: &GameSpaceEntry) {
        let is_active = entry.id == spaces::active_game_space().space_id;
        let snapshot = if is_active {
            SettingsViewState::default()
        } else {
            match Self::load_game_space_settings_snapshot(entry) {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    warn!("Failed to load game space settings: {}", err);
                    self.show_error_toast(self.t_fmt(
                        "Could not load game space settings: {error}",
                        &[("error", err)],
                    ));
                    return;
                }
            }
        };
        let state = &mut self.game_space_settings_view_state;
        state.space = Some(entry.clone());
        state.is_active_space = is_active;
        state.snapshot = snapshot;
        state.current_tab = GameSpaceSettingsTab::Game;
        if self.current_view != FoxyView::GameSpaceSettings {
            self.last_view = self.current_view;
            self.current_view = FoxyView::GameSpaceSettings;
        }
        info!("Opened game space settings for {}", entry.id);
    }

    pub fn open_active_game_space_settings(&mut self) {
        let active = spaces::active_game_space();
        let entry = spaces::load_registry()
            .ok()
            .and_then(|registry| {
                registry
                    .game_spaces
                    .into_iter()
                    .find(|entry| entry.id == active.space_id)
            })
            .unwrap_or_else(|| GameSpaceEntry {
                id: active.space_id.clone(),
                game_id: active.game_id.clone(),
                display_name: active.display_name.clone(),
                created_at: 0,
            });
        self.open_game_space_settings(&entry);
    }

    fn load_game_space_settings_snapshot(
        entry: &GameSpaceEntry,
    ) -> Result<SettingsViewState, String> {
        let app_path = Self::get_app_settings_path();
        let game_path = spaces::game_space_dir_for(&entry.id).join(spaces::GAME_SETTINGS_FILE);
        let defaults = serde_json::to_value(SettingsViewState::default())
            .map_err(|err| format!("Failed to serialize default settings: {}", err))?;
        let merged = match spaces::read_merged_settings_value(&app_path, &game_path)? {
            Some(saved) => spaces::merge_value_over_defaults(defaults, saved),
            None => defaults,
        };
        serde_json::from_value::<SettingsViewState>(merged)
            .map_err(|err| format!("Failed to parse settings: {}", err))
    }

    /// The space the game-space settings view is editing, falling back to the
    /// active space so the edit helpers stay safe if the state was reset.
    pub(crate) fn game_space_settings_target(&self) -> GameSpaceEntry {
        if let Some(space) = &self.game_space_settings_view_state.space {
            return space.clone();
        }
        let active = spaces::active_game_space();
        GameSpaceEntry {
            id: active.space_id,
            game_id: active.game_id,
            display_name: active.display_name,
            created_at: 0,
        }
    }

    pub(crate) fn editing_active_game_space(&self) -> bool {
        self.game_space_settings_view_state.space.is_none()
            || self.game_space_settings_view_state.is_active_space
    }

    pub(crate) fn edited_game_space_settings(&self) -> &SettingsViewState {
        if self.editing_active_game_space() {
            &self.settings_view_state
        } else {
            &self.game_space_settings_view_state.snapshot
        }
    }

    pub(crate) fn edited_game_space_settings_mut(&mut self) -> &mut SettingsViewState {
        if self.editing_active_game_space() {
            &mut self.settings_view_state
        } else {
            &mut self.game_space_settings_view_state.snapshot
        }
    }

    /// Persist the settings edited by the game-space settings view. Active
    /// space goes through the normal settings save; other spaces write only
    /// the game-space half into their own `game_settings.json`.
    pub(crate) fn save_edited_game_space_settings(&mut self) {
        if self.editing_active_game_space() {
            self.save_settings();
            return;
        }
        let Some(space) = self.game_space_settings_view_state.space.clone() else {
            return;
        };
        let game_path = spaces::game_space_dir_for(&space.id).join(spaces::GAME_SETTINGS_FILE);
        let result = serde_json::to_value(&self.game_space_settings_view_state.snapshot)
            .map_err(|err| format!("Failed to serialize settings: {}", err))
            .and_then(|value| spaces::write_game_settings_half(&value, &game_path));
        if let Err(err) = result {
            warn!("Failed to save game space settings: {}", err);
            self.show_error_toast(self.t_fmt(
                "Could not save game space settings: {error}",
                &[("error", err)],
            ));
        }
    }

    pub fn render_game_space_settings_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };
        let space = self.game_space_settings_target();
        let is_active = self.editing_active_game_space();
        let module_name = crate::core::game::registry()
            .get(&space.game_id)
            .map(|module| module.display_name().to_string());

        egui::Frame::NONE.inner_margin(margin).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.heading(
                        RichText::new(tr("Game space settings")).size(
                            self.settings_view_state.font_sizes.settings_view.page_title as f32,
                        ),
                    );
                    ui.label(
                        RichText::new(&space.display_name)
                            .strong()
                            .color(self.color_text_dim()),
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
                            info!("Closing game space settings view");
                            self.restore_last_view_or_default();
                        }
                    });
                });

                ui.separator();

                render_wrapped_info_row(
                    ui,
                    15.0,
                    RichText::new(self.t_fmt(
                        "These settings apply only to the game space {name}.",
                        &[("name", space.display_name.clone())],
                    ))
                    .italics()
                    .color(self.color_text_dim()),
                );
                if !is_active {
                    render_wrapped_info_row(
                        ui,
                        15.0,
                        RichText::new(tr(
                            "This game space is not active. Changes take effect when you open it.",
                        ))
                        .italics()
                        .color(self.color_text_dim()),
                    );
                }
                ui.separator();

                let game_tab_label = module_name.clone().unwrap_or_else(|| {
                    self.t_fmt(
                        "Unknown game {game_id}",
                        &[("game_id", space.game_id.clone())],
                    )
                });
                let capabilities = crate::core::game::registry()
                    .get(&space.game_id)
                    .map(|module| module.capabilities());
                let mut tabs: Vec<(GameSpaceSettingsTab, &str)> =
                    vec![(GameSpaceSettingsTab::Game, game_tab_label.as_str())];
                if capabilities.is_some_and(|caps| caps.profiles) {
                    tabs.push((GameSpaceSettingsTab::Profiles, "Profiles"));
                }
                tabs.push((
                    GameSpaceSettingsTab::SearchFolders,
                    "Additional search folders",
                ));
                if capabilities.is_some_and(|caps| caps.teamspeak3_plugins) {
                    tabs.push((GameSpaceSettingsTab::Ts3Plugin, "TS3 Plugin"));
                }
                // A tab the active module does not expose must not stay
                // selected (and silently keep rendering) when the view is
                // pointed at a different game space.
                if !tabs
                    .iter()
                    .any(|(tab, _)| *tab == self.game_space_settings_view_state.current_tab)
                {
                    self.game_space_settings_view_state.current_tab = GameSpaceSettingsTab::Game;
                }
                let labels: Vec<&str> = tabs.iter().map(|(_, label)| *label).collect();
                let selected = tabs
                    .iter()
                    .position(|(tab, _)| *tab == self.game_space_settings_view_state.current_tab)
                    .unwrap_or(0);
                if let Some(index) = self.render_adaptive_tab_bar(ui, &labels, selected) {
                    self.game_space_settings_view_state.current_tab = tabs[index].0;
                    info!("Switched game space settings tab to {}", labels[index]);
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
                let tab_index = tabs
                    .iter()
                    .position(|(tab, _)| *tab == self.game_space_settings_view_state.current_tab)
                    .unwrap_or(0);
                let mut card_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("game_space_settings_card", space.id.as_str(), tab_index))
                        .max_rect(frame_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                card_ui.set_clip_rect(card_rect.expand(2.0));

                egui::Frame::NONE
                    .fill(self.color_card_bg())
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(Margin::same(15))
                    .show(&mut card_ui, |ui| {
                        match self.game_space_settings_view_state.current_tab {
                            GameSpaceSettingsTab::Game => {
                                if module_name.is_some() {
                                    self.render_game_space_settings(ui);
                                } else {
                                    ui.label(
                                        RichText::new(self.t_fmt(
                                            "Unknown game {game_id}",
                                            &[("game_id", space.game_id.clone())],
                                        ))
                                        .color(self.color_text_error()),
                                    );
                                }
                            }
                            GameSpaceSettingsTab::Profiles => {
                                self.render_game_space_profile_settings(ui);
                            }
                            GameSpaceSettingsTab::SearchFolders => {
                                self.render_additional_search_folders(ui);
                            }
                            GameSpaceSettingsTab::Ts3Plugin => {
                                if is_active {
                                    self.render_ts3_plugins_settings(ui);
                                } else {
                                    render_wrapped_info_row(
                                        ui,
                                        0.0,
                                        RichText::new(tr(
                                            "Open this game space to manage TeamSpeak 3 plugins.",
                                        ))
                                        .italics()
                                        .color(self.color_text_dim()),
                                    );
                                }
                            }
                        }
                    });
                if is_active {
                    self.render_arma3_profile_action_modal(ui);
                }
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
