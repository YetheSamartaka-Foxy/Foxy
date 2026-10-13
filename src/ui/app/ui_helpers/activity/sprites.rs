use egui::emath::lerp;
use egui::{Color32, Mesh, Pos2, Rect, Vec2, pos2};

use super::{additive, bump, noise, particle_life};

/// A square sprite of side `size` centered on `center`.
pub(super) fn add_sprite(mesh: &mut Mesh, center: Pos2, size: f32, tint: Color32) {
    mesh.add_rect_with_uv(
        Rect::from_center_size(center, Vec2::splat(size)),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        tint,
    );
}

/// Motes that drift up through `area`, twinkle and fade, endlessly.
#[allow(clippy::too_many_arguments)]
pub(super) fn add_dust(
    mesh: &mut Mesh,
    area: Rect,
    size: f32,
    time: f64,
    color: Color32,
    intensity: f32,
    count: usize,
    seed: u32,
) {
    for index in 0..count as u32 {
        let (life, drift) = particle_life(index, time, 0.8..=1.6, seed);
        let lane = noise(index.wrapping_mul(131).wrapping_add(drift), seed + 2);
        let twinkle = bump(life);
        add_sprite(
            mesh,
            pos2(
                lerp(area.left()..=area.right(), lane),
                lerp(area.bottom()..=area.top(), life),
            ),
            size * (0.5 + 0.5 * twinkle),
            additive(color, intensity * twinkle),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dust_fills_a_mesh_with_one_quad_per_mote() {
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 20.0));
        let mut mesh = Mesh::default();
        add_dust(&mut mesh, area, 8.0, 3.0, Color32::WHITE, 1.0, 6, 9);
        assert_eq!(mesh.vertices.len(), 6 * 4);
        assert!(mesh.is_valid());
    }
}
