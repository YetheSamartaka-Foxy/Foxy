use egui::{
    Color32, Mesh, Rect, Response, Sense, Shape, Ui, Widget, WidgetInfo, WidgetType, pos2, vec2,
};

use super::sprites::add_dust;
use super::textures::ActivityTextures;
use super::triangles::{Tones, TriangleGrid};
use super::{Motion, bump, noise};

pub(super) const ROWS: usize = 4;
/// Sparkles drifting up off the cells being verified.
pub(super) const DUST_MOTES: usize = 8;
const CELL: f32 = 7.0;
const GAP: f32 = 2.0;
pub(super) const FRONT_PULSE_SECONDS: f64 = 1.1;
pub(super) const BEAM_PERIOD_SECONDS: f64 = 2.6;
/// Beam half-width in columns while the amount of work is unknown.
const BEAM_SPREAD_COLUMNS: f32 = 3.0;

/// A defragmenter-style map of the data being verified. Cells settle column by
/// column with a little jitter, as parallel hashing finishes work slightly out
/// of order; the cells being read blink, and freshly verified ones glow.
#[must_use = "You should put this widget in a ui with `ui.add(widget);`"]
pub(crate) struct VerificationMap {
    progress: Option<f32>,
    done: Color32,
    pending: Color32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum CellLook {
    Pending,
    /// Being read now; `phase` staggers the cells' flicker.
    Reading {
        phase: f32,
    },
    /// Verified; `glow` fades from 1 at the frontier to 0 behind it.
    Verified {
        glow: f32,
    },
}

impl VerificationMap {
    /// Four rows of the map's triangles; progress bars this tall match it exactly.
    pub(crate) const HEIGHT: f32 = ROWS as f32 * CELL + (ROWS - 1) as f32 * GAP;

    pub(crate) fn new(progress: Option<f32>, done: Color32, pending: Color32) -> Self {
        Self {
            progress,
            done,
            pending,
        }
    }

    pub(crate) fn paint_at(&self, ui: &Ui, rect: Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let motion = Motion::of(ui);
        let grid = TriangleGrid::fit(rect, ROWS, ui.ctx().pixels_per_point());
        let count = grid.count();
        let tones = Tones::new(self.done, self.pending);
        let mut mesh = Mesh::default();
        grid.reserve(&mut mesh);

        for column in 0..grid.columns {
            for row in 0..grid.rows {
                let index = column * grid.rows + row;
                let color = match self.progress {
                    Some(progress) => {
                        let look = cell_look(progress, index, count, grid.rows);
                        known_color(look, &tones, motion.animates(), motion)
                    }
                    None => tones.rising(scan_light(index, column, grid.columns, motion)),
                };
                grid.add(&mut mesh, column, row, color);
            }
        }
        ui.painter().add(Shape::mesh(mesh));

        let working = self.progress.is_none_or(|progress| progress < 1.0);
        if working && motion.animates() {
            let center = match self.progress {
                Some(progress) => grid.x_at(grid.columns as f32 * progress - 1.0),
                None => grid.x_at(beam_center(motion.cycle(BEAM_PERIOD_SECONDS), grid.columns)),
            };
            paint_frontier_dust(ui, &grid, rect, center, tones.bright, motion);
        }
        if working {
            motion.keep_alive(ui, rect);
        }
    }
}

/// A known-progress triangle's tone: empty ahead, blinking while being read,
/// filled behind the frontier and lit where it still glows.
pub(super) fn known_color(
    look: CellLook,
    tones: &Tones,
    animating: bool,
    motion: Motion,
) -> Color32 {
    match look {
        CellLook::Pending => tones.empty,
        CellLook::Reading { phase } => tones.rising(reading_pulse(phase, animating, motion)),
        CellLook::Verified { glow } => tones.lit(glow),
    }
}

impl Widget for VerificationMap {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = ui.available_size_before_wrap().x.max(96.0);
        let (rect, response) = ui.allocate_exact_size(vec2(width, Self::HEIGHT), Sense::hover());
        response.widget_info(|| {
            let mut info = WidgetInfo::new(WidgetType::ProgressIndicator);
            info.value = self
                .progress
                .map(|progress| (f64::from(progress.clamp(0.0, 1.0)) * 100.0).floor());
            info
        });
        self.paint_at(ui, rect);
        response
    }
}

/// How lit a triangle being read is: each one blinks on its own phase; a
/// still frame shows it half lit.
fn reading_pulse(phase: f32, animating: bool, motion: Motion) -> f32 {
    if animating {
        bump((motion.cycle(FRONT_PULSE_SECONDS) + phase).fract())
    } else {
        0.5
    }
}

/// Sparkles drifting up from the triangles around `center_x`.
pub(super) fn paint_frontier_dust(
    ui: &Ui,
    grid: &TriangleGrid,
    rect: Rect,
    center_x: f32,
    color: Color32,
    motion: Motion,
) {
    let reach = 1.5 * grid.pitch();
    let area = Rect::from_min_max(
        pos2(center_x - reach, rect.top()),
        pos2(center_x + reach, rect.bottom()),
    );
    let mut dust = Mesh::with_texture(ActivityTextures::of(ui.ctx()).sparkle());
    add_dust(
        &mut dust,
        area,
        grid.cell_size() * 1.6,
        motion.time(),
        color,
        0.9,
        DUST_MOTES,
        91,
    );
    ui.painter().add(Shape::mesh(dust));
}

