use egui::Color32;
use serde::{Deserialize, Serialize};

pub const PRIMARY_ACCENT: Color32 = Color32::from_rgb(200, 100, 15);

pub const WIDGET_BG: Color32 = Color32::from_rgb(60, 60, 60);

pub const MAIN_BG: Color32 = Color32::from_rgb(45, 45, 45);

pub const CARD_BG: Color32 = Color32::from_rgb(35, 35, 35);

pub const SERVER_OFFLINE_BG: Color32 = Color32::from_rgb(30, 30, 30);

pub const TEXT_NORMAL: Color32 = Color32::WHITE;

pub const TEXT_GRAY: Color32 = Color32::from_gray(196);

pub const TEXT_DIM: Color32 = Color32::from_gray(168);

pub const TEXT_ERROR: Color32 = Color32::from_rgb(255, 132, 132);

pub const ERROR: Color32 = Color32::from_rgb(255, 132, 132);

pub const WARN: Color32 = Color32::from_rgb(255, 180, 50);

pub const DEBUG: Color32 = Color32::from_rgb(118, 172, 255);

pub const SUCCESS: Color32 = Color32::from_rgb(86, 206, 86);

pub const SUCCESS_MUTED: Color32 = Color32::from_rgb(0, 120, 0);

pub const ACTION_INFO: Color32 = Color32::from_rgb(50, 50, 200);

pub const ACTION_DESTRUCTIVE: Color32 = Color32::from_rgb(200, 50, 50);

pub const ON_STRONG_FILL: Color32 = Color32::WHITE;

pub const ON_LIGHT_FILL: Color32 = Color32::from_rgb(16, 18, 22);

pub const BAR_LABEL_SHADE: Color32 = Color32::BLACK;

pub const AGENT_CAPTION_BACKDROP: Color32 = Color32::from_black_alpha(120);

pub const CHECKBOX_ENABLED: Color32 = Color32::from_rgb(38, 134, 48);

pub const CHECKBOX_ENABLED_HOVER: Color32 = Color32::from_rgb(44, 146, 54);

pub const CHECKBOX_ENABLED_ACTIVE: Color32 = Color32::from_rgb(32, 118, 40);

pub const CHECKBOX_ENABLED_BORDER: Color32 = Color32::from_rgb(24, 96, 32);

pub const CHECKBOX_ENABLED_LABEL: Color32 = Color32::from_rgb(118, 218, 118);

pub const CHECKBOX_ENABLED_LIGHT: Color32 = Color32::from_rgb(36, 145, 36);
pub const CHECKBOX_ENABLED_HOVER_LIGHT: Color32 = Color32::from_rgb(45, 165, 45);
pub const CHECKBOX_ENABLED_ACTIVE_LIGHT: Color32 = Color32::from_rgb(30, 125, 30);
pub const CHECKBOX_ENABLED_BORDER_LIGHT: Color32 = Color32::from_rgb(24, 100, 24);
pub const CHECKBOX_ENABLED_LABEL_LIGHT: Color32 = Color32::from_rgb(0, 105, 0);

pub const BENCHMARK_SERIES: [Color32; 6] = [
    Color32::from_rgb(86, 156, 214),
    Color32::from_rgb(232, 145, 56),
    Color32::from_rgb(104, 190, 120),
    Color32::from_rgb(214, 96, 110),
    Color32::from_rgb(170, 130, 220),
    Color32::from_rgb(210, 190, 80),
];
pub const BENCHMARK_COMPARE_A: Color32 = Color32::from_rgb(86, 180, 233);
pub const BENCHMARK_COMPARE_B: Color32 = Color32::from_rgb(230, 159, 0);
pub const BENCHMARK_CHART_GRID: Color32 = Color32::from_gray(66);
pub const BENCHMARK_CHART_AXIS: Color32 = Color32::from_gray(96);

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize, Default)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl RgbColor {
    pub fn from_color32(color: Color32) -> Self {
        Self {
            r: color.r(),
            g: color.g(),
            b: color.b(),
        }
    }

    pub fn to_color32(&self) -> Color32 {
        Color32::from_rgb(self.r, self.g, self.b)
    }
}

fn default_success_color() -> RgbColor {
    RgbColor::from_color32(SUCCESS)
}

fn default_success_muted_color() -> RgbColor {
    RgbColor::from_color32(SUCCESS_MUTED)
}

fn default_action_info_color() -> RgbColor {
    RgbColor::from_color32(ACTION_INFO)
}

