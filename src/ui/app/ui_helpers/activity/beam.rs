use std::f32::consts::{FRAC_PI_2, PI};

use egui::{Color32, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, Ui, pos2, vec2};

use super::sprites::add_sprite;
use super::textures::ActivityTextures;
use super::{Motion, additive, bump, ease_out, mix, noise};
use crate::ui::palette::ON_STRONG_FILL;

const BEAM_SECONDS: f64 = 3.6;
const BEAM_SEGMENTS: usize = 28;
/// Share of the frame's border the light's tail covers.
const BEAM_TAIL: f32 = 0.2;
const FLARE_SPARKS: u32 = 14;

/// A single light that runs around a rounded frame while its work
/// runs. Under reduced motion the frame keeps its own still border.
pub(crate) fn paint_border_beam(ui: &Ui, rect: Rect, radius: f32, color: Color32) {
    let motion = Motion::of(ui);
    if !motion.animates() || !ui.is_rect_visible(rect) {
        return;
    }
    let outline = Outline::new(rect, radius);
    let (length, tail) = (outline.length(), BEAM_TAIL * outline.length());
    let light = mix(color, ON_STRONG_FILL, 0.45);
    let feather = 1.0 / ui.ctx().pixels_per_point();
    let painter = ui.painter();
    let head = motion.cycle(BEAM_SECONDS) * length;
    let points: Vec<Pos2> = (0..=BEAM_SEGMENTS)
        .map(|step| outline.point(head - tail * (1.0 - step as f32 / BEAM_SEGMENTS as f32)))
        .collect();
    let strength = |step: usize| (step as f32 / BEAM_SEGMENTS as f32).powf(1.6);
    painter.add(Shape::mesh(ribbon(&points, 4.0, feather, |step| {
        additive(light, 0.22 * strength(step))
    })));
    painter.add(Shape::mesh(ribbon(&points, 1.4, feather, |step| {
        additive(light, 0.9 * strength(step))
    })));
    let mut glow = Mesh::with_texture(ActivityTextures::of(ui.ctx()).glow());
    add_sprite(&mut glow, outline.point(head), 12.0, additive(light, 0.7));
    painter.add(Shape::mesh(glow));
    motion.keep_alive(ui, rect);
}

/// A band `width` wide along `points`, colored per point, its long edges
/// softened over `feather`. One mesh, so a light drawn with it never doubles
/// up where pieces would overlap.
fn ribbon(points: &[Pos2], width: f32, feather: f32, color: impl Fn(usize) -> Color32) -> Mesh {
    let mut mesh = Mesh::default();
    if points.len() < 2 {
        return mesh;
    }
    let half = width / 2.0;
    for (index, point) in points.iter().enumerate() {
        let before = points[index.saturating_sub(1)];
        let after = points[(index + 1).min(points.len() - 1)];
        let along = (after - before).normalized();
        let normal = vec2(-along.y, along.x);
        let tint = color(index);
        for (offset, tint) in [
            (-half - feather, Color32::TRANSPARENT),
            (-half, tint),
            (half, tint),
            (half + feather, Color32::TRANSPARENT),
        ] {
            mesh.colored_vertex(*point + normal * offset, tint);
        }
    }
    for index in 0..(points.len() - 1) as u32 {
        let (this, next) = (index * 4, index * 4 + 4);
        for column in 0..3 {
            mesh.add_triangle(this + column, this + column + 1, next + column);
            mesh.add_triangle(this + column + 1, next + column + 1, next + column);
        }
    }
    mesh
}

/// A one-shot flare as a frame's work completes, `t` of the way through: the
/// border lights up and fades while sparks pop along it.
pub(crate) fn paint_border_flare(ui: &Ui, rect: Rect, radius: f32, color: Color32, t: f32) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let t = t.clamp(0.0, 1.0);
    let outline = Outline::new(rect, radius);
    let light = mix(color, ON_STRONG_FILL, 0.5);
    let painter = ui.painter();
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(3.0, additive(light, 0.6 * (1.0 - ease_out(t)))),
        StrokeKind::Middle,
    );
    let mut sparks = Mesh::with_texture(ActivityTextures::of(ui.ctx()).sparkle());
    for spark in 0..FLARE_SPARKS {
        let local = (t - noise(spark, 71) * 0.5) / 0.45;
        if !(0.0..1.0).contains(&local) {
            continue;
        }
        let twinkle = bump(local);
        add_sprite(
            &mut sparks,
            outline.point(noise(spark, 73) * outline.length()),
            2.0 + 14.0 * twinkle,
            additive(light, 0.9 * twinkle),
        );
    }
    painter.add(Shape::mesh(sparks));
    Motion::of(ui).keep_alive(ui, rect);
}

/// The border of a rounded rectangle, walked clockwise from the start of its
/// top edge.
#[derive(Clone, Copy, Debug)]
struct Outline {
    rect: Rect,
    radius: f32,
    across: f32,
    down: f32,
    arc: f32,
}

