//! Theme import/export and built-in presets.
//!
//! A theme is a portable snapshot of the user-customizable presentation:
//! per-view font sizes and the palette colors. Themes are serialized as JSON so
//! they can be shared between installs and distributed as defaults.
//!
//! Parsing is intentionally tolerant: a theme file only needs to carry the keys
//! it wants to override. Missing keys fall back to the built-in defaults by
//! merging the parsed JSON over a serialized default, which keeps older theme
//! files forward-compatible as new font/color keys are added.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui::fonts::FontSizes;
use crate::ui::palette::{PaletteColors, RgbColor};

/// Current on-disk theme schema version. Bump when the meaning of existing keys
/// changes in a way that needs migration; additive keys do not require a bump.
pub const THEME_FORMAT_VERSION: u32 = 1;

/// A portable snapshot of the customizable presentation settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub font_sizes: FontSizes,
    #[serde(default)]
    pub palette_colors: PaletteColors,
}

fn default_version() -> u32 {
    THEME_FORMAT_VERSION
}

impl Theme {
    /// Build a theme from the current font sizes and palette colors.
    pub fn from_current(
        name: impl Into<String>,
        font_sizes: FontSizes,
        palette: PaletteColors,
    ) -> Self {
        Self {
            version: THEME_FORMAT_VERSION,
            name: name.into(),
            font_sizes,
            palette_colors: palette,
        }
    }

    /// Serialize the theme to pretty JSON suitable for writing to a file.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse a theme from JSON.
    ///
    /// Missing keys are tolerated: the parsed values are merged over the
    /// built-in defaults so partial or older theme files still load. Font sizes
    /// are clamped to their valid ranges. An error is returned only when the
    /// JSON is malformed, is not an object, or carries a value of the wrong type
    /// for a known key.
    pub fn from_json(json: &str) -> Result<Self, ThemeError> {
        let root: Value =
            serde_json::from_str(json).map_err(|err| ThemeError::Malformed(err.to_string()))?;
        if !root.is_object() {
            return Err(ThemeError::NotAnObject);
        }

        let font_sizes = merge_over_default::<FontSizes>(&root, "font_sizes")?;
        let palette_colors = merge_over_default::<PaletteColors>(&root, "palette_colors")?;
        let version = root
            .get("version")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(THEME_FORMAT_VERSION);
        let name = root
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let mut theme = Self {
            version,
            name,
            font_sizes,
            palette_colors,
        };
        theme.font_sizes.clamp_to_limits();
        Ok(theme)
    }
}

/// Errors produced while parsing a theme file. The variants stay
/// machine-distinguishable so the UI can map them to localized messages.
#[derive(Debug)]
pub enum ThemeError {
    /// The input was not valid JSON.
    Malformed(String),
    /// The top-level JSON value was not an object.
    NotAnObject,
    /// A known field carried a value of the wrong type.
    InvalidField { field: String, detail: String },
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeError::Malformed(detail) => write!(f, "malformed JSON: {detail}"),
            ThemeError::NotAnObject => write!(f, "theme root is not a JSON object"),
            ThemeError::InvalidField { field, detail } => {
                write!(f, "invalid value for '{field}': {detail}")
            }
        }
    }
}

impl std::error::Error for ThemeError {}

/// Deserialize `root[key]` into `T`, merging any present keys over a serialized
/// `T::default()`. This lets a theme override only the keys it cares about while
/// every other key (including ones added in newer builds) keeps its default.
fn merge_over_default<T>(root: &Value, key: &str) -> Result<T, ThemeError>
where
    T: Default + Serialize + DeserializeOwned,
{
    let mut base = serde_json::to_value(T::default()).map_err(|err| ThemeError::InvalidField {
        field: key.to_string(),
        detail: err.to_string(),
    })?;
    if let Some(overrides) = root.get(key) {
        merge_value(&mut base, overrides);
    }
    serde_json::from_value(base).map_err(|err| ThemeError::InvalidField {
        field: key.to_string(),
        detail: err.to_string(),
    })
}

