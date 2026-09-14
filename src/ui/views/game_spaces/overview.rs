use std::path::Path;

use eframe::egui::{
    Align, Button, Color32, CornerRadius, Frame, Label, Layout, Margin, Rect, RichText, ScrollArea,
    Sense, Stroke, Ui, Vec2,
};
use log::{info, warn};

use crate::core::game::spaces;
use crate::ui::app::{Foxy, RepositoryGroupSummary, TeamSpeakSummary};
use crate::ui::game_logos::GAME_LOGO_HEIGHT;
use crate::ui::i18n::{fmt_bytes, fmt_date};
use crate::ui::views::settings::ts3_plugins::Ts3PluginRowStatus;

const OVERVIEW_LOGO_MAX_WIDTH: f32 = 200.0;
const STAT_VALUE_SIZE: f32 = 24.0;
const CARD_GAP: f32 = 12.0;
const SHARE_BAR_HEIGHT: f32 = 4.0;

struct StatTile {
    value: String,
    label: String,
}

#[cfg(target_os = "windows")]
fn open_directory(path: &Path) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path.as_os_str())
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(not(target_os = "windows"))]
fn open_directory(path: &Path) -> Result<(), String> {
    crate::core::utils::platform::open_with_default_app(path)
}

impl Foxy {
    /// Main-panel summary of the active game space: identity, disk footprint,
    /// repository spaces, and the most recent update and launch.
    pub(crate) fn render_game_space_overview(&mut self, ui: &mut Ui) {
        self.poll_game_space_overview();

        let active = spaces::active_game_space();
        let module = crate::core::game::registry().get(&active.game_id);
        let module_name = module
            .map(|module| module.display_name().to_string())
            .unwrap_or_else(|| active.game_id.clone());
        let install_dir = module
            .map(|module| module.install_dir_from_settings(&self.settings_view_state))
            .unwrap_or_default()
            .trim()
            .to_string();
        let workspace_path = spaces::active_game_space_dir().display().to_string();
        let report = self.game_space_overview.report.clone();
        let repository_count = self.repository_view_state.repositories.len();
        let space_count = self.repository_spaces.len();
        let last_update = self.latest_repository_activity(|repo| repo.last_updated_at);
        let last_launch = self.latest_repository_activity(|repo| repo.last_launched_at);
        let pending = self.t("Loading...");
        let text_color = self.color_text_normal();
        let dim_color = self.color_text_dim();
        let accent = self.color_primary_accent();
        let mut open_space: Option<String> = None;

        ScrollArea::vertical()
            .id_salt("game_space_overview")
            .show(ui, |ui| {
                ui.add_space(6.0);
                self.overview_card(ui, |this, ui| {
                    ui.horizontal(|ui| {
                        this.game_logo_textures.show_badge(
                            ui,
                            &active.game_id,
                            GAME_LOGO_HEIGHT,
                            OVERVIEW_LOGO_MAX_WIDTH,
                            text_color,
                        );
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.add(
                                Label::new(
                                    RichText::new(&active.display_name)
                                        .heading()
                                        .color(text_color),
                                )
                                .truncate(),
                            );
                            ui.add_space(2.0);
                            this.overview_chip(ui, &module_name, accent);
                            ui.add_space(6.0);
                            this.overview_folder_row(ui, &install_dir, "game install");
                        });
                    });
                });

                ui.add_space(CARD_GAP);
                let tiles = [
                    StatTile {
                        value: report
                            .as_ref()
                            .map(|report| fmt_bytes(report.total.total_bytes))
                            .unwrap_or_else(|| pending.clone()),
                        label: self.t("Mods on disk"),
                    },
                    StatTile {
                        value: report
                            .as_ref()
                            .map(|report| report.total.unique_addons.to_string())
                            .unwrap_or_else(|| pending.clone()),
                        label: self.t("Unique addons"),
                    },
                    StatTile {
                        value: repository_count.to_string(),
                        label: self.t("Repositories"),
                    },
                    StatTile {
                        value: space_count.to_string(),
                        label: self.t("Repository spaces"),
                    },
                ];
                self.render_stat_tiles(ui, &tiles);

                ui.add_space(CARD_GAP);
                self.overview_card(ui, |this, ui| {
                    this.overview_section_title(ui, &this.t("Repository spaces"));
                    match report.as_ref() {
                        Some(report) if report.groups.is_empty() => {
                            ui.label(
                                RichText::new(this.t("No repositories in this game space yet."))
                                    .italics()
                                    .color(dim_color),
                            );
                        }
                        Some(report) => {
                            let total_bytes = report.total.total_bytes.max(1);
                            for group in &report.groups {
                                if this.render_repository_group_row(ui, group, total_bytes)
                                    && let Some(space_id) = &group.space_id
                                {
                                    open_space = Some(space_id.clone());
                                }
                            }
                        }
                        None => {
                            ui.label(RichText::new(&pending).italics().color(dim_color));
                        }
                    }
                });

                if let Some(teamspeak) =
                    report.as_ref().and_then(|report| report.teamspeak.as_ref())
                {
                    ui.add_space(CARD_GAP);
                    self.overview_card(ui, |this, ui| {
                        this.render_teamspeak_summary(ui, teamspeak);
                    });
                }

                ui.add_space(CARD_GAP);
                self.overview_card(ui, |this, ui| {
                    this.overview_section_title(ui, &this.t("Activity"));
                    let never = this.t("Never");
                    let workspace_size = report
                        .as_ref()
                        .map(|report| fmt_bytes(report.workspace_bytes))
                        .unwrap_or_else(|| pending.clone());
                    eframe::egui::Grid::new("game_space_overview_activity")
                        .num_columns(2)
                        .spacing(Vec2::new(24.0, 8.0))
                        .show(ui, |ui| {
                            for (label, value) in [
                                (this.t("Last update"), last_update.clone()),
                                (this.t("Last launched"), last_launch.clone()),
                            ] {
                                ui.label(RichText::new(label).color(dim_color));
                                ui.horizontal(|ui| match value {
                                    Some((name, at)) => {
                                        ui.add(
                                            Label::new(
                                                RichText::new(name).strong().color(text_color),
                                            )
                                            .truncate(),
                                        );
                                        ui.label(RichText::new(fmt_date(at)).color(dim_color));
                                    }
                                    None => {
                                        ui.label(RichText::new(&never).color(dim_color));
                                    }
                                });
                                ui.end_row();
                            }
                            ui.label(RichText::new(this.t("Foxy workspace")).color(dim_color));
                            ui.label(RichText::new(workspace_size).strong().color(text_color))
                                .on_hover_text(&workspace_path);
                            ui.end_row();
                        });
                });
                ui.add_space(CARD_GAP);
            });

        if let Some(space_id) = open_space {
            self.repository_view_state.selected_repository = None;
            self.selected_repository_visual_folder_id = None;
            self.selected_repository_space_id = Some(space_id);
            self.needs_repaint = true;
        }
    }

    fn latest_repository_activity(
        &self,
        pick: impl Fn(&crate::ui::types::Repository) -> Option<u64>,
    ) -> Option<(String, u64)> {
        self.repository_view_state
            .repositories
            .iter()
            .filter_map(|repo| pick(repo).map(|at| (repo.name.clone(), at)))
            .max_by_key(|(_, at)| *at)
    }

    fn overview_card(&mut self, ui: &mut Ui, add_contents: impl FnOnce(&mut Self, &mut Ui)) {
        Frame::NONE
            .fill(self.color_card_bg())
            .stroke(Stroke::new(1.0, self.color_widget_bg()))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add_contents(self, ui);
            });
    }

    /// "Open folder" button followed by the path; an empty path renders as
    /// "Not configured" with the button disabled.
    fn overview_folder_row(&self, ui: &mut Ui, path: &str, log_label: &str) {
        let dim_color = self.color_text_dim();
        ui.horizontal(|ui| {
            let open_button = ui
                .add_enabled(
                    !path.is_empty(),
                    Button::new(RichText::new(self.t("Open folder")).small()),
                )
                .on_hover_text(path);
            if open_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if open_button.clicked() {
                match open_directory(Path::new(path)) {
                    Ok(()) => info!(
                        "Opened the {} folder from the game space overview",
                        log_label
                    ),
                    Err(err) => warn!("Failed to open the {} folder: {}", log_label, err),
                }
            }
            if path.is_empty() {
                ui.label(
                    RichText::new(self.t("Not configured"))
                        .small()
                        .italics()
                        .color(dim_color),
                );
            } else {
                ui.add(Label::new(RichText::new(path).small().color(dim_color)).truncate())
                    .on_hover_text(path);
            }
        });
    }

    fn render_teamspeak_summary(&self, ui: &mut Ui, teamspeak: &TeamSpeakSummary) {
        let text_color = self.color_text_normal();
        let dim_color = self.color_text_dim();
        self.overview_section_title(ui, &self.t("TeamSpeak 3"));
        self.overview_folder_row(
            ui,
            &teamspeak.directory.display().to_string(),
            "TeamSpeak 3",
        );
        if teamspeak.plugins.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new(self.t("No TS3 plugins found in the repositories."))
                    .italics()
                    .color(dim_color),
            );
            return;
        }
        ui.add_space(8.0);
        for plugin in &teamspeak.plugins {
            let version = match &plugin.version {
                Some(version) => self.t_fmt("Version {version}", &[("version", version.clone())]),
                None => self.t("Unknown version"),
            };
            let (status_text, status_color) = match plugin.status {
                Ts3PluginRowStatus::UpToDate => (
                    format!("\u{2714} {}", self.t("Up to date")),
                    self.color_success(),
                ),
                Ts3PluginRowStatus::UpdateAvailable => {
                    (self.t("Update available"), self.color_warn())
                }
                Ts3PluginRowStatus::InstallPending => (
                    self.t("Waiting for TeamSpeak to finish installing"),
                    self.color_warn(),
                ),
                Ts3PluginRowStatus::NotInstalled => (self.t("Not installed"), self.color_warn()),
            };
            Frame::NONE
                .fill(self.color_main_bg())
                .stroke(Stroke::new(1.0, self.color_widget_bg()))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), ui.spacing().interact_size.y),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            ui.label(RichText::new(status_text).color(status_color));
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(
                                    Label::new(
                                        RichText::new(&plugin.addon_name)
                                            .strong()
                                            .color(text_color),
                                    )
                                    .truncate(),
                                );
                                ui.add(
                                    Label::new(RichText::new(version).small().color(dim_color))
                                        .truncate(),
                                );
                            });
                        },
                    );
                });
            ui.add_space(6.0);
        }
    }

    fn overview_section_title(&self, ui: &mut Ui, title: &str) {
        ui.label(
            RichText::new(title.to_uppercase())
                .small()
                .strong()
                .color(self.color_text_dim()),
        );
        ui.add_space(8.0);
    }

    fn overview_chip(&self, ui: &mut Ui, text: &str, color: Color32) {
        Frame::NONE
            .fill(self.color_main_bg())
            .stroke(Stroke::new(1.0, color))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.label(RichText::new(text).small().color(color));
            });
    }

    fn render_stat_tiles(&self, ui: &mut Ui, tiles: &[StatTile]) {
        let count = tiles.len().max(1) as f32;
        let tile_width = ((ui.available_width() - CARD_GAP * (count - 1.0)) / count).max(0.0);
        let accent = self.color_primary_accent();
        let dim_color = self.color_text_dim();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for tile in tiles {
                ui.allocate_ui_with_layout(
                    Vec2::new(tile_width, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        Frame::NONE
                            .fill(self.color_card_bg())
                            .stroke(Stroke::new(1.0, self.color_widget_bg()))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(16, 12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.add(
                                    Label::new(
                                        RichText::new(&tile.value)
                                            .size(STAT_VALUE_SIZE)
                                            .strong()
                                            .color(accent),
                                    )
                                    .truncate(),
                                );
                                ui.add(
                                    Label::new(RichText::new(&tile.label).small().color(dim_color))
                                        .truncate(),
                                );
                            });
                    },
                );
            }
        });
    }

    /// One repository space row with its share of the game space's mods drawn
    /// as a bar. Returns true when the row was clicked.
    fn render_repository_group_row(
        &self,
        ui: &mut Ui,
        group: &RepositoryGroupSummary,
        total_bytes: u64,
    ) -> bool {
        let text_color = self.color_text_normal();
        let dim_color = self.color_text_dim();
        let accent = self.color_primary_accent();
        let clickable = group.space_id.is_some();
        let name = if clickable {
            group.name.clone()
        } else {
            self.t("Standalone repositories")
        };
        let detail = self.t_fmt(
            "{repositories} repositories, {addons} addons",
            &[
                ("repositories", group.repository_count.to_string()),
                ("addons", group.stats.unique_addons.to_string()),
            ],
        );
        let share = (group.stats.total_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0) as f32;
        let sense = if clickable {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = Frame::NONE
            .fill(self.color_main_bg())
            .stroke(Stroke::new(1.0, self.color_widget_bg()))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), ui.spacing().interact_size.y),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(fmt_bytes(group.stats.total_bytes))
                                .strong()
                                .color(text_color),
                        );
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(
                                Label::new(RichText::new(name).strong().color(text_color))
                                    .truncate(),
                            );
                            ui.add(
                                Label::new(RichText::new(detail).small().color(dim_color))
                                    .truncate(),
                            );
                        });
                    },
                );
                ui.add_space(6.0);
                let (track, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), SHARE_BAR_HEIGHT),
                    Sense::hover(),
                );
                let painter = ui.painter();
                let radius = CornerRadius::same((SHARE_BAR_HEIGHT / 2.0) as u8);
                painter.rect_filled(track, radius, self.color_widget_bg());
                let fill = Rect::from_min_size(
                    track.min,
                    Vec2::new(track.width() * share, track.height()),
                );
                painter.rect_filled(fill, radius, accent);
            })
            .response
            .interact(sense);
        if clickable && response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        ui.add_space(6.0);
        clickable && response.clicked()
    }
}
