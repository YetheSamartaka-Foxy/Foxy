use egui::{Button, Color32, CursorIcon, RichText, TextStyle, Ui, Vec2, Visuals};

use crate::ui::app::{CachedPaletteColor32, Foxy};
use crate::ui::i18n::tr;
use crate::ui::palette;

impl Foxy {
    fn ensure_color32_cache(&mut self) {
        let palette = &self.settings_view_state.palette_colors;
        if self
            .last_applied_palette
            .as_ref()
            .is_some_and(|last| last == palette)
        {
            return;
        }

        let primary_accent = palette.primary_accent.to_color32();
        let widget_bg = palette.widget_bg.to_color32();

        self.cached_color32 = Some(CachedPaletteColor32 {
            primary_accent,
            primary_accent_hover: Self::blend_color(primary_accent, widget_bg, 0.22),
            primary_accent_active: Self::blend_color(primary_accent, widget_bg, 0.36),
            widget_bg,
            main_bg: palette.main_bg.to_color32(),
            card_bg: palette.card_bg.to_color32(),
            server_offline_bg: palette.server_offline_bg.to_color32(),
            server_selected_bg: Self::blend_color(
                primary_accent,
                palette.card_bg.to_color32(),
                0.80,
            ),
            server_selected_bg_hover: Self::blend_color(
                primary_accent,
                palette.card_bg.to_color32(),
                0.72,
            ),
            server_selected_stroke: primary_accent,
            text_normal: palette.text_normal.to_color32(),
            text_gray: palette.text_gray.to_color32(),
            text_dim: palette.text_dim.to_color32(),
            text_error: palette.text_error.to_color32(),
            error: palette.error.to_color32(),
            warn: palette.warn.to_color32(),
            debug: palette.debug.to_color32(),
            success: palette.success.to_color32(),
            success_muted: palette.success_muted.to_color32(),
            action_info: palette.action_info.to_color32(),
            action_destructive: palette.action_destructive.to_color32(),
            widget_bg_hover: Self::blend_color(primary_accent, widget_bg, 0.72),
            widget_bg_active: Self::blend_color(primary_accent, widget_bg, 0.58),
        });
        self.last_applied_palette = Some(palette.clone());
    }

    /// Returns the cached Color32 values, rebuilding the cache if the palette changed.
    fn color_cache(&self) -> &CachedPaletteColor32 {
        self.cached_color32
            .as_ref()
            .expect("color32 cache must be initialized before rendering")
    }

    pub fn color_primary_accent(&self) -> Color32 {
        self.color_cache().primary_accent
    }

    pub fn color_primary_accent_hover(&self) -> Color32 {
        self.color_cache().primary_accent_hover
    }

    pub fn color_primary_accent_active(&self) -> Color32 {
        self.color_cache().primary_accent_active
    }

    pub fn color_widget_bg(&self) -> Color32 {
        self.color_cache().widget_bg
    }

    pub fn color_main_bg(&self) -> Color32 {
        self.color_cache().main_bg
    }

    pub fn color_card_bg(&self) -> Color32 {
        self.color_cache().card_bg
    }

    pub fn color_server_offline_bg(&self) -> Color32 {
        self.color_cache().server_offline_bg
    }

    pub fn color_server_selected_bg(&self) -> Color32 {
        self.color_cache().server_selected_bg
    }

    pub fn color_server_selected_bg_hover(&self) -> Color32 {
        self.color_cache().server_selected_bg_hover
    }

    pub fn color_server_selected_stroke(&self) -> Color32 {
        self.color_cache().server_selected_stroke
    }

    pub fn color_text_normal(&self) -> Color32 {
        self.color_cache().text_normal
    }

    pub fn color_text_gray(&self) -> Color32 {
        self.color_cache().text_gray
    }

    pub fn color_text_dim(&self) -> Color32 {
        self.color_cache().text_dim
    }

    pub fn color_text_error(&self) -> Color32 {
        self.color_cache().text_error
    }

