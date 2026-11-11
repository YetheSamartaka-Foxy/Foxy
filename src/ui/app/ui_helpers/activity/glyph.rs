use std::f32::consts::{FRAC_PI_2, TAU};

use egui::emath::lerp;
use egui::{
    Color32, Mesh, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui, Vec2, Widget,
    WidgetInfo, pos2, vec2,
};

use super::{Motion, arc_points, bump, ease_in_out, fade, mix, point_at};
use crate::ui::palette::ON_STRONG_FILL;

const RADAR_TURN_SECONDS: f64 = 1.8;
const SHIELD_SCAN_SECONDS: f64 = 1.6;
const SIGNAL_SECONDS: f64 = 1.4;
const GEAR_TURN_SECONDS: f64 = 4.0;
const DROP_SECONDS: f64 = 1.2;
/// Blips the radar beam lights as it passes, as (share of radius, bearing).
const RADAR_BLIPS: [(f32, f32); 3] = [(0.62, 0.6), (0.38, 2.6), (0.8, 4.4)];
const GEAR_TEETH: usize = 8;

/// The kind of work an [`ActivityIcon`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityGlyph {
    /// Local files being read and hashed: a radar sweep.
    Radar,
    /// Every file re-read and verified: a shield under a scan line.
    Shield,
    /// Remote data being fetched: broadcast waves.
    Signal,
    /// Database work: a turning gear.
    Gear,
    /// Files arriving: an arrow dropping into a tray.
    Download,
}

/// An animated icon that names the work in progress, where a spinner would
/// only say that something is happening.
#[must_use = "You should put this widget in a ui with `ui.add(widget);`"]
pub(crate) struct ActivityIcon {
    glyph: ActivityGlyph,
    size: Option<f32>,
    color: Option<Color32>,
}

impl ActivityIcon {
    pub(crate) fn new(glyph: ActivityGlyph) -> Self {
        Self {
            glyph,
            size: None,
            color: None,
        }
    }

    pub(crate) fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    pub(crate) fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    pub(crate) fn paint_at(&self, ui: &Ui, rect: Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let motion = Motion::of(ui);
        let color = self
            .color
            .unwrap_or_else(|| ui.visuals().strong_text_color());
        let square =
            Rect::from_center_size(rect.center(), Vec2::splat(rect.width().min(rect.height())));
        let glyph = Glyph {
            painter: ui.painter_at(rect.expand(2.0)),
            square,
            line: (square.width() / 12.0).clamp(1.2, 2.2),
            color,
            motion,
        };
        match self.glyph {
            ActivityGlyph::Radar => glyph.radar(),
            ActivityGlyph::Shield => glyph.shield(),
            ActivityGlyph::Signal => glyph.signal(),
            ActivityGlyph::Gear => glyph.gear(),
            ActivityGlyph::Download => glyph.download(),
        }
        motion.keep_alive(ui, rect);
    }
}

impl Widget for ActivityIcon {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = self
            .size
            .unwrap_or_else(|| ui.style().spacing.interact_size.y);
        let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
        response.widget_info(|| WidgetInfo::new(Role::ProgressIndicator));
        self.paint_at(ui, rect);
        response
    }
}

struct Glyph {
    painter: Painter,
    square: Rect,
    line: f32,
    color: Color32,
    motion: Motion,
}