/// How far toward lit a triangle is while the scanning beam sweeps work with no
/// known end; a still frame shows a faint, even glow.
pub(super) fn scan_light(index: usize, column: usize, columns: usize, motion: Motion) -> f32 {
    if !motion.animates() {
        return 0.12;
    }
    let intensity = beam_intensity(motion.cycle(BEAM_PERIOD_SECONDS), column, columns);
    let flicker = 0.75 + 0.25 * noise(index as u32, 31);
    intensity * flicker
}

/// Progress at which cell `index` of `count` is verified. Cells run column by
/// column, each jittered by up to about one column either way.
fn cell_threshold(index: usize, count: usize, rows: usize) -> f32 {
    let count = count.max(1) as f32;
    let jitter = (noise(index as u32, 11) - 0.5) * rows as f32 * 1.5;
    ((index as f32 + 0.5 + jitter) / count).clamp(0.5 / count, 1.0)
}

pub(super) fn cell_look(progress: f32, index: usize, count: usize, rows: usize) -> CellLook {
    let progress = progress.clamp(0.0, 1.0);
    let threshold = cell_threshold(index, count, rows);
    let cells = count.max(1) as f32;
    let front = rows as f32 * 2.0 / cells;
    let trail = rows as f32 * 5.0 / cells;
    if progress >= threshold {
        let behind = (progress - threshold) / trail;
        CellLook::Verified {
            glow: if progress >= 1.0 {
                0.0
            } else {
                (1.0 - behind).clamp(0.0, 1.0).powf(1.5)
            },
        }
    } else if threshold - progress <= front {
        CellLook::Reading {
            phase: noise(index as u32, 23),
        }
    } else {
        CellLook::Pending
    }
}

/// How strongly the scanning beam lights `column` when `beam` is the share of
/// its sweep across `columns` columns, run past both edges.
fn beam_intensity(beam: f32, column: usize, columns: usize) -> f32 {
    let distance = (column as f32 - beam_center(beam, columns)) / BEAM_SPREAD_COLUMNS;
    (-distance * distance).exp()
}

/// The column the beam is over, `beam` of the way through a sweep that runs
/// past both edges.
pub(super) fn beam_center(beam: f32, columns: usize) -> f32 {
    let span = columns as f32 + 4.0 * BEAM_SPREAD_COLUMNS;
    beam * span - 2.0 * BEAM_SPREAD_COLUMNS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_threshold_lies_inside_the_run() {
        let count = 400;
        for index in 0..count {
            let threshold = cell_threshold(index, count, 4);
            assert!(threshold > 0.0 && threshold <= 1.0);
        }
    }

    #[test]
    fn verified_cells_track_the_progress() {
        let count = 400;
        let verified = |progress: f32| {
            (0..count)
                .filter(|&index| {
                    matches!(
                        cell_look(progress, index, count, 4),
                        CellLook::Verified { .. }
                    )
                })
                .count()
        };
        assert_eq!(verified(0.0), 0);
        assert_eq!(verified(1.0), count);
        for progress in [0.1_f32, 0.25, 0.5, 0.75, 0.9] {
            let expected = progress * count as f32;
            let actual = verified(progress) as f32;
            assert!(
                (actual - expected).abs() <= 8.0,
                "{actual} verified at {progress}"
            );
        }
    }

    #[test]
    fn the_frontier_glows_and_a_finished_map_settles() {
        let count = 400;
        let glow_at = |progress: f32, index: usize| match cell_look(progress, index, count, 4) {
            CellLook::Verified { glow } => Some(glow),
            _ => None,
        };
        let frontier = (0..count)
            .filter_map(|index| glow_at(0.5, index))
            .fold(0.0_f32, f32::max);
        assert!(frontier > 0.8);
        assert_eq!(glow_at(0.5, 0), Some(0.0));
        assert!((0..count).all(|index| glow_at(1.0, index) == Some(0.0)));
    }

    #[test]
    fn cells_just_ahead_of_the_frontier_are_being_read() {
        let count = 400;
        let reading = (0..count)
            .filter(|&index| matches!(cell_look(0.5, index, count, 4), CellLook::Reading { .. }))
            .count();
        assert!((2..=16).contains(&reading), "{reading} cells reading");
        let starting = (0..count)
            .filter(|&index| matches!(cell_look(0.0, index, count, 4), CellLook::Reading { .. }))
            .count();
        assert!(
            starting > 0,
            "a run that just started shows cells being read"
        );
    }

    #[test]
    fn the_beam_lights_the_column_under_it() {
        let columns = 50;
        let lit = |beam: f32| {
            (0..columns)
                .max_by(|a, b| {
                    beam_intensity(beam, *a, columns).total_cmp(&beam_intensity(beam, *b, columns))
                })
                .unwrap()
        };
        assert!(lit(0.3) < lit(0.6));
        assert!(beam_intensity(0.0, columns - 1, columns) < 0.01);
        assert!(beam_center(0.0, columns) < 0.0);
        assert!(beam_center(1.0, columns) > columns as f32);
    }
}
