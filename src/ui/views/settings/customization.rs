use std::fs;

use crate::ui::app::Foxy;
use crate::ui::fonts::{self, FontSizes};
use crate::ui::i18n::tr;
use crate::ui::palette::PaletteColors;
use crate::ui::theme::{Theme, builtin_presets};
use crate::ui::types::{DEFAULT_UI_SCALE_PERCENT, MAX_UI_SCALE_PERCENT, MIN_UI_SCALE_PERCENT};
use eframe::egui::{Button, RichText, ScrollArea, Slider, Ui, Vec2};
use log::{info, warn};
use rfd::FileDialog;

use super::{render_font_size_slider, render_palette_color_picker};

mod saved_themes;

const CUSTOMIZATION_FONT_CATEGORY_MIN_WIDTH: f32 = 300.0;
const CUSTOMIZATION_SPLIT_SECTION_MIN_WIDTH: f32 = 340.0;

#[derive(Clone, Copy)]
enum CustomizationCategory {
    Main,
    Settings,
    Help,
    About,
    Repository,
    RepositorySettings,
}

fn render_responsive_category_row(
    ui: &mut Ui,
    min_column_width: f32,
    categories: &[CustomizationCategory],
    mut render_section: impl FnMut(&mut Ui, usize) -> bool,
) -> bool {
    if categories.is_empty() {
        return false;
    }

    let spacing = ui.spacing().item_spacing.x.max(1.0);
    let available_width = ui.available_width().max(min_column_width);
    let column_count = ((available_width + spacing) / (min_column_width + spacing))
        .floor()
        .max(1.0) as usize;
    let column_count = column_count.min(categories.len());
    let mut changed = false;

    for row_start in (0..categories.len()).step_by(column_count) {
        let row_end = (row_start + column_count).min(categories.len());
        let row_len = row_end - row_start;

        ui.columns(row_len, |columns| {
            for (offset, column_ui) in columns.iter_mut().enumerate() {
                let section_index = row_start + offset;
                changed |= render_section(column_ui, section_index);
            }
        });

        if row_end < categories.len() {
            ui.separator();
        }
    }

    changed
}