impl Glyph {
    /// A point at `(x, y)` shares of the icon square.
    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.square.min + self.square.size() * vec2(x, y)
    }

    fn radar(&self) {
        let center = self.square.center();
        let radius = self.square.width() / 2.0 - 1.0;
        self.painter.circle_stroke(
            center,
            radius,
            Stroke::new(self.line, fade(self.color, 0.6)),
        );
        self.painter.circle_stroke(
            center,
            radius * 0.5,
            Stroke::new(self.line * 0.75, fade(self.color, 0.3)),
        );
        let beam = if self.motion.animates() {
            TAU * self.motion.cycle(RADAR_TURN_SECONDS) - FRAC_PI_2
        } else {
            -FRAC_PI_2 * 0.5
        };

        const SLICES: usize = 12;
        let trail = TAU * 0.22;
        let mut mesh = Mesh::default();
        for slice in 0..SLICES {
            let from = beam - trail * (1.0 - slice as f32 / SLICES as f32);
            let to = beam - trail * (1.0 - (slice + 1) as f32 / SLICES as f32);
            let tint = fade(
                self.color,
                0.42 * ((slice + 1) as f32 / SLICES as f32).powi(2),
            );
            let base = mesh.vertices.len() as u32;
            mesh.colored_vertex(center, tint);
            mesh.colored_vertex(point_at(center, radius, from), tint);
            mesh.colored_vertex(point_at(center, radius, to), tint);
            mesh.add_triangle(base, base + 1, base + 2);
        }
        self.painter.add(Shape::mesh(mesh));
        self.painter.line_segment(
            [center, point_at(center, radius, beam)],
            Stroke::new(self.line, self.color),
        );

        for (distance, bearing) in RADAR_BLIPS {
            let since_beam = (beam - bearing).rem_euclid(TAU) / TAU;
            let glow = if self.motion.animates() {
                (1.0 - since_beam * 2.2).max(0.0)
            } else {
                0.5
            };
            self.painter.circle_filled(
                point_at(center, radius * distance, bearing),
                self.line * 0.9,
                fade(self.color, 0.2 + 0.8 * glow),
            );
        }
    }

    fn shield(&self) {
        let outline = vec![
            self.at(0.5, 0.06),
            self.at(0.88, 0.2),
            self.at(0.86, 0.52),
            self.at(0.5, 0.94),
            self.at(0.14, 0.52),
            self.at(0.12, 0.2),
        ];
        self.painter.add(Shape::convex_polygon(
            outline.clone(),
            fade(self.color, 0.16),
            Stroke::new(self.line, self.color),
        ));
        let travel = if self.motion.animates() {
            let cycle = self.motion.cycle(SHIELD_SCAN_SECONDS);
            ease_in_out(1.0 - (2.0 * cycle - 1.0).abs())
        } else {
            0.45
        };
        let y = lerp(self.at(0.0, 0.18).y..=self.at(0.0, 0.8).y, travel);
        if let Some((left, right)) = span_at(&outline, y) {
            let inset = self.line * 1.5;
            let scan = [pos2(left + inset, y), pos2(right - inset, y)];
            self.painter
                .line_segment(scan, Stroke::new(self.line * 3.0, fade(self.color, 0.25)));
            self.painter.line_segment(
                scan,
                Stroke::new(self.line, mix(self.color, ON_STRONG_FILL, 0.4)),
            );
        }
    }

    fn signal(&self) {
        let size = self.square.width();
        let origin = self.at(0.5, 0.78);
        self.painter
            .circle_filled(origin, self.line * 1.2, self.color);
        for wave in 0..3 {
            let delay = wave as f32 * 0.2;
            let strength = if self.motion.animates() {
                bump((self.motion.cycle(SIGNAL_SECONDS) - delay).rem_euclid(1.0))
            } else {
                1.0 - delay
            };
            self.painter.add(Shape::line(
                arc_points(
                    origin,
                    size * (0.22 + 0.2 * wave as f32),
                    -FRAC_PI_2 - 0.75,
                    1.5,
                ),
                Stroke::new(self.line, fade(self.color, 0.25 + 0.75 * strength)),
            ));
        }
    }

    fn gear(&self) {
        let center = self.square.center();
        let size = self.square.width();
        let turn = TAU * self.motion.cycle(GEAR_TURN_SECONDS);
        let outline = gear_outline(center, size * 0.46, size * 0.34, turn);
        let mut body = Mesh::default();
        let tint = fade(self.color, 0.16);
        body.colored_vertex(center, tint);
        for point in &outline {
            body.colored_vertex(*point, tint);
        }
        let rim = outline.len() as u32;
        for index in 0..rim {
            body.add_triangle(0, 1 + index, 1 + (index + 1) % rim);
        }
        self.painter.add(Shape::mesh(body));
        self.painter.add(Shape::closed_line(
            outline,
            Stroke::new(self.line, self.color),
        ));
        self.painter
            .circle_stroke(center, size * 0.13, Stroke::new(self.line, self.color));
    }

    fn download(&self) {
        let size = self.square.width();
        self.painter.add(Shape::line(
            vec![
                self.at(0.16, 0.6),
                self.at(0.16, 0.86),
                self.at(0.84, 0.86),
                self.at(0.84, 0.6),
            ],
            Stroke::new(self.line, self.color),
        ));
        let (drop, opacity) = if self.motion.animates() {
            let cycle = self.motion.cycle(DROP_SECONDS);
            (ease_in_out(cycle), (bump(cycle) * 1.6).min(1.0))
        } else {
            (0.5, 1.0)
        };
        let offset = vec2(0.0, lerp(-0.22..=0.12, drop) * size);
        let arrow = Stroke::new(self.line, fade(self.color, opacity));
        let above_tray = self.painter.with_clip_rect(Rect::from_min_max(
            self.square.min - vec2(2.0, 2.0),
            pos2(self.square.max.x + 2.0, self.at(0.0, 0.8).y),
        ));
        let tip = self.at(0.5, 0.62) + offset;
        above_tray.line_segment([self.at(0.5, 0.12) + offset, tip], arrow);
        let wing = size * 0.17;
        above_tray.add(Shape::line(
            vec![tip + vec2(-wing, -wing), tip, tip + vec2(wing, -wing)],
            arrow,
        ));
    }
}

