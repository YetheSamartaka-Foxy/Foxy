use std::f32::consts::TAU;

use egui::{Color32, Mesh, Pos2, Rect, Shape, Ui, pos2, vec2};

use super::triangles::{Tones, TriangleGrid};
use super::{BarMotion, Motion, bump, fade, noise, particle_life};

const WAVE_SECONDS: f64 = 1.1;
const SLOSH_SECONDS: f64 = 1.6;
const ORB_BUBBLES: u32 = 2;
/// Opacity of the orb's empty triangles, over whatever row it sits in.
const EMPTY_OPACITY: f32 = 0.25;
/// The mana's surface is lit, so the level reads at a glance.
const SURFACE_LIGHT: f32 = 0.4;
/// Mana level while the amount of work is unknown: it sloshes around here.
const UNKNOWN_LEVEL: f32 = 0.55;
/// Rows of triangles in an orb, at most.
const ORB_ROWS: usize = 6;
/// Triangles whose center lies within this share of the half-side make the orb.
const ROUNDNESS: f32 = 0.95;

/// A small round cluster of triangles that fills with mana, for places too
/// tight for a bar such as the repository sidebar.
pub(crate) struct ProgressOrb {
    progress: Option<f32>,
    color: Color32,
    motion: BarMotion,
}

impl ProgressOrb {
    /// `None` progress sloshes the mana around a half-full orb: work with no
    /// known end.
    pub(crate) fn new(progress: Option<f32>, color: Color32) -> Self {
        Self {
            progress,
            color,
            motion: BarMotion::Forward,
        }
    }

    /// Only `Forward` work ripples, sloshes and bubbles.
    pub(crate) fn motion(mut self, motion: BarMotion) -> Self {
        self.motion = motion;
        self
    }

    pub(crate) fn paint_at(&self, ui: &Ui, rect: Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let side = rect.width().min(rect.height());
        if side < 6.0 {
            return;
        }
        let square = Rect::from_center_size(rect.center(), vec2(side, side));
        let grid = TriangleGrid::fit(square, ORB_ROWS, ui.ctx().pixels_per_point());
        let (center, radius) = (square.center(), side / 2.0 * ROUNDNESS);
        let rows = grid.rows as f32;
        let columns = grid.columns as f32;

        let motion = Motion::of(ui);
        let moving = self.motion == BarMotion::Forward;
        let level = self.progress.unwrap_or(UNKNOWN_LEVEL).clamp(0.0, 1.0);
        let wave_phase = TAU * motion.cycle(WAVE_SECONDS);
        let slosh = if moving && self.progress.is_none() {
            (TAU * motion.cycle(SLOSH_SECONDS)).sin()
        } else {
            0.0
        };
        let surface = |column: usize| {
            let wave = if moving && level > 0.0 && level < 1.0 {
                0.45 * (wave_phase + column as f32 * 0.6).sin()
            } else {
                0.0
            };
            let tilt = 1.4 * slosh * (column as f32 + 0.5 - columns / 2.0) / columns;
            level * rows + wave + tilt
        };
        let tones = Tones::new(self.color, fade(self.color, EMPTY_OPACITY));
        let bubbles: Vec<(usize, usize, f32)> = if moving && motion.animates() && level > 0.15 {
            (0..ORB_BUBBLES)
                .map(|bubble| {
                    let (life, cycle) = particle_life(bubble, motion.time(), 0.9..=1.5, 61);
                    let pick = noise(bubble.wrapping_mul(17).wrapping_add(cycle), 63);
                    let column = ((pick * columns) as usize).min(grid.columns - 1);
                    let height = surface(column).max(1.0);
                    let from_bottom = ((life * height) as usize).min(grid.rows - 1);
                    (column, grid.rows - 1 - from_bottom, bump(life))
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut mesh = Mesh::default();
        grid.reserve(&mut mesh);
        for row in 0..grid.rows {
            for column in 0..grid.columns {
                if !in_orb(grid.corners(column, row), center, radius) {
                    continue;
                }
                let filled_to = surface(column);
                let from_bottom = (grid.rows - 1 - row) as f32 + 0.5;
                let color = if from_bottom < filled_to {
                    let at_surface = filled_to - from_bottom < 1.0 && level < 1.0;
                    let bubble = bubbles
                        .iter()
                        .filter(|(c, r, _)| *c == column && *r == row)
                        .map(|(_, _, light)| *light)
                        .sum::<f32>();
                    let surface_light = if at_surface { SURFACE_LIGHT } else { 0.0 };
                    tones.lit(bubble.max(surface_light))
                } else {
                    tones.empty
                };
                grid.add(&mut mesh, column, row, color);
            }
        }
        ui.painter().add(Shape::mesh(mesh));

        if moving && self.progress.is_none_or(|progress| progress < 1.0) {
            motion.keep_alive(ui, rect);
        }
    }
}

/// Whether a triangle with these `corners` lies inside the round orb.
fn in_orb(corners: [Pos2; 3], center: Pos2, radius: f32) -> bool {
    let centroid = pos2(
        (corners[0].x + corners[1].x + corners[2].x) / 3.0,
        (corners[0].y + corners[1].y + corners[2].y) / 3.0,
    );
    (centroid - center).length() <= radius
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::PROGRESS_FRAME_INTERVAL;
    use std::time::Duration;

    #[test]
    fn the_orb_keeps_triangles_inside_its_circle() {
        let center = pos2(10.0, 10.0);
        let around = |x: f32, y: f32| {
            [
                pos2(x - 1.0, y + 1.0),
                pos2(x + 1.0, y + 1.0),
                pos2(x, y - 2.0),
            ]
        };
        assert!(in_orb(around(10.0, 10.0), center, 9.0));
        assert!(in_orb(around(17.5, 10.0), center, 9.0));
        assert!(!in_orb(around(19.5, 19.5), center, 9.0));
    }

    fn repaint_delay(orb: impl Fn() -> ProgressOrb) -> Duration {
        super::super::settled_repaint_delay(0.1, |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            orb().paint_at(ui, rect);
        })
    }

    #[test]
    fn an_orb_moves_while_work_runs_and_rests_when_held_or_done() {
        let running = || ProgressOrb::new(Some(0.3), Color32::WHITE);
        assert_eq!(repaint_delay(running), PROGRESS_FRAME_INTERVAL);
        let unknown = || ProgressOrb::new(None, Color32::WHITE);
        assert_eq!(repaint_delay(unknown), PROGRESS_FRAME_INTERVAL);
        let held = || ProgressOrb::new(Some(0.3), Color32::WHITE).motion(BarMotion::Held);
        assert!(repaint_delay(held) > Duration::from_secs(60));
        let done = || ProgressOrb::new(Some(1.0), Color32::WHITE);
        assert!(repaint_delay(done) > Duration::from_secs(60));
    }
}