    pub fn color_error(&self) -> Color32 {
        self.color_cache().error
    }

    pub fn color_warn(&self) -> Color32 {
        self.color_cache().warn
    }

    pub fn color_debug(&self) -> Color32 {
        self.color_cache().debug
    }

    pub fn color_success(&self) -> Color32 {
        self.color_cache().success
    }

    pub fn color_success_muted(&self) -> Color32 {
        self.color_cache().success_muted
    }

    pub fn color_action_info(&self) -> Color32 {
        self.color_cache().action_info
    }

    pub fn color_action_destructive(&self) -> Color32 {
        self.color_cache().action_destructive
    }

    pub(in crate::ui::app) fn blend_color(start: Color32, end: Color32, factor: f32) -> Color32 {
        let factor = factor.clamp(0.0, 1.0);
        let blend = |a: u8, b: u8| -> u8 {
            (a as f32 + (b as f32 - a as f32) * factor)
                .round()
                .clamp(0.0, 255.0) as u8
        };

        Color32::from_rgba_unmultiplied(
            blend(start.r(), end.r()),
            blend(start.g(), end.g()),
            blend(start.b(), end.b()),
            blend(start.a(), end.a()),
        )
    }

    pub(in crate::ui::app) fn color_with_alpha(color: Color32, alpha_factor: f32) -> Color32 {
        let alpha = (color.a() as f32 * alpha_factor.clamp(0.0, 1.0))
            .round()
            .clamp(0.0, 255.0) as u8;
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
    }

    pub fn color_widget_bg_active(&self) -> Color32 {
        self.color_cache().widget_bg_active
    }

    pub fn set_pointing_cursor_output(output: &mut egui::PlatformOutput) {
        output.cursor_icon = CursorIcon::PointingHand;
    }

    pub fn add_sized_primary_button(
        &self,
        ui: &mut Ui,
        size: Vec2,
        button: Button<'_>,
        enabled: bool,
    ) -> egui::Response {
        ui.scope(|ui| {
            let visuals = &mut ui.style_mut().visuals.widgets;
            visuals.inactive.bg_fill = self.color_primary_accent();
            visuals.hovered.bg_fill = self.color_primary_accent_hover();
            visuals.hovered.weak_bg_fill = self.color_primary_accent_hover();
            visuals.active.bg_fill = self.color_primary_accent_active();
            visuals.active.weak_bg_fill = self.color_primary_accent_active();
            ui.add_enabled(
                enabled,
                button.fill(self.color_primary_accent()).min_size(size),
            )
        })
        .inner
    }

    pub fn color_checkbox_enabled(&self) -> Color32 {
        palette::CHECKBOX_ENABLED
    }

    pub fn color_addon_row_enabled_bg(&self) -> Color32 {
        addon_row_colors(
            self.color_widget_bg(),
            self.color_card_bg(),
            self.color_text_normal(),
        )
        .0
    }

    pub fn color_addon_row_disabled_bg(&self) -> Color32 {
        addon_row_colors(
            self.color_widget_bg(),
            self.color_card_bg(),
            self.color_text_normal(),
        )
        .1
    }

    pub fn square_icon_button_size(icon_size: f32, min_edge: f32) -> Vec2 {
        Vec2::splat((icon_size + 10.0).max(min_edge))
    }

    pub fn header_control_button_size(&self) -> Vec2 {
        Self::square_icon_button_size(
            self.settings_view_state
                .font_sizes
                .main_view
                .window_control_icons as f32,
            28.0,
        )
    }

    pub fn modal_icon_button_size(icon_size: f32) -> Vec2 {
        Self::square_icon_button_size(icon_size, 30.0)
    }

    pub fn toolbar_icon_button_size(icon_size: f32) -> Vec2 {
        Self::square_icon_button_size(icon_size, 30.0)
    }

