use egui::{Color32, ColorImage, Context, Id, TextureHandle, TextureId, TextureOptions};

const SPRITE_TEXELS: usize = 32;

/// The sprites the activity visuals tint and scatter.
#[derive(Clone)]
pub(super) struct ActivityTextures {
    glow: TextureHandle,
    sparkle: TextureHandle,
}

impl ActivityTextures {
    pub(super) fn of(ctx: &Context) -> Self {
        let id = Id::unique("foxy_activity_textures");
        if let Some(textures) = ctx.data(|data| data.get_temp::<Self>(id)) {
            return textures;
        }
        let load = |name: &str, alpha: fn(f32, f32) -> f32| {
            ctx.load_texture(
                name,
                ColorImage::new([SPRITE_TEXELS, SPRITE_TEXELS], sprite_image(alpha)),
                TextureOptions::LINEAR,
            )
        };
        let textures = Self {
            glow: load("activity_glow", glow_alpha),
            sparkle: load("activity_sparkle", sparkle_alpha),
        };
        ctx.data_mut(|data| data.insert_temp(id, textures.clone()));
        textures
    }

    /// A soft round light.
    pub(super) fn glow(&self) -> TextureId {
        self.glow.id()
    }

    /// A four-pointed twinkle.
    pub(super) fn sparkle(&self) -> TextureId {
        self.sparkle.id()
    }
}

fn white(alpha: f32) -> Color32 {
    Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn sprite_image(alpha: fn(f32, f32) -> f32) -> Vec<Color32> {
    let mut pixels = Vec::with_capacity(SPRITE_TEXELS * SPRITE_TEXELS);
    for y in 0..SPRITE_TEXELS {
        for x in 0..SPRITE_TEXELS {
            let dx = (x as f32 + 0.5) / SPRITE_TEXELS as f32 * 2.0 - 1.0;
            let dy = (y as f32 + 0.5) / SPRITE_TEXELS as f32 * 2.0 - 1.0;
            pixels.push(white(alpha(dx, dy)));
        }
    }
    pixels
}

fn glow_alpha(dx: f32, dy: f32) -> f32 {
    (1.0 - (dx * dx + dy * dy).sqrt()).max(0.0).powf(2.2)
}

fn sparkle_alpha(dx: f32, dy: f32) -> f32 {
    let arm = |along: f32, across: f32| {
        (-(across / 0.07).powi(2)).exp() * (1.0 - along.abs()).max(0.0).powi(3)
    };
    let core = (-((dx * dx + dy * dy).sqrt() / 0.22).powi(2)).exp();
    arm(dx, dy) + arm(dy, dx) + 0.9 * core
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprites_fade_out_before_their_edges() {
        for alpha in [glow_alpha as fn(f32, f32) -> f32, sparkle_alpha] {
            let pixels = sprite_image(alpha);
            for corner in [0, SPRITE_TEXELS - 1] {
                assert!(pixels[corner].a() < 8);
                assert!(pixels[corner * SPRITE_TEXELS].a() < 8);
            }
            assert!(pixels.iter().any(|c| c.a() > 200));
        }
    }
}
