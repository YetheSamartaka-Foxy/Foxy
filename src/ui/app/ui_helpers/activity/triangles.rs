use egui::epaint::{Vertex, WHITE_UV};
use egui::{Color32, Mesh, Pos2, Rect, pos2};

use super::{bump, mix, noise};
use crate::ui::palette::ON_STRONG_FILL;

/// Gap between neighboring triangles as a share of a row's height.
const GAP_SHARE: f32 = 0.2;
/// How far lit and brightest triangles move from the fill toward white.
const LIGHT_SHARE: f32 = 0.35;
const BRIGHT_SHARE: f32 = 0.7;
/// Shorter triangles stop reading as triangles, so a short grid drops rows.
const MIN_HEIGHT_PX: f32 = 4.0;
/// Base of an equilateral triangle as a share of its height.
const BASE_PER_HEIGHT: f32 = 1.154_700_5;
/// Share of its own rhythm a blinking triangle spends lit.
const BLINK_SHARE: f32 = 0.12;

/// Rows of triangles side by side, pointing up and down by turns like a
/// tiling, each one its own cell. Rows sit on whole physical pixels so every
/// row and gap is the same size at fractional scales.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TriangleGrid {
    pub(super) columns: usize,
    pub(super) rows: usize,
    left_px: f32,
    top_px: f32,
    height_px: f32,
    base_px: f32,
    step_px: f32,
    row_pitch_px: f32,
    pixels_per_point: f32,
}

impl TriangleGrid {
    /// Up to `max_rows` rows of the tallest triangles that fit `rect`'s height,
    /// as many across as fit its width, centered in it.
    pub(super) fn fit(rect: Rect, max_rows: usize, pixels_per_point: f32) -> Self {
        let pixels_per_point = pixels_per_point.max(f32::EPSILON);
        let height_px = (rect.height() * pixels_per_point).max(1.0);
        let (rows, row_px, gap_px) = rows_that_fit(height_px, max_rows);
        let base_px = row_px * BASE_PER_HEIGHT;
        let step_px = base_px / 2.0 + gap_px;
        let width_px = rect.width() * pixels_per_point;
        let columns = (((width_px - base_px).max(0.0) / step_px).floor() as usize) + 1;
        let used_width = (columns - 1) as f32 * step_px + base_px;
        let used_height = rows as f32 * (row_px + gap_px) - gap_px;
        Self {
            columns,
            rows,
            left_px: (rect.left() * pixels_per_point + ((width_px - used_width) / 2.0).max(0.0))
                .round(),
            top_px: (rect.top() * pixels_per_point + ((height_px - used_height) / 2.0).max(0.0))
                .round(),
            height_px: row_px,
            base_px,
            step_px,
            row_pitch_px: row_px + gap_px,
            pixels_per_point,
        }
    }

    pub(super) fn count(&self) -> usize {
        self.columns * self.rows
    }

    /// Whether triangle (`column`, `row`) points up; its neighbors point down.
    pub(super) fn points_up(column: usize, row: usize) -> bool {
        (column + row).is_multiple_of(2)
    }

    /// The three corners of triangle (`column`, `row`), in points: the two
    /// ends of its base, then its apex.
    pub(super) fn corners(&self, column: usize, row: usize) -> [Pos2; 3] {
        let left = self.left_px + column as f32 * self.step_px;
        let top = self.top_px + row as f32 * self.row_pitch_px;
        let bottom = top + self.height_px;
        let (base_y, apex_y) = if Self::points_up(column, row) {
            (bottom, top)
        } else {
            (top, bottom)
        };
        [
            pos2(left, base_y),
            pos2(left + self.base_px, base_y),
            pos2(left + self.base_px / 2.0, apex_y),
        ]
        .map(|corner| pos2(corner.x, corner.y) / self.pixels_per_point)
    }

    /// Adds triangle (`column`, `row`) in `color`, its edges softened over a
    /// physical pixel so the slanted sides do not stair-step.
    pub(super) fn add(&self, mesh: &mut Mesh, column: usize, row: usize, color: Color32) {
        add_soft_triangle(
            mesh,
            self.corners(column, row),
            1.0 / self.pixels_per_point,
            color,
        );
    }

    /// Distance from one triangle to the next along a row, in points.
    pub(super) fn pitch(&self) -> f32 {
        self.step_px / self.pixels_per_point
    }