    pub fn activity_log_toggle_button_size(&self) -> Vec2 {
        Self::square_icon_button_size(
            self.settings_view_state
                .font_sizes
                .main_view
                .activity_log_toggle_icon as f32,
            18.0,
        )
    }

    pub fn adaptive_button_height(font_size: f32, min_height: f32) -> f32 {
        (font_size + 22.0).max(min_height)
    }

    pub fn render_adaptive_tab_bar(
        &self,
        ui: &mut Ui,
        tabs: &[&str],
        selected_index: usize,
    ) -> Option<usize> {
        let tab_count = tabs.len();
        if tab_count == 0 {
            return None;
        }

        let mut clicked: Option<usize> = None;

        ui.scope(|ui| {
            let tab_spacing = ui.spacing().item_spacing.x;
            let available = ui.available_width().min(ui.clip_rect().width());
            let total_tab_spacing = tab_spacing * (tab_count.saturating_sub(1) as f32);
            let available_for_tabs = (available - total_tab_spacing).max(0.0);
            let labels: Vec<String> = tabs.iter().map(|label| tr(label)).collect();
            let tab_widths = adaptive_tab_widths(ui, &labels, available_for_tabs);
            let average_tab_width = available_for_tabs / tab_count as f32;
            let default_padding_x = ui.spacing().button_padding.x;
            let tight_width = 80.0;
            let relaxed_width = 140.0;
            let padding_blend =
                ((average_tab_width - tight_width) / (relaxed_width - tight_width)).clamp(0.0, 1.0);

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.spacing_mut().button_padding.x = default_padding_x * padding_blend;

                for (index, label) in labels.iter().enumerate() {
                    let is_selected = index == selected_index;
                    let color = if is_selected {
                        self.color_primary_accent()
                    } else {
                        self.color_main_bg()
                    };

                    let tab_button = ui
                        .add_sized(
                            Vec2::new(tab_widths[index], 30.0),
                            Button::new(
                                RichText::new(label.as_str()).color(self.color_text_normal()),
                            )
                            .truncate()
                            .min_size(Vec2::new(0.0, 30.0))
                            .fill(color),
                        )
                        .on_hover_text(label.as_str());
                    if tab_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if tab_button.clicked() {
                        clicked = Some(index);
                    }
                    if index + 1 < tab_count {
                        ui.add_space(tab_spacing);
                    }
                }
            });
        });

        clicked
    }

    pub fn header_bar_height(&self) -> f32 {
        (self.header_control_button_size().y + 12.0).max(40.0)
    }

    pub fn footer_bar_height(&self) -> f32 {
        (self.activity_log_toggle_button_size().y + 6.0).max(20.0)
    }

    pub fn ui_state_checkbox(
        ui: &mut Ui,
        checked: &mut bool,
        text: impl Into<String>,
    ) -> egui::Response {
        if !*checked {
            return ui.add(egui::Checkbox::new(checked, text.into()));
        }

        let is_dark = ui.visuals().dark_mode;
        let (base_fill, hover_fill, active_fill, border_color, label_color, check_color) =
            if is_dark {
                (
                    palette::CHECKBOX_ENABLED,
                    palette::CHECKBOX_ENABLED_HOVER,
                    palette::CHECKBOX_ENABLED_ACTIVE,
                    palette::CHECKBOX_ENABLED_BORDER,
                    palette::CHECKBOX_ENABLED_LABEL,
                    Color32::WHITE,
                )
            } else {
                (
                    palette::CHECKBOX_ENABLED_LIGHT,
                    palette::CHECKBOX_ENABLED_HOVER_LIGHT,
                    palette::CHECKBOX_ENABLED_ACTIVE_LIGHT,
                    palette::CHECKBOX_ENABLED_BORDER_LIGHT,
                    palette::CHECKBOX_ENABLED_LABEL_LIGHT,
                    Color32::WHITE,
                )
            };
        let label = text.into();

        ui.scope(|ui| {
            let style = ui.style_mut();

            style.visuals.widgets.inactive.bg_fill = base_fill;
            style.visuals.widgets.inactive.weak_bg_fill = base_fill;
            style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border_color);
            style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(2.0, check_color);

            style.visuals.widgets.hovered.bg_fill = hover_fill;
            style.visuals.widgets.hovered.weak_bg_fill = hover_fill;
            style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, border_color);
            style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(2.0, check_color);

            style.visuals.widgets.active.bg_fill = active_fill;
            style.visuals.widgets.active.weak_bg_fill = active_fill;
            style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, border_color);
            style.visuals.widgets.active.fg_stroke = egui::Stroke::new(2.0, check_color);

            style.visuals.widgets.open.bg_fill = base_fill;
            style.visuals.widgets.open.weak_bg_fill = base_fill;
            style.visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, border_color);
            style.visuals.widgets.open.fg_stroke = egui::Stroke::new(2.0, check_color);

            style.visuals.widgets.noninteractive.bg_fill = base_fill;
            style.visuals.widgets.noninteractive.weak_bg_fill = base_fill;
            style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border_color);
            style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(2.0, check_color);

            if label.is_empty() {
                ui.add(egui::Checkbox::new(checked, ""))
            } else {
                ui.add(egui::Checkbox::new(
                    checked,
                    RichText::new(label).color(label_color),
                ))
            }
        })
        .inner
    }

    /// Apply the user-selected global UI scale to the egui zoom factor.
    /// The percentage multiplies the platform's native scale, so 100% leaves
    /// the OS-reported scaling untouched. Only updates when the value actually
    /// differs to avoid forcing a relayout every frame.
    pub(in crate::ui::app) fn apply_runtime_ui_scale(&self, ctx: &egui::Context) {
        let target = (self.settings_view_state.ui_scale_percent as f32 / 100.0).clamp(0.05, 5.0);
        if (ctx.zoom_factor() - target).abs() > f32::EPSILON {
            ctx.set_zoom_factor(target);
        }
    }

    pub(in crate::ui::app) fn apply_runtime_palette_visuals(&mut self, ctx: &egui::Context) {
        let palette_changed = self
            .last_applied_palette
            .as_ref()
            .is_none_or(|last| *last != self.settings_view_state.palette_colors);

        // Rebuild the Color32 cache and Visuals only when palette actually changed.
        self.ensure_color32_cache();

        if !palette_changed {
            return;
        }

        let cache = self.color_cache();

        let dark_mode = uses_dark_visuals(cache.main_bg, cache.text_normal);
        let mut visuals = if dark_mode {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        visuals.dark_mode = dark_mode;
        visuals.override_text_color = Some(cache.text_normal);
        visuals.weak_text_color = Some(cache.text_gray);
        visuals.warn_fg_color = cache.warn;
        visuals.error_fg_color = cache.error;
        visuals.interact_cursor = Some(CursorIcon::PointingHand);

        // Preserve egui's original dark tones for unchanged default palette
        // colors. Once a related palette color is customized (including by a
        // preset), the surface follows that color instead.
        let primary_changed = cache.primary_accent != palette::PRIMARY_ACCENT;
        let main_changed = cache.main_bg != palette::MAIN_BG;
        let card_changed = cache.card_bg != palette::CARD_BG;
        let text_gray_changed = cache.text_gray != palette::TEXT_GRAY;
        if primary_changed {
            visuals.hyperlink_color = cache.primary_accent;
        }
        if main_changed {
            visuals.panel_fill = cache.main_bg;
            visuals.faint_bg_color = Self::blend_color(cache.main_bg, cache.text_normal, 0.06);
        }
        if card_changed {
            visuals.window_fill = cache.card_bg;
            visuals.extreme_bg_color = cache.card_bg;
            visuals.text_edit_bg_color = Some(cache.card_bg);
            visuals.code_bg_color = cache.card_bg;
            visuals.widgets.noninteractive.bg_fill = cache.card_bg;
            visuals.widgets.noninteractive.weak_bg_fill = cache.card_bg;
        }
        if text_gray_changed {
            visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, cache.text_gray);
        }
        visuals.widgets.noninteractive.fg_stroke.color = cache.text_normal;

        let widget_base = cache.widget_bg;
        let widget_hover = cache.widget_bg_hover;
        let widget_active = cache.widget_bg_active;
        let accent_stroke = egui::Stroke::new(2.0, cache.primary_accent);
        let focus_fill = Self::blend_color(cache.widget_bg, cache.primary_accent, 0.36);

        visuals.widgets.inactive.bg_fill = widget_base;
        visuals.widgets.inactive.weak_bg_fill = widget_base;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        visuals.widgets.inactive.fg_stroke.color = cache.text_normal;

        visuals.widgets.hovered.bg_fill = widget_hover;
        visuals.widgets.hovered.weak_bg_fill = widget_hover;
        visuals.widgets.hovered.bg_stroke = accent_stroke;
        visuals.widgets.hovered.fg_stroke.color = cache.text_normal;

        visuals.widgets.active.bg_fill = widget_active;
        visuals.widgets.active.weak_bg_fill = widget_active;
        visuals.widgets.active.bg_stroke = accent_stroke;
        visuals.widgets.active.fg_stroke.color = cache.text_normal;

        visuals.widgets.open.bg_fill = widget_active;
        visuals.widgets.open.weak_bg_fill = widget_active;
        visuals.widgets.open.bg_stroke = accent_stroke;
        visuals.widgets.open.fg_stroke.color = cache.text_normal;
        visuals.selection.bg_fill = focus_fill;
        visuals.selection.stroke = egui::Stroke::new(2.0, cache.text_normal);

        let active_theme = if dark_mode {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(active_theme);
        ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
        ctx.set_visuals_of(egui::Theme::Light, visuals);
        ctx.global_style_mut(|style| {
            style.spacing.interact_size.x = style.spacing.interact_size.x.max(44.0);
            style.spacing.interact_size.y = style.spacing.interact_size.y.max(28.0);
        });
    }
}