impl Foxy {
    pub(super) fn render_customization_settings(&mut self, ui: &mut Ui, _viewport_height: f32) {
        let horizontal_padding = 15.0;
        let mut changed = false;

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!("{} {}", '\u{2139}', tr("Customize UI font sizes"));
                ui.label(
                    RichText::new(info_text)
                        .italics()
                        .color(self.color_text_dim()),
                );
            });
            ui.separator();

            let scroll_height = (_viewport_height - 48.0).max(120.0);
            let scroll_size = Vec2::new(ui.available_width(), scroll_height);
            ui.allocate_ui(scroll_size, |ui| {
                ScrollArea::both()
                    .id_salt("customization_settings")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(4.0);

                        self.render_theme_management(ui, horizontal_padding);

                        ui.separator();
                        ui.heading(tr("UI Scale"));
                        changed |= self.render_ui_scale_slider(ui, horizontal_padding);

                        ui.separator();
                        changed |= self.render_customization_font_rows(ui, horizontal_padding);

                        ui.separator();
                        changed |=
                            self.render_split_update_view_font_settings(ui, horizontal_padding);

                        ui.separator();
                        changed |= self.render_split_palette_color_settings(ui, horizontal_padding);

                        ui.separator();
                        if self.render_customization_reset_button(
                            ui,
                            horizontal_padding,
                            tr("Reset font sizes"),
                            tr("Restore all font sizes to their default values."),
                        ) {
                            self.settings_view_state.font_sizes = FontSizes::default();
                            changed = true;
                        }

                        if self.render_customization_reset_button(
                            ui,
                            horizontal_padding,
                            tr("Reset colors"),
                            tr("Restore all palette colors to their default values."),
                        ) {
                            self.settings_view_state.palette_colors = PaletteColors::default();
                            changed = true;
                        }

                        ui.add_space(12.0);
                    });
            });
        });

        if changed {
            self.settings_view_state.font_sizes.clamp_to_limits();
            self.save_settings();
        }
    }

    fn render_customization_reset_button(
        &self,
        ui: &mut Ui,
        horizontal_padding: f32,
        label: String,
        hover_text: String,
    ) -> bool {
        let button_width = (ui.available_width() - (horizontal_padding * 2.0)).max(0.0);

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let reset_button = ui
                .add_sized(
                    Vec2::new(button_width, 30.0),
                    Button::new(label).fill(self.color_main_bg()),
                )
                .on_hover_text(hover_text);
            if reset_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            ui.add_space(horizontal_padding);
            reset_button.clicked()
        })
        .inner
    }

    fn render_customization_font_rows(&mut self, ui: &mut Ui, horizontal_padding: f32) -> bool {
        let rows = [
            [CustomizationCategory::Main, CustomizationCategory::Settings],
            [CustomizationCategory::Help, CustomizationCategory::About],
            [
                CustomizationCategory::Repository,
                CustomizationCategory::RepositorySettings,
            ],
        ];
        let mut changed = false;

        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                ui.separator();
            }
            changed |= render_responsive_category_row(
                ui,
                CUSTOMIZATION_FONT_CATEGORY_MIN_WIDTH,
                row,
                |ui, index| self.render_customization_category(ui, row[index], horizontal_padding),
            );
        }

        changed
    }

    fn render_customization_category(
        &mut self,
        ui: &mut Ui,
        category: CustomizationCategory,
        horizontal_padding: f32,
    ) -> bool {
        match category {
            CustomizationCategory::Main => {
                self.render_main_view_font_settings(ui, horizontal_padding)
            }
            CustomizationCategory::Settings => {
                self.render_settings_view_font_settings(ui, horizontal_padding)
            }
            CustomizationCategory::Help => {
                self.render_help_view_font_settings(ui, horizontal_padding)
            }
            CustomizationCategory::About => {
                self.render_about_view_font_settings(ui, horizontal_padding)
            }
            CustomizationCategory::Repository => {
                self.render_repository_view_font_settings(ui, horizontal_padding)
            }
            CustomizationCategory::RepositorySettings => {
                self.render_repository_settings_view_font_settings(ui, horizontal_padding)
            }
        }
    }

    fn render_main_view_font_settings(&mut self, ui: &mut Ui, horizontal_padding: f32) -> bool {
        let mut changed = false;

        ui.heading(tr("Main View Fonts"));
        changed |= render_font_size_slider(
            ui,
            tr("Window control icons"),
            &mut self
                .settings_view_state
                .font_sizes
                .main_view
                .window_control_icons,
            fonts::MAIN_WINDOW_CONTROL_ICONS_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Activity log toggle icon"),
            &mut self
                .settings_view_state
                .font_sizes
                .main_view
                .activity_log_toggle_icon,
            fonts::MAIN_ACTIVITY_LOG_TOGGLE_ICON_RANGE,
            horizontal_padding,
        );

        changed
    }

    fn render_settings_view_font_settings(&mut self, ui: &mut Ui, horizontal_padding: f32) -> bool {
        let mut changed = false;

        ui.heading(tr("Settings View Fonts"));
        changed |= render_font_size_slider(
            ui,
            tr("Settings page title"),
            &mut self.settings_view_state.font_sizes.settings_view.page_title,
            fonts::SETTINGS_PAGE_TITLE_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Settings close icon"),
            &mut self.settings_view_state.font_sizes.settings_view.close_icon,
            fonts::SETTINGS_CLOSE_ICON_RANGE,
            horizontal_padding,
        );

        changed
    }

    fn render_help_view_font_settings(&mut self, ui: &mut Ui, horizontal_padding: f32) -> bool {
        let mut changed = false;

        ui.heading(tr("Help View Fonts"));
        changed |= render_font_size_slider(
            ui,
            tr("Help page title"),
            &mut self.settings_view_state.font_sizes.help_view.page_title,
            fonts::HELP_PAGE_TITLE_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Help tab labels"),
            &mut self.settings_view_state.font_sizes.help_view.tab_label,
            fonts::HELP_TAB_LABEL_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Help section title"),
            &mut self.settings_view_state.font_sizes.help_view.section_title,
            fonts::HELP_SECTION_TITLE_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Help body text"),
            &mut self.settings_view_state.font_sizes.help_view.body,
            fonts::HELP_BODY_RANGE,
            horizontal_padding,
        );

        changed
    }

    fn render_about_view_font_settings(&mut self, ui: &mut Ui, horizontal_padding: f32) -> bool {
        let mut changed = false;

        ui.heading(tr("About View Fonts"));
        changed |= render_font_size_slider(
            ui,
            tr("About H1 heading"),
            &mut self.settings_view_state.font_sizes.about_view.h1,
            fonts::ABOUT_H1_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("About H2 heading"),
            &mut self.settings_view_state.font_sizes.about_view.h2,
            fonts::ABOUT_H2_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("About H3 heading"),
            &mut self.settings_view_state.font_sizes.about_view.h3,
            fonts::ABOUT_H3_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("About body text"),
            &mut self.settings_view_state.font_sizes.about_view.body,
            fonts::ABOUT_BODY_RANGE,
            horizontal_padding,
        );

        changed
    }

    fn render_repository_view_font_settings(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
    ) -> bool {
        let mut changed = false;

        ui.heading(tr("Repository View Fonts"));
        changed |= render_font_size_slider(
            ui,
            tr("Add repository button"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_view
                .add_repository_button,
            fonts::REPOSITORY_ADD_BUTTON_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Repository toolbar icons"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_view
                .toolbar_icons,
            fonts::REPOSITORY_TOOLBAR_ICONS_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Repository status banners"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_view
                .status_banner,
            fonts::REPOSITORY_STATUS_BANNER_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Launch and Join buttons"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_view
                .launch_join_buttons,
            fonts::REPOSITORY_LAUNCH_JOIN_BUTTONS_RANGE,
            horizontal_padding,
        );

        changed
    }

    fn render_split_update_view_font_settings(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
    ) -> bool {
        let mut changed = false;

        ui.heading(tr("Update View Fonts"));
        if self.should_split_customization_section(ui, CUSTOMIZATION_SPLIT_SECTION_MIN_WIDTH) {
            ui.columns(2, |columns| {
                changed |= self.render_update_view_font_settings_range(
                    &mut columns[0],
                    horizontal_padding,
                    0..6,
                    false,
                );
                changed |= self.render_update_view_font_settings_range(
                    &mut columns[1],
                    horizontal_padding,
                    6..11,
                    false,
                );
            });
        } else {
            changed |=
                self.render_update_view_font_settings_range(ui, horizontal_padding, 0..11, false);
        }

        changed
    }

    fn should_split_customization_section(&self, ui: &Ui, min_column_width: f32) -> bool {
        let spacing = ui.spacing().item_spacing.x;
        ui.available_width() >= (min_column_width * 2.0) + spacing
    }

    fn render_update_view_font_settings_range(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        range: std::ops::Range<usize>,
        show_heading: bool,
    ) -> bool {
        let mut changed = false;

        if show_heading {
            ui.heading(tr("Update View Fonts"));
        }
        for index in range {
            changed |= self.render_update_view_font_setting(ui, horizontal_padding, index);
        }

        changed
    }

    fn render_update_view_font_setting(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        index: usize,
    ) -> bool {
        match index {
            0 => render_font_size_slider(
                ui,
                tr("Update page title"),
                &mut self.settings_view_state.font_sizes.update_view.page_title,
                fonts::UPDATE_PAGE_TITLE_RANGE,
                horizontal_padding,
            ),
            1 => render_font_size_slider(
                ui,
                tr("Update close icon"),
                &mut self.settings_view_state.font_sizes.update_view.close_icon,
                fonts::UPDATE_CLOSE_ICON_RANGE,
                horizontal_padding,
            ),
            2 => render_font_size_slider(
                ui,
                tr("Update section title"),
                &mut self
                    .settings_view_state
                    .font_sizes
                    .update_view
                    .section_title,
                fonts::UPDATE_SECTION_TITLE_RANGE,
                horizontal_padding,
            ),
            3 => render_font_size_slider(
                ui,
                tr("Update total size label"),
                &mut self.settings_view_state.font_sizes.update_view.total_size,
                fonts::UPDATE_TOTAL_SIZE_RANGE,
                horizontal_padding,
            ),
            4 => render_font_size_slider(
                ui,
                tr("Update summary heading"),
                &mut self
                    .settings_view_state
                    .font_sizes
                    .update_view
                    .summary_heading,
                fonts::UPDATE_SUMMARY_HEADING_RANGE,
                horizontal_padding,
            ),
            5 => render_font_size_slider(
                ui,
                tr("Update summary body fallback"),
                &mut self
                    .settings_view_state
                    .font_sizes
                    .update_view
                    .summary_body_fallback,
                fonts::UPDATE_SUMMARY_BODY_RANGE,
                horizontal_padding,
            ),
            6 => render_font_size_slider(
                ui,
                tr("Update mod name"),
                &mut self.settings_view_state.font_sizes.update_view.mod_name,
                fonts::UPDATE_MOD_NAME_RANGE,
                horizontal_padding,
            ),
            7 => render_font_size_slider(
                ui,
                tr("Update mod status"),
                &mut self.settings_view_state.font_sizes.update_view.mod_status,
                fonts::UPDATE_MOD_STATUS_RANGE,
                horizontal_padding,
            ),
            8 => render_font_size_slider(
                ui,
                tr("Update mod progress"),
                &mut self.settings_view_state.font_sizes.update_view.mod_progress,
                fonts::UPDATE_MOD_PROGRESS_RANGE,
                horizontal_padding,
            ),
            9 => render_font_size_slider(
                ui,
                tr("Update file details"),
                &mut self.settings_view_state.font_sizes.update_view.file_details,
                fonts::UPDATE_FILE_DETAILS_RANGE,
                horizontal_padding,
            ),
            10 => render_font_size_slider(
                ui,
                tr("Update pause button"),
                &mut self.settings_view_state.font_sizes.update_view.pause_button,
                fonts::UPDATE_PAUSE_BUTTON_RANGE,
                horizontal_padding,
            ),
            _ => false,
        }
    }

    fn render_repository_settings_view_font_settings(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
    ) -> bool {
        let mut changed = false;

        ui.heading(tr("Repository Settings View Fonts"));
        changed |= render_font_size_slider(
            ui,
            tr("Repository settings title"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_settings_view
                .page_title,
            fonts::REPOSITORY_SETTINGS_PAGE_TITLE_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Repository settings close icon"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_settings_view
                .close_icon,
            fonts::REPOSITORY_SETTINGS_CLOSE_ICON_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Repository settings refresh icon"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_settings_view
                .refresh_icon,
            fonts::REPOSITORY_SETTINGS_REFRESH_ICON_RANGE,
            horizontal_padding,
        );
        changed |= render_font_size_slider(
            ui,
            tr("Repository addon path text"),
            &mut self
                .settings_view_state
                .font_sizes
                .repository_settings_view
                .addon_path,
            fonts::REPOSITORY_SETTINGS_ADDON_PATH_RANGE,
            horizontal_padding,
        );

        changed
    }

    fn render_split_palette_color_settings(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
    ) -> bool {
        let mut changed = false;

        ui.heading(tr("Palette Colors"));
        if self.should_split_customization_section(ui, CUSTOMIZATION_SPLIT_SECTION_MIN_WIDTH) {
            ui.columns(2, |columns| {
                changed |= self.render_palette_color_settings_range(
                    &mut columns[0],
                    horizontal_padding,
                    0..8,
                );
                changed |= self.render_palette_color_settings_range(
                    &mut columns[1],
                    horizontal_padding,
                    8..16,
                );
            });
        } else {
            changed |= self.render_palette_color_settings_range(ui, horizontal_padding, 0..16);
        }

        changed
    }

    fn render_palette_color_settings_range(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        range: std::ops::Range<usize>,
    ) -> bool {
        let mut changed = false;

        for index in range {
            changed |= self.render_palette_color_setting(ui, horizontal_padding, index);
        }

        changed
    }

    fn render_palette_color_setting(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        index: usize,
    ) -> bool {
        match index {
            0 => render_palette_color_picker(
                ui,
                tr("Primary Accent"),
                &mut self.settings_view_state.palette_colors.primary_accent,
                horizontal_padding,
            ),
            1 => render_palette_color_picker(
                ui,
                tr("Widget Background"),
                &mut self.settings_view_state.palette_colors.widget_bg,
                horizontal_padding,
            ),
            2 => render_palette_color_picker(
                ui,
                tr("Main Background"),
                &mut self.settings_view_state.palette_colors.main_bg,
                horizontal_padding,
            ),
            3 => render_palette_color_picker(
                ui,
                tr("Card Background"),
                &mut self.settings_view_state.palette_colors.card_bg,
                horizontal_padding,
            ),
            4 => render_palette_color_picker(
                ui,
                tr("Server Offline Background"),
                &mut self.settings_view_state.palette_colors.server_offline_bg,
                horizontal_padding,
            ),
            5 => render_palette_color_picker(
                ui,
                tr("Text Normal"),
                &mut self.settings_view_state.palette_colors.text_normal,
                horizontal_padding,
            ),
            6 => render_palette_color_picker(
                ui,
                tr("Text Gray"),
                &mut self.settings_view_state.palette_colors.text_gray,
                horizontal_padding,
            ),
            7 => render_palette_color_picker(
                ui,
                tr("Text Dim"),
                &mut self.settings_view_state.palette_colors.text_dim,
                horizontal_padding,
            ),
            8 => render_palette_color_picker(
                ui,
                tr("Text Error"),
                &mut self.settings_view_state.palette_colors.text_error,
                horizontal_padding,
            ),
            9 => render_palette_color_picker(
                ui,
                tr("Log Error"),
                &mut self.settings_view_state.palette_colors.error,
                horizontal_padding,
            ),
            10 => render_palette_color_picker(
                ui,
                tr("Log Warning"),
                &mut self.settings_view_state.palette_colors.warn,
                horizontal_padding,
            ),
            11 => render_palette_color_picker(
                ui,
                tr("Log Debug"),
                &mut self.settings_view_state.palette_colors.debug,
                horizontal_padding,
            ),
            12 => render_palette_color_picker(
                ui,
                tr("Success"),
                &mut self.settings_view_state.palette_colors.success,
                horizontal_padding,
            ),
            13 => render_palette_color_picker(
                ui,
                tr("Success Muted"),
                &mut self.settings_view_state.palette_colors.success_muted,
                horizontal_padding,
            ),
            14 => render_palette_color_picker(
                ui,
                tr("Action Info"),
                &mut self.settings_view_state.palette_colors.action_info,
                horizontal_padding,
            ),
            15 => render_palette_color_picker(
                ui,
                tr("Action Destructive"),
                &mut self.settings_view_state.palette_colors.action_destructive,
                horizontal_padding,
            ),
            _ => false,
        }
    }

    /// Global UI scale slider. The slider edits a pending value; the change is
    /// applied to the egui zoom factor (100% = native platform scale) only when
    /// the user clicks Apply. Reset restores the default 100% immediately.
    /// Returns true when the applied value changed and should be persisted.
    fn render_ui_scale_slider(&mut self, ui: &mut Ui, horizontal_padding: f32) -> bool {
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let info_text = format!(
                "{} {}",
                '\u{2139}',
                tr("Scale the entire interface. 100% is the default size. Click Apply to use the new scale.")
            );
            ui.label(
                RichText::new(info_text)
                    .italics()
                    .color(self.color_text_dim()),
            );
        });

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("UI Scale"));
            let slider = ui.add(
                Slider::new(
                    &mut self.settings_view_state.ui_scale_percent_draft,
                    MIN_UI_SCALE_PERCENT..=MAX_UI_SCALE_PERCENT,
                )
                .suffix("%")
                .show_value(true)
                .trailing_fill(true),
            );
            if slider.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let mut applied = false;

            let has_pending = self.settings_view_state.ui_scale_percent_draft
                != self.settings_view_state.ui_scale_percent;
            let apply_button = ui
                .add_enabled(
                    has_pending,
                    Button::new(tr("Apply")).fill(self.color_main_bg()),
                )
                .on_hover_text(tr("Apply the selected UI scale."));
            if has_pending && apply_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if apply_button.clicked() && has_pending {
                self.settings_view_state.ui_scale_percent =
                    self.settings_view_state.ui_scale_percent_draft;
                applied = true;
            }

            let reset_button = ui
                .add(Button::new(tr("Reset")).fill(self.color_main_bg()))
                .on_hover_text(tr("Restore the UI scale to 100%."));
            if reset_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if reset_button.clicked() {
                self.settings_view_state.ui_scale_percent_draft = DEFAULT_UI_SCALE_PERCENT;
                if self.settings_view_state.ui_scale_percent != DEFAULT_UI_SCALE_PERCENT {
                    self.settings_view_state.ui_scale_percent = DEFAULT_UI_SCALE_PERCENT;
                    applied = true;
                }
            }

            ui.add_space(horizontal_padding);
            applied
        })
        .inner
    }

    /// Theme presets plus import/export controls, shown at the top of the
    /// customization tab. Applying a preset or importing a file replaces both
    /// the font sizes and palette colors and persists immediately.
    fn render_theme_management(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        ui.heading(tr("Themes"));
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let info_text = format!(
                "{} {}",
                '\u{2139}',
                tr("Apply a preset, or export the current look as a theme file to share or back up.")
            );
            ui.label(
                RichText::new(info_text)
                    .italics()
                    .color(self.color_text_dim()),
            );
        });

        ui.add_space(4.0);
        let presets = builtin_presets();
        ui.horizontal_wrapped(|ui| {
            ui.add_space(horizontal_padding);
            for preset in &presets {
                let button = ui
                    .add(Button::new(tr(preset.name)).fill(self.color_main_bg()))
                    .on_hover_text(tr(
                        "Apply this preset's colors and reset font sizes to their defaults.",
                    ));
                if button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if button.clicked() {
                    info!("Applying theme preset: {}", preset.name);
                    self.apply_theme(preset.to_theme());
                    self.show_success_toast(self.t("Theme preset applied."));
                }
            }
        });

        ui.add_space(6.0);
        self.render_saved_themes(ui, horizontal_padding);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let spacing = ui.spacing().item_spacing.x;
            let button_width =
                ((ui.available_width() - horizontal_padding - spacing) / 2.0).max(80.0);

            let export_button = ui
                .add_sized(
                    Vec2::new(button_width, 30.0),
                    Button::new(tr("Export theme...")).fill(self.color_main_bg()),
                )
                .on_hover_text(tr(
                    "Save the current font sizes and colors to a theme file.",
                ));
            if export_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if export_button.clicked() {
                self.export_theme();
            }

            let import_button = ui
                .add_sized(
                    Vec2::new(button_width, 30.0),
                    Button::new(tr("Import theme...")).fill(self.color_main_bg()),
                )
                .on_hover_text(tr("Load font sizes and colors from a theme file."));
            if import_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if import_button.clicked() {
                self.import_theme();
            }
            ui.add_space(horizontal_padding);
        });
    }

    /// Replace the current font sizes and palette with `theme`, clamp font
    /// sizes to valid ranges, and persist. The palette color cache rebuilds on
    /// the next frame, so the change is visible immediately.
    fn apply_theme(&mut self, theme: Theme) {
        self.settings_view_state.font_sizes = theme.font_sizes;
        self.settings_view_state.palette_colors = theme.palette_colors;
        self.settings_view_state.font_sizes.clamp_to_limits();
        self.save_settings();
    }

    fn export_theme(&mut self) {
        let theme = Theme::from_current(
            "Foxy custom theme",
            self.settings_view_state.font_sizes.clone(),
            self.settings_view_state.palette_colors.clone(),
        );
        let Some(path) = crate::ui::app::agent_support::save_file(|| {
            FileDialog::new()
                .add_filter(tr("Foxy Theme"), &["json"])
                .set_file_name("foxy-theme.json")
                .save_file()
        }) else {
            return;
        };

        match theme.to_json() {
            Ok(json) => match fs::write(&path, json) {
                Ok(()) => {
                    info!("Exported theme to {}", path.display());
                    self.show_success_toast(self.t("Theme exported successfully."));
                }
                Err(err) => {
                    warn!("Failed to write theme file: {}", err);
                    self.show_error_toast(self.t("Failed to export theme."));
                }
            },
            Err(err) => {
                warn!("Failed to serialize theme: {}", err);
                self.show_error_toast(self.t("Failed to export theme."));
            }
        }
    }

    fn import_theme(&mut self) {
        let Some(path) = crate::ui::app::agent_support::pick_file(|| {
            FileDialog::new()
                .add_filter(tr("Foxy Theme"), &["json"])
                .pick_file()
        }) else {
            return;
        };

        match fs::read_to_string(&path) {
            Ok(contents) => match Theme::from_json(&contents) {
                Ok(theme) => {
                    info!("Imported theme from {}", path.display());
                    self.apply_theme(theme);
                    self.show_success_toast(self.t("Theme imported successfully."));
                }
                Err(err) => {
                    warn!("Failed to parse theme file: {}", err);
                    self.show_error_toast(
                        self.t("Failed to import theme. The file is not a valid theme."),
                    );
                }
            },
            Err(err) => {
                warn!("Failed to read theme file: {}", err);
                self.show_error_toast(self.t("Failed to import theme."));
            }
        }
    }
}
