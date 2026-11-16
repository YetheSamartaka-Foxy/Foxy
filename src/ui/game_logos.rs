//! Use official Arma logos unmodified and untinted; other games use a palette-tinted generic glyph. See the logo artwork notice in `src/ui/icons/games/README.md`.

use std::collections::HashMap;

use egui::{Color32, TextureHandle, Vec2};

use crate::core::game::arma3::ARMA3_GAME_ID;
use crate::core::game::reforger::REFORGER_GAME_ID;

/// Reserve at least the Arma manual's 60 px digital minimum; use one slot height across spaces.
pub const GAME_LOGO_HEIGHT: f32 = 60.0;

/// Keep sidebar badges below the Arma logo's 60 px minimum while preserving room for the space name.
pub const GAME_LOGO_BADGE_HEIGHT: f32 = 28.0;
pub const GAME_LOGO_BADGE_MAX_WIDTH: f32 = 72.0;

/// Reserve trademark clear space equal to one fifth of the Arma logo height on each side.
const SAFE_ZONE_RATIO: f32 = 0.2;

const ARMA3_BLACK: &[u8] = include_bytes!("icons/games/arma3_black.png");
const ARMA3_WHITE: &[u8] = include_bytes!("icons/games/arma3_white.png");
const REFORGER_BLACK: &[u8] = include_bytes!("icons/games/reforger_black.png");
const REFORGER_WHITE: &[u8] = include_bytes!("icons/games/reforger_white.png");
const GENERIC_WHITE: &[u8] = include_bytes!("icons/games/generic_white.png");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GameLogo {
    Arma3,
    Reforger,
    Generic,
}

impl GameLogo {
    pub fn for_game_id(game_id: &str) -> Self {
        if game_id == ARMA3_GAME_ID {
            Self::Arma3
        } else if game_id == REFORGER_GAME_ID {
            Self::Reforger
        } else {
            Self::Generic
        }
    }

    /// Show third-party logos untinted with clear space; tint only the generic glyph.
    pub fn is_trademark(self) -> bool {
        !matches!(self, Self::Generic)
    }

    fn texture_key(self, dark_mode: bool) -> String {
        let variant = if dark_mode { "white" } else { "black" };
        format!("game_logo_{}_{}", self.name(), variant)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Arma3 => "arma3",
            Self::Reforger => "reforger",
            Self::Generic => "generic",
        }
    }

    fn bytes(self, dark_mode: bool) -> &'static [u8] {
        match (self, dark_mode) {
            (Self::Arma3, true) => ARMA3_WHITE,
            (Self::Arma3, false) => ARMA3_BLACK,
            (Self::Reforger, true) => REFORGER_WHITE,
            (Self::Reforger, false) => REFORGER_BLACK,
            (Self::Generic, _) => GENERIC_WHITE,
        }
    }
}

/// Logo slot height includes the reserved trademark clear zone.
pub fn slot_height(logo_height: f32) -> f32 {
    logo_height * (1.0 + 2.0 * SAFE_ZONE_RATIO)
}

#[derive(Default)]
pub struct GameLogoTextures {
    textures: HashMap<String, TextureHandle>,
    texture_bytes: usize,
}

impl GameLogoTextures {
    pub fn texture_bytes(&self) -> usize {
        self.texture_bytes
    }

