use egui::{Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke, Ui, Vec2, pos2};

use super::{Motion, ease_out, fade};

const PING_SECONDS: f64 = 1.6;
const AREA_TOP_OPACITY: f32 = 0.3;
const AREA_BOTTOM_OPACITY: f32 = 0.02;

/// Fills under a chart line in the line's color, fading out towards `baseline`.
pub(crate) fn paint_area_under(painter: &Painter, line: &[Pos2], baseline: f32, color: Color32) {
    if line.len() < 2 {
        return;
    }
    painter.add(Shape::mesh(area_mesh(
        line,
        baseline,
        fade(color, AREA_TOP_OPACITY),
        fade(color, AREA_BOTTOM_OPACITY),
    )));
}

/// Marks the newest sample of a live chart: a dot that sends out a fading ring.
pub(crate) fn paint_live_marker(ui: &Ui, painter: &Painter, center: Pos2, color: Color32) {
    let motion = Motion::of(ui);
    painter.circle_filled(center, 3.0, color);
    if !motion.animates() {
        return;
    }
    let ping = motion.cycle(PING_SECONDS);
    painter.circle_stroke(
        center,
        3.0 + 7.0 * ease_out(ping),
        Stroke::new(1.5, fade(color, 0.7 * (1.0 - ping))),
    );
    motion.keep_alive(ui, Rect::from_center_size(center, Vec2::splat(24.0)));
}

/// A strip of quads from each line point down to `baseline`.
fn area_mesh(line: &[Pos2], baseline: f32, top: Color32, bottom: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(line.len() * 2);
    mesh.reserve_triangles(line.len().saturating_sub(1) * 2);
    for point in line {
        mesh.colored_vertex(*point, top);
        mesh.colored_vertex(pos2(point.x, baseline), bottom);
    }
    for segment in 0..line.len().saturating_sub(1) as u32 {
        let left = segment * 2;
        mesh.add_triangle(left, left + 1, left + 2);
        mesh.add_triangle(left + 1, left + 3, left + 2);
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_area_spans_from_the_line_down_to_the_baseline() {
        let line = [pos2(0.0, 10.0), pos2(5.0, 4.0), pos2(10.0, 8.0)];
        let mesh = area_mesh(&line, 20.0, Color32::WHITE, Color32::BLACK);
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.indices.len(), 12);
        assert!(mesh.is_valid());
        let lowest = mesh
            .vertices
            .iter()
            .map(|vertex| vertex.pos.y)
            .fold(f32::MIN, f32::max);
        assert_eq!(lowest, 20.0);
        assert!(
            area_mesh(&line[..1], 20.0, Color32::WHITE, Color32::BLACK)
                .indices
                .is_empty()
        );
    }
}