/// Where a horizontal line at `y` crosses a convex outline.
fn span_at(outline: &[Pos2], y: f32) -> Option<(f32, f32)> {
    let mut left = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    for (index, a) in outline.iter().enumerate() {
        let b = outline[(index + 1) % outline.len()];
        if (a.y - y) * (b.y - y) <= 0.0 && (a.y - b.y).abs() > f32::EPSILON {
            let x = a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x);
            left = left.min(x);
            right = right.max(x);
        }
    }
    (left <= right).then_some((left, right))
}

/// Outline of a gear, star-shaped around `center`, turned by `turn` radians.
fn gear_outline(center: Pos2, outer: f32, inner: f32, turn: f32) -> Vec<Pos2> {
    let pitch = TAU / GEAR_TEETH as f32;
    let mut points = Vec::with_capacity(GEAR_TEETH * 5);
    for tooth in 0..GEAR_TEETH {
        let mid = turn + pitch * tooth as f32;
        points.push(point_at(center, inner, mid - pitch * 0.5));
        points.push(point_at(center, inner, mid - pitch * 0.26));
        points.push(point_at(center, outer, mid - pitch * 0.17));
        points.push(point_at(center, outer, mid + pitch * 0.17));
        points.push(point_at(center, inner, mid + pitch * 0.26));
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_line_spans_the_shield_where_it_crosses() {
        let outline = [
            pos2(0.0, 0.0),
            pos2(10.0, 0.0),
            pos2(10.0, 10.0),
            pos2(0.0, 10.0),
        ];
        assert_eq!(span_at(&outline, 5.0), Some((0.0, 10.0)));
        assert_eq!(span_at(&outline, 11.0), None);
        let diamond = [
            pos2(5.0, 0.0),
            pos2(10.0, 5.0),
            pos2(5.0, 10.0),
            pos2(0.0, 5.0),
        ];
        assert_eq!(span_at(&diamond, 2.5), Some((2.5, 7.5)));
    }

    #[test]
    fn gear_teeth_reach_the_outer_radius_and_valleys_the_inner() {
        let center = pos2(0.0, 0.0);
        let outline = gear_outline(center, 10.0, 7.0, 0.3);
        assert_eq!(outline.len(), GEAR_TEETH * 5);
        let distances: Vec<f32> = outline.iter().map(|p| (*p - center).length()).collect();
        assert!(
            distances
                .iter()
                .all(|d| (*d - 10.0).abs() < 1e-4 || (*d - 7.0).abs() < 1e-4)
        );
        assert_eq!(
            distances
                .iter()
                .filter(|d| (**d - 10.0).abs() < 1e-4)
                .count(),
            GEAR_TEETH * 2
        );
    }
}