    /// Height of one row of triangles, in points.
    pub(super) fn cell_size(&self) -> f32 {
        self.height_px / self.pixels_per_point
    }

    /// The x of a point `columns` triangles in from the grid's left edge.
    pub(super) fn x_at(&self, columns: f32) -> f32 {
        (self.left_px + self.base_px / 2.0 + columns * self.step_px) / self.pixels_per_point
    }

    pub(super) fn reserve(&self, mesh: &mut Mesh) {
        mesh.reserve_vertices(self.count() * 6);
        mesh.reserve_triangles(self.count() * 7);
    }
}

/// Rows, row height and gap, in physical pixels, for a grid `height_px` tall.
fn rows_that_fit(height_px: f32, max_rows: usize) -> (usize, f32, f32) {
    let mut rows = max_rows.max(1);
    loop {
        if rows == 1 {
            let row = height_px.floor().max(1.0);
            return (1, row, (row * GAP_SHARE).round().max(1.0));
        }
        let gap = (height_px / rows as f32 * GAP_SHARE).round().max(1.0);
        let row = ((height_px - (rows - 1) as f32 * gap) / rows as f32).floor();
        if row >= MIN_HEIGHT_PX {
            return (rows, row, gap);
        }
        rows -= 1;
    }
}

/// A triangle whose edges fade out over `feather` points: an opaque core
/// shrunk by half the feather and a clear rim grown by the other half.
fn add_soft_triangle(mesh: &mut Mesh, corners: [Pos2; 3], feather: f32, color: Color32) {
    let center = pos2(
        (corners[0].x + corners[1].x + corners[2].x) / 3.0,
        (corners[0].y + corners[1].y + corners[2].y) / 3.0,
    );
    let inradius = inradius(corners).max(f32::EPSILON);
    let scale = |offset: f32| (inradius + offset).max(0.0) / inradius;
    let (inner, outer) = (scale(-feather / 2.0), scale(feather / 2.0));
    let base = mesh.vertices.len() as u32;
    for (scale, color) in [(inner, color), (outer, Color32::TRANSPARENT)] {
        for corner in corners {
            mesh.vertices.push(Vertex {
                pos: center + (corner - center) * scale,
                uv: WHITE_UV,
                color,
            });
        }
    }
    mesh.add_triangle(base, base + 1, base + 2);
    for side in 0..3 {
        let next = (side + 1) % 3;
        mesh.add_triangle(base + side, base + next, base + 3 + side);
        mesh.add_triangle(base + next, base + 3 + next, base + 3 + side);
    }
}

fn inradius(corners: [Pos2; 3]) -> f32 {
    let [a, b, c] = corners;
    let perimeter = (b - a).length() + (c - b).length() + (a - c).length();
    let area = ((b - a).x * (c - a).y - (b - a).y * (c - a).x).abs() / 2.0;
    2.0 * area / perimeter.max(f32::EPSILON)
}

/// The four tones every triangle is painted in, flat like the rest of the
/// app: empty, filled, lit and brightest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Tones {
    pub(super) empty: Color32,
    pub(super) fill: Color32,
    pub(super) light: Color32,
    pub(super) bright: Color32,
}

impl Tones {
    pub(super) fn new(fill: Color32, empty: Color32) -> Self {
        Self {
            empty,
            fill,
            light: mix(fill, ON_STRONG_FILL, LIGHT_SHARE),
            bright: mix(fill, ON_STRONG_FILL, BRIGHT_SHARE),
        }
    }

    /// A filled triangle carrying `light` from a blink, a bubble or the
    /// frontier glow, from none (0) to full (1).
    pub(super) fn lit(&self, light: f32) -> Color32 {
        if light >= 0.6 {
            self.bright
        } else if light >= 0.25 {
            self.light
        } else {
            self.fill
        }
    }

    /// A triangle `amount` of the way from empty to lit, as cells being read
    /// and the scanning beam show it.
    pub(super) fn rising(&self, amount: f32) -> Color32 {
        if amount >= 0.75 {
            self.light
        } else if amount >= 0.35 {
            self.fill
        } else {
            self.empty
        }
    }
}