/// Recursively overlay `overrides` onto `base`. Objects are merged key by key;
/// any other value type replaces the base value outright.
fn merge_value(base: &mut Value, overrides: &Value) {
    match (base, overrides) {
        (Value::Object(base_map), Value::Object(override_map)) => {
            for (key, override_value) in override_map {
                merge_value(
                    base_map.entry(key.clone()).or_insert(Value::Null),
                    override_value,
                );
            }
        }
        (base_slot, override_value) => {
            *base_slot = override_value.clone();
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

/// A named, built-in theme preset the user can apply with one click.
pub struct ThemePreset {
    /// English key, also used as the i18n lookup key for the button label.
    pub name: &'static str,
    /// Palette applied by this preset. Font sizes always reset to defaults.
    pub palette: PaletteColors,
}

impl ThemePreset {
    /// Materialize this preset into a full [`Theme`].
    pub fn to_theme(&self) -> Theme {
        Theme::from_current(self.name, FontSizes::default(), self.palette.clone())
    }
}

/// The built-in presets distributed with the app, in display order.
pub fn builtin_presets() -> Vec<ThemePreset> {
    vec![
        ThemePreset {
            name: "Default (Dark)",
            palette: PaletteColors::default(),
        },
        ThemePreset {
            name: "Swiftier",
            palette: PaletteColors {
                primary_accent: rgb(100, 166, 121),
                widget_bg: rgb(58, 58, 58),
                main_bg: rgb(42, 42, 42),
                card_bg: rgb(58, 58, 58),
                server_offline_bg: rgb(47, 47, 47),
                text_normal: rgb(245, 245, 245),
                text_gray: rgb(190, 190, 190),
                text_dim: rgb(155, 155, 155),
                text_error: rgb(242, 48, 28),
                error: rgb(242, 48, 28),
                warn: rgb(235, 176, 70),
                debug: rgb(100, 170, 220),
                success: rgb(101, 166, 121),
                success_muted: rgb(40, 99, 59),
                action_info: rgb(40, 99, 59),
                action_destructive: rgb(242, 48, 28),
            },
        },
        ThemePreset {
            name: "Light",
            palette: PaletteColors {
                primary_accent: rgb(200, 100, 15),
                widget_bg: rgb(210, 210, 210),
                main_bg: rgb(235, 235, 235),
                card_bg: rgb(250, 250, 250),
                server_offline_bg: rgb(220, 220, 220),
                text_normal: rgb(20, 20, 20),
                text_gray: rgb(90, 90, 90),
                text_dim: rgb(120, 120, 120),
                text_error: rgb(170, 40, 40),
                error: rgb(200, 40, 40),
                warn: rgb(190, 120, 0),
                debug: rgb(40, 90, 200),
                success: rgb(0, 150, 0),
                success_muted: rgb(0, 110, 0),
                action_info: rgb(40, 90, 200),
                action_destructive: rgb(200, 50, 50),
            },
        },
        ThemePreset {
            name: "High Contrast",
            palette: PaletteColors {
                primary_accent: rgb(255, 140, 0),
                widget_bg: rgb(0, 0, 0),
                main_bg: rgb(0, 0, 0),
                card_bg: rgb(0, 0, 0),
                server_offline_bg: rgb(12, 12, 12),
                text_normal: rgb(255, 255, 255),
                text_gray: rgb(200, 200, 200),
                text_dim: rgb(180, 180, 180),
                text_error: rgb(255, 80, 80),
                error: rgb(255, 80, 80),
                warn: rgb(255, 200, 0),
                debug: rgb(100, 180, 255),
                success: rgb(0, 255, 0),
                success_muted: rgb(0, 200, 0),
                action_info: rgb(0, 150, 255),
                action_destructive: rgb(255, 60, 60),
            },
        },
        ThemePreset {
            name: "Nord",
            palette: PaletteColors {
                primary_accent: rgb(136, 192, 208),
                widget_bg: rgb(67, 76, 94),
                main_bg: rgb(46, 52, 64),
                card_bg: rgb(59, 66, 82),
                server_offline_bg: rgb(38, 43, 54),
                text_normal: rgb(236, 239, 244),
                text_gray: rgb(171, 178, 191),
                text_dim: rgb(143, 150, 165),
                text_error: rgb(191, 97, 106),
                error: rgb(191, 97, 106),
                warn: rgb(235, 203, 139),
                debug: rgb(129, 161, 193),
                success: rgb(163, 190, 140),
                success_muted: rgb(133, 160, 110),
                action_info: rgb(94, 129, 172),
                action_destructive: rgb(191, 97, 106),
            },
        },
        ThemePreset {
            name: "Solarized Dark",
            palette: PaletteColors {
                primary_accent: rgb(38, 139, 210),
                widget_bg: rgb(88, 110, 117),
                main_bg: rgb(0, 43, 54),
                card_bg: rgb(7, 54, 66),
                server_offline_bg: rgb(0, 33, 43),
                text_normal: rgb(147, 161, 161),
                text_gray: rgb(101, 123, 131),
                text_dim: rgb(88, 110, 117),
                text_error: rgb(220, 50, 47),
                error: rgb(220, 50, 47),
                warn: rgb(181, 137, 0),
                debug: rgb(38, 139, 210),
                success: rgb(133, 153, 0),
                success_muted: rgb(100, 115, 0),
                action_info: rgb(38, 139, 210),
                action_destructive: rgb(220, 50, 47),
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_current_theme() {
        let theme = Theme::from_current("My Theme", FontSizes::default(), PaletteColors::default());
        let json = theme.to_json().expect("serialize");
        let parsed = Theme::from_json(&json).expect("parse");
        assert_eq!(parsed, theme);
    }

    #[test]
    fn parses_partial_theme_filling_defaults() {
        // Only one palette color and one font size provided; everything else
        // must fall back to the built-in defaults.
        let json = r#"{
            "name": "Partial",
            "palette_colors": { "primary_accent": { "r": 1, "g": 2, "b": 3 } },
            "font_sizes": { "about_view": { "h1": 28 } }
        }"#;
        let theme = Theme::from_json(json).expect("parse");
        assert_eq!(theme.palette_colors.primary_accent, rgb(1, 2, 3));
        // Untouched palette key keeps its default.
        assert_eq!(
            theme.palette_colors.widget_bg,
            PaletteColors::default().widget_bg
        );
        assert_eq!(theme.font_sizes.about_view.h1, 28);
        // Untouched font key keeps its default.
        assert_eq!(
            theme.font_sizes.about_view.body,
            FontSizes::default().about_view.body
        );
    }

    #[test]
    fn clamps_out_of_range_font_sizes() {
        let json = r#"{ "font_sizes": { "about_view": { "h1": 9999 } } }"#;
        let theme = Theme::from_json(json).expect("parse");
        assert_eq!(
            theme.font_sizes.about_view.h1,
            crate::ui::fonts::ABOUT_H1_RANGE.max
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            Theme::from_json("not json"),
            Err(ThemeError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_non_object_root() {
        assert!(matches!(
            Theme::from_json("[1, 2, 3]"),
            Err(ThemeError::NotAnObject)
        ));
    }

    #[test]
    fn rejects_wrong_typed_field() {
        let json = r#"{ "palette_colors": { "primary_accent": "red" } }"#;
        assert!(matches!(
            Theme::from_json(json),
            Err(ThemeError::InvalidField { .. })
        ));
    }

    #[test]
    fn builtin_presets_are_well_formed() {
        for preset in builtin_presets() {
            let theme = preset.to_theme();
            let json = theme.to_json().expect("serialize preset");
            let parsed = Theme::from_json(&json).expect("parse preset");
            assert_eq!(parsed.palette_colors, preset.palette);
        }
    }
}
