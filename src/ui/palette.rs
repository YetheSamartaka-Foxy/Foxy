use egui::Color32;
use serde::{Deserialize, Serialize};

/// Highlight brand color (Used for top bar, launch buttons, etc)
pub const PRIMARY_ACCENT: Color32 = Color32::from_rgb(200, 100, 15);

/// A slightly lighter gray for buttons or selected states
pub const WIDGET_BG: Color32 = Color32::from_rgb(60, 60, 60);

/// A darker background color (Used for panels, bottom bar, etc)
pub const MAIN_BG: Color32 = Color32::from_rgb(45, 45, 45);

/// A dark gray background for cards or frames
pub const CARD_BG: Color32 = Color32::from_rgb(35, 35, 35);

/// An even darker background to represent offline servers (or a disabled state)
pub const SERVER_OFFLINE_BG: Color32 = Color32::from_rgb(30, 30, 30);

/// Normal, bright text color
pub const TEXT_NORMAL: Color32 = Color32::WHITE;

/// Gray text color
pub const TEXT_GRAY: Color32 = Color32::GRAY;

/// Dimmer text color (Used for hints, help messages, or disabled content)
pub const TEXT_DIM: Color32 = Color32::from_gray(150);

/// Error or destructive color
pub const TEXT_ERROR: Color32 = Color32::from_rgb(122, 31, 39);

/// Bright red for error log messages
pub const ERROR: Color32 = Color32::from_rgb(255, 80, 80);

/// Orange for warning log messages
pub const WARN: Color32 = Color32::from_rgb(255, 180, 50);

/// Blue for debug log messages
pub const DEBUG: Color32 = Color32::from_rgb(90, 150, 255);

/// Green used for success/completed actions
pub const SUCCESS: Color32 = Color32::from_rgb(0, 170, 0);

/// Darker green used for success banners and secondary success actions
pub const SUCCESS_MUTED: Color32 = Color32::from_rgb(0, 120, 0);

/// Blue action color used by repository status banners
pub const ACTION_INFO: Color32 = Color32::from_rgb(50, 50, 200);

/// Red action color used by destructive/pending update actions
pub const ACTION_DESTRUCTIVE: Color32 = Color32::from_rgb(200, 50, 50);

/// Green used to indicate enabled checkbox states
pub const CHECKBOX_ENABLED: Color32 = Color32::from_rgb(70, 170, 70);

/// Enabled checkbox color when hovered
pub const CHECKBOX_ENABLED_HOVER: Color32 = Color32::from_rgb(82, 192, 82);

/// Enabled checkbox color when pressed/active
pub const CHECKBOX_ENABLED_ACTIVE: Color32 = Color32::from_rgb(62, 150, 62);

/// Border color for enabled checkbox state
pub const CHECKBOX_ENABLED_BORDER: Color32 = Color32::from_rgb(48, 118, 48);

/// Label color for enabled checkbox state
pub const CHECKBOX_ENABLED_LABEL: Color32 = Color32::from_rgb(118, 218, 118);

/// Enabled checkbox colors used when the active palette has a light surface.
pub const CHECKBOX_ENABLED_LIGHT: Color32 = Color32::from_rgb(36, 145, 36);
pub const CHECKBOX_ENABLED_HOVER_LIGHT: Color32 = Color32::from_rgb(45, 165, 45);
pub const CHECKBOX_ENABLED_ACTIVE_LIGHT: Color32 = Color32::from_rgb(30, 125, 30);
pub const CHECKBOX_ENABLED_BORDER_LIGHT: Color32 = Color32::from_rgb(24, 100, 24);
pub const CHECKBOX_ENABLED_LABEL_LIGHT: Color32 = Color32::from_rgb(0, 105, 0);

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