fn default_action_destructive_color() -> RgbColor {
    RgbColor::from_color32(ACTION_DESTRUCTIVE)
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct PaletteColors {
    #[serde(default)]
    pub primary_accent: RgbColor,
    #[serde(default)]
    pub widget_bg: RgbColor,
    #[serde(default)]
    pub main_bg: RgbColor,
    #[serde(default)]
    pub card_bg: RgbColor,
    #[serde(default)]
    pub server_offline_bg: RgbColor,
    #[serde(default)]
    pub text_normal: RgbColor,
    #[serde(default)]
    pub text_gray: RgbColor,
    #[serde(default)]
    pub text_dim: RgbColor,
    #[serde(default)]
    pub text_error: RgbColor,
    #[serde(default)]
    pub error: RgbColor,
    #[serde(default)]
    pub warn: RgbColor,
    #[serde(default)]
    pub debug: RgbColor,
    #[serde(default = "default_success_color")]
    pub success: RgbColor,
    #[serde(default = "default_success_muted_color")]
    pub success_muted: RgbColor,
    #[serde(default = "default_action_info_color")]
    pub action_info: RgbColor,
    #[serde(default = "default_action_destructive_color")]
    pub action_destructive: RgbColor,
}

impl Default for PaletteColors {
    fn default() -> Self {
        Self {
            primary_accent: RgbColor::from_color32(PRIMARY_ACCENT),
            widget_bg: RgbColor::from_color32(WIDGET_BG),
            main_bg: RgbColor::from_color32(MAIN_BG),
            card_bg: RgbColor::from_color32(CARD_BG),
            server_offline_bg: RgbColor::from_color32(SERVER_OFFLINE_BG),
            text_normal: RgbColor::from_color32(TEXT_NORMAL),
            text_gray: RgbColor::from_color32(TEXT_GRAY),
            text_dim: RgbColor::from_color32(TEXT_DIM),
            text_error: RgbColor::from_color32(TEXT_ERROR),
            error: RgbColor::from_color32(ERROR),
            warn: RgbColor::from_color32(WARN),
            debug: RgbColor::from_color32(DEBUG),
            success: default_success_color(),
            success_muted: default_success_muted_color(),
            action_info: default_action_info_color(),
            action_destructive: default_action_destructive_color(),
        }
    }
}

/// Computes the WCAG 2.2 relative luminance of an opaque sRGB color.
pub fn relative_luminance(color: Color32) -> f64 {
    fn linearize(channel: u8) -> f64 {
        let s = f64::from(channel) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linearize(color.r()) + 0.7152 * linearize(color.g()) + 0.0722 * linearize(color.b())
}

/// Computes the WCAG 2.2 contrast ratio between two opaque colors (1.0 to 21.0).
pub fn contrast_ratio(a: Color32, b: Color32) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Selects high-contrast foreground text (light or dark ink) for a solid fill color.
pub fn contrast_text_on(background: Color32) -> Color32 {
    if contrast_ratio(ON_STRONG_FILL, background) >= contrast_ratio(ON_LIGHT_FILL, background) {
        ON_STRONG_FILL
    } else {
        ON_LIGHT_FILL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_constants_meet_wcag_aa_contrast() {
        for (name, fg, bg) in [
            ("text_normal/main_bg", TEXT_NORMAL, MAIN_BG),
            ("text_normal/card_bg", TEXT_NORMAL, CARD_BG),
            ("text_normal/widget_bg", TEXT_NORMAL, WIDGET_BG),
            ("text_gray/main_bg", TEXT_GRAY, MAIN_BG),
            ("text_dim/card_bg", TEXT_DIM, CARD_BG),
            ("text_error/widget_bg", TEXT_ERROR, WIDGET_BG),
            ("error/main_bg", ERROR, MAIN_BG),
            ("warn/main_bg", WARN, MAIN_BG),
            ("debug/main_bg", DEBUG, MAIN_BG),
            ("success/main_bg", SUCCESS, MAIN_BG),
            (
                "checkbox_label/widget_bg",
                CHECKBOX_ENABLED_LABEL,
                WIDGET_BG,
            ),
            (
                "on_accent/primary_accent",
                contrast_text_on(PRIMARY_ACCENT),
                PRIMARY_ACCENT,
            ),
            (
                "on_info/action_info",
                contrast_text_on(ACTION_INFO),
                ACTION_INFO,
            ),
            (
                "on_destructive/action_destructive",
                contrast_text_on(ACTION_DESTRUCTIVE),
                ACTION_DESTRUCTIVE,
            ),
            (
                "on_checkbox/checkbox_enabled",
                contrast_text_on(CHECKBOX_ENABLED),
                CHECKBOX_ENABLED,
            ),
        ] {
            let ratio = contrast_ratio(fg, bg);
            assert!(
                ratio >= 4.5,
                "palette pair `{name}` contrast {ratio:.2}:1 below WCAG AA 4.5:1"
            );
        }
    }
}