    fn texture(
        &mut self,
        ctx: &egui::Context,
        logo: GameLogo,
        dark_mode: bool,
    ) -> Option<&TextureHandle> {
        let key = logo.texture_key(dark_mode);
        if !self.textures.contains_key(&key) {
            let image = image::load_from_memory(logo.bytes(dark_mode))
                .map(|img| img.to_rgba8())
                .map_err(|err| log::error!("Failed to decode embedded game logo {key}: {err}"))
                .ok()?;
            let (width, height) = image.dimensions();
            let color_image =
                egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &image);
            self.texture_bytes = self.texture_bytes.saturating_add(
                (width as usize)
                    .saturating_mul(height as usize)
                    .saturating_mul(4),
            );
            let texture = ctx.load_texture(&key, color_image, egui::TextureOptions::LINEAR);
            self.textures.insert(key.clone(), texture);
        }
        self.textures.get(&key)
    }

    /// Keep aspect ratio and fit the logo within its slot; trademark logos ignore tint and retain clear space.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        game_id: &str,
        height: f32,
        slot_width: f32,
        tint: Color32,
    ) -> egui::Response {
        self.paint(ui, game_id, height, slot_width, slot_width, tint)
    }

    /// Size the badge to its logo up to `max_width` so adjacent text keeps its space.
    pub fn show_badge(
        &mut self,
        ui: &mut egui::Ui,
        game_id: &str,
        height: f32,
        max_width: f32,
        tint: Color32,
    ) -> egui::Response {
        self.paint(ui, game_id, height, 0.0, max_width, tint)
    }

    fn paint(
        &mut self,
        ui: &mut egui::Ui,
        game_id: &str,
        height: f32,
        min_width: f32,
        max_width: f32,
        tint: Color32,
    ) -> egui::Response {
        let logo = GameLogo::for_game_id(game_id);
        let dark_mode = ui.visuals().dark_mode;
        let ctx = ui.ctx().clone();
        let Some(texture) = self.texture(&ctx, logo, dark_mode) else {
            return ui.label(" ");
        };
        let texture_size = texture.size_vec2();
        let aspect = if texture_size.y > 0.0 {
            texture_size.x / texture_size.y
        } else {
            1.0
        };
        let pad_ratio = if logo.is_trademark() {
            SAFE_ZONE_RATIO
        } else {
            0.0
        };
        let mut logo_height = height;
        let mut logo_width = logo_height * aspect;
        let frame_width = |w: f32, h: f32| w + 2.0 * h * pad_ratio;
        if frame_width(logo_width, logo_height) > max_width && max_width > 0.0 {
            logo_width = max_width / (aspect + 2.0 * pad_ratio) * aspect;
            logo_height = logo_width / aspect;
        }
        let pad = logo_height * pad_ratio;
        // Reserve trademark clear space in every slot so headers and rows keep a consistent height.
        let slot_size = Vec2::new(min_width.max(logo_width + 2.0 * pad), slot_height(height));
        let (rect, response) = ui.allocate_exact_size(slot_size, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let logo_rect =
                egui::Rect::from_center_size(rect.center(), Vec2::new(logo_width, logo_height));
            let mut image = egui::Image::new((texture.id(), logo_rect.size()));
            if !logo.is_trademark() {
                image = image.tint(tint);
            }
            image.paint_at(ui, logo_rect);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_ids_map_to_official_logos_only_for_arma_titles() {
        assert_eq!(GameLogo::for_game_id("arma3"), GameLogo::Arma3);
        assert_eq!(GameLogo::for_game_id("reforger"), GameLogo::Reforger);
        assert_eq!(GameLogo::for_game_id("twwh3"), GameLogo::Generic);
        assert_eq!(GameLogo::for_game_id("generic"), GameLogo::Generic);
        assert_eq!(GameLogo::for_game_id("unknown"), GameLogo::Generic);
    }

    #[test]
    fn only_arma_logos_are_trademarks() {
        assert!(GameLogo::Arma3.is_trademark());
        assert!(GameLogo::Reforger.is_trademark());
        assert!(!GameLogo::Generic.is_trademark());
    }

    #[test]
    fn embedded_logo_artwork_decodes() {
        for logo in [GameLogo::Arma3, GameLogo::Reforger, GameLogo::Generic] {
            for dark_mode in [true, false] {
                let image = image::load_from_memory(logo.bytes(dark_mode))
                    .unwrap_or_else(|err| panic!("{:?} dark={dark_mode}: {err}", logo));
                assert!(image.width() > 0 && image.height() > 0);
            }
        }
    }

    #[test]
    fn texture_keys_differ_per_surface_variant() {
        assert_ne!(
            GameLogo::Arma3.texture_key(true),
            GameLogo::Arma3.texture_key(false)
        );
        assert_ne!(
            GameLogo::Arma3.texture_key(true),
            GameLogo::Reforger.texture_key(true)
        );
    }
}
