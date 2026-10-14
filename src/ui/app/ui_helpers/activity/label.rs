use std::sync::Arc;

use egui::{Color32, Galley, Painter, Pos2, Rect, Vec2, vec2};

use super::fade;
use crate::ui::palette::{BAR_LABEL_SHADE, ON_STRONG_FILL};

/// Layers of the soft backing under a label, widest first: each dims the
/// triangles a little more toward the text.
const BACKING_LAYERS: [(f32, f32); 3] = [(10.0, 3.0), (6.0, 2.0), (3.0, 1.0)];
const BACKING_OPACITY: f32 = 0.18;
const OUTLINE_OPACITY: f32 = 0.85;

/// Text over a progress bar, readable over lit triangles: a feathered backing
/// dims the triangles right under the text, and an outline rings each glyph, both
/// in the shade that contrasts with the text.
pub(crate) fn paint_bar_label(painter: &Painter, galley: Arc<Galley>, pos: Pos2, color: Color32) {
    let shade = contrast_shade(color);
    let text = Rect::from_min_size(pos, galley.size());
    for (x, y) in BACKING_LAYERS {
        painter.rect_filled(
            text.expand2(vec2(x, y)),
            y + 2.0,
            fade(shade, BACKING_OPACITY),
        );
    }
    let outline = fade(shade, OUTLINE_OPACITY);
    for offset in outline_offsets() {
        painter.galley_with_override_text_color(pos + offset, galley.clone(), outline);
    }
    painter.galley(pos, galley, color);
}

/// Dark behind light text, light behind dark text.
fn contrast_shade(text: Color32) -> Color32 {
    let luma =
        0.299 * f32::from(text.r()) + 0.587 * f32::from(text.g()) + 0.114 * f32::from(text.b());
    if luma >= 128.0 {
        BAR_LABEL_SHADE
    } else {
        ON_STRONG_FILL
    }
}

/// One point out in each of the eight directions.
fn outline_offsets() -> impl Iterator<Item = Vec2> {
    (0..8).map(|step| {
        let angle = step as f32 * std::f32::consts::FRAC_PI_4;
        vec2(angle.cos().round(), angle.sin().round())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outline_rings_the_glyphs_on_every_side() {
        let offsets: Vec<_> = outline_offsets().collect();
        assert_eq!(offsets.len(), 8);
        for (x, y) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (1.0, 1.0)] {
            assert!(offsets.contains(&vec2(x, y)), "missing ({x}, {y})");
        }
        assert!(!offsets.contains(&vec2(0.0, 0.0)));
    }

    #[test]
    fn light_text_gets_a_dark_shade_and_dark_text_a_light_one() {
        assert_eq!(contrast_shade(Color32::WHITE), BAR_LABEL_SHADE);
        assert_eq!(contrast_shade(Color32::from_gray(30)), ON_STRONG_FILL);
    }
}