impl Outline {
    fn new(rect: Rect, radius: f32) -> Self {
        let radius = radius.clamp(0.0, rect.width().min(rect.height()) / 2.0);
        Self {
            rect,
            radius,
            across: rect.width() - 2.0 * radius,
            down: rect.height() - 2.0 * radius,
            arc: FRAC_PI_2 * radius,
        }
    }

    fn length(&self) -> f32 {
        2.0 * (self.across + self.down) + 4.0 * self.arc
    }

    /// The point `distance` along the border, wrapping around.
    fn point(&self, distance: f32) -> Pos2 {
        let Self {
            rect,
            radius,
            across,
            down,
            arc,
        } = *self;
        let corner = |center: Pos2, start: f32, along: f32| {
            let angle = start + if radius > 0.0 { along / radius } else { 0.0 };
            center + radius * vec2(angle.cos(), angle.sin())
        };
        let mut rest = distance.rem_euclid(self.length().max(f32::EPSILON));
        if rest <= across {
            return pos2(rect.left() + radius + rest, rect.top());
        }
        rest -= across;
        if rest <= arc {
            let center = pos2(rect.right() - radius, rect.top() + radius);
            return corner(center, -FRAC_PI_2, rest);
        }
        rest -= arc;
        if rest <= down {
            return pos2(rect.right(), rect.top() + radius + rest);
        }
        rest -= down;
        if rest <= arc {
            let center = pos2(rect.right() - radius, rect.bottom() - radius);
            return corner(center, 0.0, rest);
        }
        rest -= arc;
        if rest <= across {
            return pos2(rect.right() - radius - rest, rect.bottom());
        }
        rest -= across;
        if rest <= arc {
            let center = pos2(rect.left() + radius, rect.bottom() - radius);
            return corner(center, FRAC_PI_2, rest);
        }
        rest -= arc;
        if rest <= down {
            return pos2(rect.left(), rect.bottom() - radius - rest);
        }
        rest -= down;
        let center = pos2(rect.left() + radius, rect.top() + radius);
        corner(center, PI, rest.min(arc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::PROGRESS_FRAME_INTERVAL;
    use std::time::Duration;

    #[test]
    fn the_outline_is_as_long_as_the_rounded_border() {
        let outline = Outline::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0)), 10.0);
        let expected = 2.0 * (80.0 + 30.0) + 4.0 * FRAC_PI_2 * 10.0;
        assert!((outline.length() - expected).abs() < 1e-3);
    }

    #[test]
    fn every_outline_point_lies_on_the_border() {
        let rect = Rect::from_min_size(pos2(5.0, 5.0), vec2(100.0, 50.0));
        let outline = Outline::new(rect, 10.0);
        let centers = [
            pos2(15.0, 15.0),
            pos2(95.0, 15.0),
            pos2(95.0, 45.0),
            pos2(15.0, 45.0),
        ];
        for step in 0..400 {
            let point = outline.point(outline.length() * step as f32 / 400.0);
            let on_edge = [rect.top(), rect.bottom()]
                .iter()
                .any(|y| (point.y - y).abs() < 1e-3)
                || [rect.left(), rect.right()]
                    .iter()
                    .any(|x| (point.x - x).abs() < 1e-3);
            let on_corner = centers
                .iter()
                .any(|center| ((point - *center).length() - 10.0).abs() < 1e-3);
            assert!(on_edge || on_corner, "{point:?} is off the border");
            assert!(rect.expand(1e-3).contains(point));
        }
        assert_eq!(outline.point(0.0), pos2(15.0, 5.0));
        assert!((outline.point(0.0) - outline.point(outline.length())).length() < 1e-3);
    }

    #[test]
    fn a_square_cornered_outline_walks_its_edges() {
        let outline = Outline::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)), 0.0);
        assert_eq!(outline.length(), 40.0);
        assert_eq!(outline.point(15.0), pos2(10.0, 5.0));
        assert_eq!(outline.point(35.0), pos2(0.0, 5.0));
    }

    #[test]
    fn a_ribbon_follows_its_points_at_its_width() {
        let points = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(20.0, 0.0)];
        let mesh = ribbon(&points, 4.0, 1.0, |_| Color32::WHITE);
        assert!(mesh.is_valid());
        assert_eq!(mesh.vertices.len(), points.len() * 4);
        assert_eq!(mesh.indices.len(), (points.len() - 1) * 18);
        let reach = mesh
            .vertices
            .iter()
            .map(|vertex| vertex.pos.y.abs())
            .fold(0.0_f32, f32::max);
        assert!((reach - 3.0).abs() < 1e-4);
        assert!(ribbon(&points[..1], 4.0, 1.0, |_| Color32::WHITE).is_empty());
    }

    #[test]
    fn the_beam_runs_only_while_motion_is_on() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 60.0));
        let beam = |ui: &mut Ui| paint_border_beam(ui, rect, 10.0, Color32::WHITE);
        assert_eq!(
            super::super::settled_repaint_delay(0.1, beam),
            PROGRESS_FRAME_INTERVAL
        );
        assert!(super::super::settled_repaint_delay(0.0, beam) > Duration::from_secs(60));
    }
}
