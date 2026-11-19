//! Theme files are partial portable snapshots; missing keys inherit defaults so older files remain forward-compatible.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui::fonts::FontSizes;
use crate::ui::palette::{PaletteColors, RgbColor};

/// Bump this version only when existing key meanings require migration; additive keys remain compatible.
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

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Partial themes merge over defaults; font sizes are clamped. Errors are limited to malformed JSON, non-object roots, or wrong types for known keys.
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

/// Keep parse errors distinguishable so the UI can map each one to localized text.
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

pub struct ThemePreset {
    pub name: &'static str,
    /// Palette applied by this preset. Font sizes always reset to defaults.
    pub palette: PaletteColors,
}

impl ThemePreset {
    pub fn to_theme(&self) -> Theme {
        Theme::from_current(self.name, FontSizes::default(), self.palette.clone())
    }
}

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
                text_gray: rgb(196, 196, 196),
                text_dim: rgb(172, 172, 172),
                text_error: rgb(255, 134, 124),
                error: rgb(255, 134, 124),
                warn: rgb(235, 176, 70),
                debug: rgb(118, 176, 228),
                success: rgb(124, 204, 148),
                success_muted: rgb(40, 99, 59),
                action_info: rgb(40, 99, 59),
                action_destructive: rgb(194, 44, 32),
            },
        },
        ThemePreset {
            name: "Red",
            palette: PaletteColors {
                primary_accent: rgb(188, 58, 54),
                widget_bg: rgb(62, 48, 49),
                main_bg: rgb(43, 38, 39),
                card_bg: rgb(35, 30, 31),
                server_offline_bg: rgb(29, 25, 26),
                text_normal: rgb(246, 244, 244),
                text_gray: rgb(196, 184, 184),
                text_dim: rgb(174, 162, 162),
                text_error: rgb(255, 134, 130),
                error: rgb(255, 134, 130),
                warn: rgb(229, 166, 67),
                debug: rgb(124, 164, 230),
                success: rgb(96, 184, 104),
                success_muted: rgb(50, 126, 63),
                action_info: rgb(170, 72, 82),
                action_destructive: rgb(194, 52, 48),
            },
        },
        ThemePreset {
            name: "Austrian Owl",
            palette: PaletteColors {
                primary_accent: rgb(157, 91, 60),
                widget_bg: rgb(75, 61, 55),
                main_bg: rgb(47, 41, 38),
                card_bg: rgb(58, 50, 46),
                server_offline_bg: rgb(40, 35, 33),
                text_normal: rgb(250, 246, 238),
                text_gray: rgb(230, 218, 202),
                text_dim: rgb(199, 181, 159),
                text_error: rgb(255, 148, 128),
                error: rgb(255, 148, 128),
                warn: rgb(229, 171, 84),
                debug: rgb(154, 180, 226),
                success: rgb(138, 200, 124),
                success_muted: rgb(52, 116, 52),
                action_info: rgb(178, 126, 82),
                action_destructive: rgb(184, 64, 46),
            },
        },
        ThemePreset {
            name: "Viola",
            palette: PaletteColors {
                primary_accent: rgb(154, 104, 226),
                widget_bg: rgb(61, 49, 78),
                main_bg: rgb(39, 33, 50),
                card_bg: rgb(49, 40, 63),
                server_offline_bg: rgb(33, 28, 43),
                text_normal: rgb(244, 241, 248),
                text_gray: rgb(199, 190, 211),
                text_dim: rgb(174, 162, 190),
                text_error: rgb(255, 136, 160),
                error: rgb(255, 136, 160),
                warn: rgb(231, 176, 79),
                debug: rgb(132, 171, 238),
                success: rgb(104, 190, 132),
                success_muted: rgb(42, 118, 74),
                action_info: rgb(106, 80, 192),
                action_destructive: rgb(182, 54, 90),
            },
        },
        ThemePreset {
            name: "Light",
            palette: PaletteColors {
                primary_accent: rgb(202, 96, 20),
                widget_bg: rgb(205, 210, 216),
                main_bg: rgb(222, 226, 231),
                card_bg: rgb(236, 239, 243),
                server_offline_bg: rgb(212, 217, 224),
                text_normal: rgb(29, 34, 41),
                text_gray: rgb(64, 72, 82),
                text_dim: rgb(82, 90, 102),
                text_error: rgb(156, 30, 30),
                error: rgb(164, 34, 34),
                warn: rgb(130, 74, 0),
                debug: rgb(28, 86, 158),
                success: rgb(22, 104, 44),
                success_muted: rgb(29, 107, 54),
                action_info: rgb(43, 101, 170),
                action_destructive: rgb(185, 58, 53),
            },
        },
        ThemePreset {
            name: "High Contrast",
            palette: PaletteColors {
                primary_accent: rgb(220, 116, 24),
                widget_bg: rgb(36, 38, 41),
                main_bg: rgb(22, 24, 27),
                card_bg: rgb(31, 34, 38),
                server_offline_bg: rgb(27, 29, 32),
                text_normal: rgb(238, 241, 245),
                text_gray: rgb(190, 196, 204),
                text_dim: rgb(154, 162, 172),
                text_error: rgb(239, 93, 86),
                error: rgb(239, 93, 86),
                warn: rgb(236, 181, 78),
                debug: rgb(117, 169, 231),
                success: rgb(87, 185, 115),
                success_muted: rgb(55, 150, 84),
                action_info: rgb(95, 159, 217),
                action_destructive: rgb(218, 75, 70),
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
                text_gray: rgb(194, 202, 214),
                text_dim: rgb(182, 190, 204),
                text_error: rgb(255, 166, 170),
                error: rgb(255, 166, 170),
                warn: rgb(235, 203, 139),
                debug: rgb(162, 194, 232),
                success: rgb(172, 202, 148),
                success_muted: rgb(133, 160, 110),
                action_info: rgb(74, 110, 152),
                action_destructive: rgb(191, 97, 106),
            },
        },
        ThemePreset {
            name: "Solarized Dark",
            palette: PaletteColors {
                primary_accent: rgb(38, 139, 210),
                widget_bg: rgb(18, 68, 82),
                main_bg: rgb(0, 43, 54),
                card_bg: rgb(7, 54, 66),
                server_offline_bg: rgb(0, 33, 43),
                text_normal: rgb(238, 238, 238),
                text_gray: rgb(202, 210, 211),
                text_dim: rgb(168, 184, 188),
                text_error: rgb(255, 136, 126),
                error: rgb(255, 136, 126),
                warn: rgb(224, 178, 36),
                debug: rgb(114, 184, 240),
                success: rgb(168, 192, 40),
                success_muted: rgb(78, 112, 0),
                action_info: rgb(28, 110, 176),
                action_destructive: rgb(194, 46, 44),
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

    #[test]
    fn builtin_presets_meet_wcag_aa_contrast() {
        use crate::ui::palette::{contrast_ratio, contrast_text_on};

        let mut failures = Vec::new();
        for preset in builtin_presets() {
            let p = &preset.palette;
            let surfaces = [
                ("main_bg", p.main_bg.to_color32()),
                ("card_bg", p.card_bg.to_color32()),
                ("widget_bg", p.widget_bg.to_color32()),
                ("server_offline_bg", p.server_offline_bg.to_color32()),
            ];
            let fg_tokens = [
                ("text_normal", p.text_normal.to_color32()),
                ("text_gray", p.text_gray.to_color32()),
                ("text_dim", p.text_dim.to_color32()),
                ("text_error", p.text_error.to_color32()),
                ("error", p.error.to_color32()),
                ("warn", p.warn.to_color32()),
                ("debug", p.debug.to_color32()),
                ("success", p.success.to_color32()),
            ];
            for (surface_name, bg) in surfaces {
                for (fg_name, fg) in fg_tokens {
                    let ratio = contrast_ratio(fg, bg);
                    if ratio < 4.5 {
                        failures.push(format!(
                            "{}: {fg_name} {:?} on {surface_name} {:?} = {ratio:.2}:1 (< 4.5:1)",
                            preset.name,
                            (fg.r(), fg.g(), fg.b()),
                            (bg.r(), bg.g(), bg.b()),
                        ));
                    }
                }
            }
            let button_fills = [
                ("primary_accent", p.primary_accent.to_color32()),
                ("action_info", p.action_info.to_color32()),
                ("action_destructive", p.action_destructive.to_color32()),
                ("success_muted", p.success_muted.to_color32()),
            ];
            for (fill_name, fill) in button_fills {
                let ink = contrast_text_on(fill);
                let ratio = contrast_ratio(ink, fill);
                if ratio < 4.5 {
                    failures.push(format!(
                        "{}: contrast ink {:?} on {fill_name} {:?} = {ratio:.2}:1 (< 4.5:1)",
                        preset.name,
                        (ink.r(), ink.g(), ink.b()),
                        (fill.r(), fill.g(), fill.b()),
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "WCAG AA contrast failures:\n{}",
            failures.join("\n")
        );
    }
}