/// How brightly triangle `index` blinks at `time`: each one flashes briefly
/// on its own rhythm, so a filled bar glitters all over.
pub(super) fn blink(index: usize, time: f64) -> f32 {
    let period = 1.6 + 2.2 * f64::from(noise(index as u32, 71));
    let phase = (time / period + f64::from(noise(index as u32, 73))).fract() as f32;
    if phase < BLINK_SHARE {
        bump(phase / BLINK_SHARE)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn grid(width: f32, height: f32, rows: usize, pixels_per_point: f32) -> TriangleGrid {
        TriangleGrid::fit(
            Rect::from_min_size(pos2(10.0, 20.0), vec2(width, height)),
            rows,
            pixels_per_point,
        )
    }

    #[test]
    fn a_tall_enough_bar_gets_every_row() {
        assert_eq!(grid(300.0, 34.0, 4, 1.5).rows, 4);
    }

    #[test]
    fn a_short_bar_drops_rows_rather_than_shrinking_triangles_to_specks() {
        assert_eq!(grid(300.0, 10.0, 4, 1.5).rows, 3);
        assert_eq!(grid(300.0, 3.0, 4, 1.5).rows, 1);
        assert_eq!(grid(300.0, 30.0, 1, 1.5).rows, 1);
    }

    #[test]
    fn neighbors_point_opposite_ways() {
        assert!(TriangleGrid::points_up(0, 0));
        assert!(!TriangleGrid::points_up(1, 0));
        assert!(!TriangleGrid::points_up(0, 1));
        let triangles = grid(200.0, 34.0, 4, 1.5);
        let up = triangles.corners(0, 0);
        let down = triangles.corners(1, 0);
        assert!(up[2].y < up[0].y, "the apex of an up triangle is on top");
        assert!(
            down[2].y > down[0].y,
            "the apex of a down triangle is below"
        );
    }

    #[test]
    fn triangles_stay_inside_their_rect_and_keep_apart() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(301.0, 34.0));
        let triangles = TriangleGrid::fit(rect, 4, 1.5);
        for row in 0..triangles.rows {
            for column in 0..triangles.columns {
                for corner in triangles.corners(column, row) {
                    assert!(rect.expand(1e-3).contains(corner));
                }
            }
        }
        let up = triangles.corners(0, 0);
        let down = triangles.corners(1, 0);
        assert!(
            down[0].x > up[2].x,
            "the slanted sides keep a gap between them"
        );
    }

    #[test]
    fn a_soft_triangle_is_a_core_and_a_clear_rim() {
        let mut mesh = Mesh::default();
        let corners = [pos2(0.0, 10.0), pos2(11.547, 10.0), pos2(5.7735, 0.0)];
        add_soft_triangle(&mut mesh, corners, 0.5, Color32::RED);
        assert!(mesh.is_valid());
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.indices.len(), 7 * 3);
        assert!(mesh.vertices[..3].iter().all(|v| v.color == Color32::RED));
        assert!(
            mesh.vertices[3..]
                .iter()
                .all(|v| v.color == Color32::TRANSPARENT)
        );
        assert!((inradius(corners) - 10.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn every_triangle_takes_one_of_four_tones_none_darker_than_the_fill() {
        let fill = Color32::from_rgb(200, 100, 20);
        let tones = Tones::new(fill, Color32::from_gray(40));
        let palette = [tones.empty, tones.fill, tones.light, tones.bright];
        for step in 0..=100 {
            let amount = step as f32 / 100.0;
            assert!(palette.contains(&tones.lit(amount)));
            assert!(palette.contains(&tones.rising(amount)));
            assert!(tones.lit(amount).g() >= fill.g());
        }
        assert_eq!(tones.lit(0.0), fill);
        assert_eq!(tones.lit(1.0), tones.bright);
        assert_eq!(tones.rising(0.0), tones.empty);
        assert_eq!(tones.rising(1.0), tones.light);
        assert!(tones.bright.g() > tones.light.g() && tones.light.g() > fill.g());
    }

    #[test]
    fn each_triangle_blinks_briefly_and_on_its_own_rhythm() {
        let frames = |index: usize| -> Vec<bool> {
            (0..400)
                .map(|step| blink(index, f64::from(step) * 0.05) > 0.0)
                .collect()
        };
        for index in 0..20 {
            let lit = frames(index).iter().filter(|lit| **lit).count();
            assert!(lit > 0 && lit < 200, "triangle {index} lit {lit}/400");
        }
        assert_ne!(frames(1), frames(2));
    }
}