fn adaptive_tab_widths(ui: &Ui, labels: &[String], available_width: f32) -> Vec<f32> {
    let font_id = TextStyle::Button.resolve(ui.style());
    let default_padding_x = ui.spacing().button_padding.x;
    let text_widths: Vec<f32> = labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap(label.clone(), font_id.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
        .collect();
    let preferred_widths: Vec<f32> = text_widths
        .iter()
        .map(|text_width| (text_width + default_padding_x * 2.0 + 8.0).max(48.0))
        .collect();
    let fallback_widths: Vec<f32> = text_widths
        .iter()
        .zip(preferred_widths.iter())
        .map(|(text_width, preferred)| {
            (text_width * 0.55 + default_padding_x * 2.0)
                .max(48.0)
                .min(*preferred)
        })
        .collect();

    fit_tab_widths(&preferred_widths, &fallback_widths, available_width)
}

fn fit_tab_widths(
    preferred_widths: &[f32],
    fallback_widths: &[f32],
    available_width: f32,
) -> Vec<f32> {
    let tab_count = preferred_widths.len();
    if tab_count == 0 {
        return Vec::new();
    }
    if available_width <= 0.0 {
        return vec![0.0; tab_count];
    }

    let preferred_total: f32 = preferred_widths.iter().sum();
    if preferred_total <= available_width {
        let extra_width = (available_width - preferred_total) / tab_count as f32;
        return preferred_widths
            .iter()
            .map(|width| width + extra_width)
            .collect();
    }

    let fallback_total: f32 = fallback_widths.iter().sum();
    if fallback_total >= available_width {
        let scale = available_width / fallback_total.max(1.0);
        return fallback_widths.iter().map(|width| width * scale).collect();
    }

    let mut widths = preferred_widths.to_vec();
    let mut excess_width = preferred_total - available_width;
    for _ in 0..(tab_count * 4) {
        if excess_width <= 0.5 {
            break;
        }

        let flexible_indices: Vec<usize> = widths
            .iter()
            .zip(fallback_widths.iter())
            .enumerate()
            .filter_map(|(index, (width, fallback_width))| {
                (*width > *fallback_width + 0.5).then_some(index)
            })
            .collect();
        if flexible_indices.is_empty() {
            break;
        }

        let shrink_per_tab = excess_width / flexible_indices.len() as f32;
        let mut progress = 0.0;
        for index in flexible_indices {
            let reduction = shrink_per_tab.min(widths[index] - fallback_widths[index]);
            widths[index] -= reduction;
            excess_width -= reduction;
            progress += reduction;
        }
        if progress <= 0.01 {
            break;
        }
    }

    let fitted_total: f32 = widths.iter().sum();
    if fitted_total < available_width {
        let extra_width = (available_width - fitted_total) / tab_count as f32;
        for width in &mut widths {
            *width += extra_width;
        }
    }

    widths
}

fn uses_dark_visuals(background: Color32, text: Color32) -> bool {
    relative_luminance(background) < relative_luminance(text)
}

fn relative_luminance(color: Color32) -> u32 {
    2126 * u32::from(color.r()) + 7152 * u32::from(color.g()) + 722 * u32::from(color.b())
}

fn addon_row_colors(widget_bg: Color32, card_bg: Color32, text: Color32) -> (Color32, Color32) {
    let enabled = Foxy::blend_color(widget_bg, card_bg, 0.08);
    let disabled_shade = if uses_dark_visuals(card_bg, text) {
        0.37
    } else {
        0.05
    };
    let disabled = Foxy::blend_color(card_bg, Color32::BLACK, disabled_shade);
    (enabled, disabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_visual_mode_from_palette_contrast() {
        assert!(uses_dark_visuals(
            Color32::from_rgb(45, 45, 45),
            Color32::WHITE
        ));
        assert!(!uses_dark_visuals(
            Color32::from_rgb(235, 235, 235),
            Color32::from_rgb(20, 20, 20)
        ));
    }

    #[test]
    fn addon_rows_follow_dark_and_light_palette_surfaces() {
        assert_eq!(
            addon_row_colors(
                Color32::from_rgb(60, 60, 60),
                Color32::from_rgb(35, 35, 35),
                Color32::WHITE,
            ),
            (Color32::from_rgb(58, 58, 58), Color32::from_rgb(22, 22, 22),)
        );
        assert_eq!(
            addon_row_colors(
                Color32::from_rgb(210, 210, 210),
                Color32::from_rgb(250, 250, 250),
                Color32::from_rgb(20, 20, 20),
            ),
            (
                Color32::from_rgb(213, 213, 213),
                Color32::from_rgb(238, 238, 238),
            )
        );
    }

    #[test]
    fn tab_widths_use_natural_widths_before_truncating() {
        let preferred = [90.0, 210.0, 80.0];
        let fallback = [60.0, 120.0, 56.0];

        let widths = fit_tab_widths(&preferred, &fallback, 500.0);

        assert!(widths[1] > widths[0]);
        assert!(widths[0] > widths[2]);
        assert!((widths.iter().sum::<f32>() - 500.0).abs() < 0.01);
    }

    #[test]
    fn tab_widths_shrink_to_fit_before_fallback() {
        let preferred = [90.0, 210.0, 80.0];
        let fallback = [60.0, 120.0, 56.0];

        let widths = fit_tab_widths(&preferred, &fallback, 300.0);

        assert!(widths[1] > widths[0]);
        assert!(widths[0] >= fallback[0]);
        assert!(widths[1] >= fallback[1]);
        assert!(widths[2] >= fallback[2]);
        assert!((widths.iter().sum::<f32>() - 300.0).abs() < 0.01);
    }
}
