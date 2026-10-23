use egui::{
    Color32, Mesh, Rect, Response, Sense, Shape, TextStyle, Ui, Widget, WidgetInfo, WidgetType,
    vec2,
};

use super::label::paint_bar_label;
use super::map::{
    BEAM_PERIOD_SECONDS, CellLook, ROWS, beam_center, cell_look, known_color, paint_frontier_dust,
    scan_light,
};
use super::triangles::{Tones, TriangleGrid, blink};
use super::{Motion, bump, fade, mix, noise, particle_life};

/// One bubble per this many filled triangles across, up to `MAX_BUBBLES`.
const COLUMNS_PER_BUBBLE: usize = 5;
const MAX_BUBBLES: usize = 48;
/// One twinkle per this many filled triangles across, up to `MAX_TWINKLES`.
const COLUMNS_PER_TWINKLE: usize = 3;
const MAX_TWINKLES: usize = 64;
/// How much a triangle's own blink lights it.
const BLINK_LIGHT: f32 = 0.45;
/// Sparkles drift off the frontier only once triangles are this tall.
const DUST_MIN_CELL: f32 = 4.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BarMotion {
    /// Work is moving: the filled triangles blink, bubbles of light climb
    /// through them and the frontier glows.
    Forward,
    /// Work is being undone, such as a cancel rolling changes back: the
    /// bubbles sink.
    Backward,
    /// Waiting on the user.
    Held,
    /// Nothing is running.
    #[default]
    Still,
}

/// Triangles fill with mana while work runs; resting and completed bars use
/// solid fills. Drop-in for `egui::ProgressBar`: same default size and track color.
#[must_use = "You should put this widget in a ui with `ui.add(widget);`"]
pub(crate) struct ActivityBar {
    progress: Option<f32>,
    fill: Option<Color32>,
    track: Option<Color32>,
    height: Option<f32>,
    rows: usize,
    motion: BarMotion,
    text: Option<String>,
}

impl ActivityBar {
    /// `None` progress sweeps a beam during forward or backward motion.
    pub(crate) fn new(progress: Option<f32>) -> Self {
        Self {
            progress,
            fill: None,
            track: None,
            height: None,
            rows: ROWS,
            motion: BarMotion::Still,
            text: None,
        }
    }

    pub(crate) fn fill(mut self, color: Color32) -> Self {
        self.fill = Some(color);
        self
    }

    pub(crate) fn track(mut self, color: Color32) -> Self {
        self.track = Some(color);
        self
    }

    pub(crate) fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// At most this many rows of triangles; a bar too short for them drops
    /// rows.
    pub(crate) fn rows(mut self, rows: usize) -> Self {
        self.rows = rows.max(1);
        self
    }

    pub(crate) fn motion(mut self, motion: BarMotion) -> Self {
        self.motion = motion;
        self
    }

    /// Text over the bar's left end, as `egui::ProgressBar::text` draws it.
    pub(crate) fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub(crate) fn paint_at(&self, ui: &Ui, rect: Rect) {
        if !ui.is_rect_visible(rect) || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let visuals = ui.visuals();
        let fill = self.fill.unwrap_or(visuals.selection.bg_fill);
        let track = self.track.unwrap_or(visuals.extreme_bg_color);
        let motion = Motion::of(ui);
        let live = if matches!(self.motion, BarMotion::Still | BarMotion::Held)
            || self.progress.is_some_and(|progress| progress >= 1.0)
        {
            let rounding = visuals.widgets.inactive.corner_radius;
            ui.painter().rect_filled(rect, rounding, track);
            if let Some(progress) = self.progress.filter(|progress| *progress > 0.0) {
                let filled = Rect::from_min_size(
                    rect.min,
                    vec2(rect.width() * progress.clamp(0.0, 1.0), rect.height()),
                );
                ui.painter().rect_filled(filled, rounding, fill);
            }
            false
        } else {
            let empty = empty_color(track, fill);
            let grid = TriangleGrid::fit(rect, self.rows, ui.ctx().pixels_per_point());
            match self.progress {
                Some(progress) => self.paint_mana(ui, &grid, rect, fill, empty, progress, motion),
                None => paint_scan(ui, &grid, rect, fill, empty, motion),
            }
        };

        if let Some(text) = &self.text {
            let painter = ui.painter();
            let galley = painter.layout_no_wrap(
                text.clone(),
                TextStyle::Button.resolve(ui.style()),
                Color32::PLACEHOLDER,
            );
            let color = visuals
                .override_text_color
                .unwrap_or(visuals.selection.stroke.color);
            let pos =
                rect.left_center() + vec2(ui.spacing().item_spacing.x, -galley.size().y / 2.0);
            paint_bar_label(&painter.with_clip_rect(rect), galley, pos, color);
        }

        if live {
            motion.keep_alive(ui, rect);
        }
    }

    /// Returns whether anything in the mana moves with time.
    #[allow(clippy::too_many_arguments)]
    fn paint_mana(
        &self,
        ui: &Ui,
        grid: &TriangleGrid,
        rect: Rect,
        fill: Color32,
        empty: Color32,
        progress: f32,
        motion: Motion,
    ) -> bool {
        let progress = progress.clamp(0.0, 1.0);
        let flowing = matches!(self.motion, BarMotion::Forward | BarMotion::Backward);
        let running = flowing && progress < 1.0;
        let animating = running && motion.animates();
        let sinking = self.motion == BarMotion::Backward;
        let tones = Tones::new(fill, empty);
        let count = grid.count();
        let rows = grid.rows;
        let filled_columns = ((progress * grid.columns as f32) as usize).min(grid.columns);
        let light = if animating {
            mana_light(grid, filled_columns, motion.time(), sinking)
        } else {
            Vec::new()
        };

        let mut mesh = Mesh::default();
        grid.reserve(&mut mesh);
        for column in 0..grid.columns {
            for row in 0..rows {
                let index = column * rows + row;
                let look = match cell_look(progress, index, count, rows) {
                    _ if progress <= 0.0 && !running => CellLook::Pending,
                    CellLook::Verified { glow } => {
                        let lit = light.get(index).copied().unwrap_or(0.0);
                        CellLook::Verified {
                            glow: glow.max(lit),
                        }
                    }
                    look => look,
                };
                let color = known_color(look, &tones, animating, motion);
                grid.add(&mut mesh, column, row, color);
            }
        }
        ui.painter().add(Shape::mesh(mesh));

        if animating && grid.cell_size() >= DUST_MIN_CELL {
            let front = grid.x_at(grid.columns as f32 * progress - 1.0);
            paint_frontier_dust(ui, grid, rect, front, tones.bright, motion);
        }
        running && motion.animates()
    }
}

impl Widget for ActivityBar {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = ui.available_size_before_wrap().x.max(96.0);
        let height = self.height.unwrap_or_else(|| ui.spacing().interact_size.y);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        response.widget_info(|| {
            let mut info = match &self.text {
                Some(text) => {
                    WidgetInfo::labeled(WidgetType::ProgressIndicator, ui.is_enabled(), text)
                }
                None => WidgetInfo::new(WidgetType::ProgressIndicator),
            };
            info.value = self
                .progress
                .map(|progress| (f64::from(progress.clamp(0.0, 1.0)) * 100.0).floor());
            info
        });
        self.paint_at(ui, rect);
        response
    }
}

/// A transfer's progress for its bar: unknown until the first bytes arrive, so
/// a starting transfer sweeps a beam instead of showing empty triangles.
pub(crate) fn transfer_progress(fraction: f32, motion: BarMotion) -> Option<f32> {
    let fraction = fraction.clamp(0.0, 1.0);
    (motion != BarMotion::Forward || fraction > 0.0).then_some(fraction)
}

/// Work with no known end: a beam of light sweeps along the triangles,
/// lighting mana as it passes.
fn paint_scan(
    ui: &Ui,
    grid: &TriangleGrid,
    rect: Rect,
    fill: Color32,
    empty: Color32,
    motion: Motion,
) -> bool {
    let tones = Tones::new(fill, empty);
    let mut mesh = Mesh::default();
    grid.reserve(&mut mesh);
    for column in 0..grid.columns {
        for row in 0..grid.rows {
            let index = column * grid.rows + row;
            let color = tones.rising(scan_light(index, column, grid.columns, motion));
            grid.add(&mut mesh, column, row, color);
        }
    }
    ui.painter().add(Shape::mesh(mesh));
    if motion.animates() && grid.cell_size() >= DUST_MIN_CELL {
        let beam = beam_center(motion.cycle(BEAM_PERIOD_SECONDS), grid.columns);
        paint_frontier_dust(ui, grid, rect, grid.x_at(beam), tones.bright, motion);
    }
    motion.animates()
}

/// Extra light on each triangle, indexed like the grid: every filled
/// triangle blinks on its own rhythm, bubbles climb (or sink through) the
/// filled columns row by row, and twinkles flash single triangles.
fn mana_light(grid: &TriangleGrid, filled_columns: usize, time: f64, sinking: bool) -> Vec<f32> {
    let rows = grid.rows;
    let mut light = vec![0.0_f32; grid.count()];
    if filled_columns == 0 {
        return light;
    }
    for (index, lit) in light.iter_mut().take(filled_columns * rows).enumerate() {
        *lit = BLINK_LIGHT * blink(index, time);
    }
    let bubbles = (filled_columns / COLUMNS_PER_BUBBLE).clamp(1, MAX_BUBBLES) as u32;
    for bubble in 0..bubbles {
        let (life, cycle) = particle_life(bubble, time, 0.8..=1.4, 21);
        let column = lane(bubble, cycle, 23, filled_columns);
        light[column * rows + bubble_row(life, rows, sinking)] += 0.85 * bump(life);
    }
    let twinkles = (filled_columns / COLUMNS_PER_TWINKLE).min(MAX_TWINKLES) as u32;
    for twinkle in 0..twinkles {
        let (life, cycle) = particle_life(twinkle, time, 0.5..=1.0, 33);
        let column = lane(twinkle, cycle, 35, filled_columns);
        let row = lane(twinkle, cycle, 37, rows);
        light[column * rows + row] += bump(life).powi(2);
    }
    light
}

/// The row a bubble `life` of the way through its trip is in: rising from the
/// floor to the top, or sinking the other way.
fn bubble_row(life: f32, rows: usize, sinking: bool) -> usize {
    let travel = if sinking { life } else { 1.0 - life };
    ((travel.clamp(0.0, 1.0) * rows as f32) as usize).min(rows.saturating_sub(1))
}

/// A stable pick in `0..span` for one life of a particle.
fn lane(index: u32, cycle: u32, seed: u32, span: usize) -> usize {
    let pick = noise(index.wrapping_mul(977).wrapping_add(cycle), seed);
    ((pick * span as f32) as usize).min(span.saturating_sub(1))
}

/// Empty triangles: the track tinted toward the fill, or a faint fill over a
/// clear track.
fn empty_color(track: Color32, fill: Color32) -> Color32 {
    if track.a() == 0 {
        fade(fill, 0.16)
    } else {
        mix(track, fill, 0.12)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::PROGRESS_FRAME_INTERVAL;
    use egui::pos2;
    use std::time::Duration;

    fn grid(width: f32) -> TriangleGrid {
        TriangleGrid::fit(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 34.0)),
            ROWS,
            1.0,
        )
    }

    #[test]
    fn a_starting_transfer_sweeps_a_beam_until_bytes_arrive() {
        assert_eq!(transfer_progress(0.0, BarMotion::Forward), None);
        assert_eq!(transfer_progress(0.2, BarMotion::Forward), Some(0.2));
        assert_eq!(transfer_progress(0.0, BarMotion::Held), Some(0.0));
        assert_eq!(transfer_progress(0.0, BarMotion::Still), Some(0.0));
        assert_eq!(transfer_progress(1.4, BarMotion::Forward), Some(1.0));
    }

    #[test]
    fn blinks_bubbles_and_twinkles_light_only_filled_columns() {
        let triangles = grid(400.0);
        let filled = triangles.columns / 3;
        for step in 0..40 {
            let light = mana_light(&triangles, filled, f64::from(step) * 0.11, false);
            assert_eq!(light.len(), triangles.count());
            assert!(
                light[filled * triangles.rows..]
                    .iter()
                    .all(|lit| *lit == 0.0)
            );
        }
        let lit = mana_light(&triangles, filled, 1.3, false);
        assert!(lit.iter().any(|lit| *lit > 0.0));
        assert!(
            mana_light(&triangles, 0, 1.3, false)
                .iter()
                .all(|lit| *lit == 0.0)
        );
    }

    #[test]
    fn rising_bubbles_climb_and_sinking_ones_fall() {
        assert_eq!(bubble_row(0.05, 4, false), 3);
        assert_eq!(bubble_row(0.95, 4, false), 0);
        assert_eq!(bubble_row(0.05, 4, true), 0);
        assert_eq!(bubble_row(0.95, 4, true), 3);
        assert_eq!(bubble_row(0.5, 1, false), 0);
    }

    #[test]
    fn empty_triangles_show_over_a_clear_track() {
        let fill = Color32::from_rgb(200, 100, 20);
        assert!(empty_color(Color32::TRANSPARENT, fill).a() > 0);
        assert_eq!(empty_color(Color32::BLACK, fill).a(), 255);
    }

    fn repaint_delay(animation_time: f32, bar: impl Fn() -> ActivityBar) -> Duration {
        super::super::settled_repaint_delay(animation_time, |ui| {
            ui.add(bar());
        })
    }

    #[test]
    fn a_moving_bar_asks_for_paced_frames() {
        let forward = || ActivityBar::new(Some(0.4)).motion(BarMotion::Forward);
        assert_eq!(repaint_delay(0.1, forward), PROGRESS_FRAME_INTERVAL);
        let draining = || ActivityBar::new(Some(0.4)).motion(BarMotion::Backward);
        assert_eq!(repaint_delay(0.1, draining), PROGRESS_FRAME_INTERVAL);
        let thin = || {
            ActivityBar::new(Some(0.4))
                .height(4.0)
                .rows(1)
                .motion(BarMotion::Forward)
        };
        assert_eq!(repaint_delay(0.1, thin), PROGRESS_FRAME_INTERVAL);
        assert_eq!(
            repaint_delay(0.1, || ActivityBar::new(None).motion(BarMotion::Forward)),
            PROGRESS_FRAME_INTERVAL
        );
    }

    #[test]
    fn a_still_bar_and_reduced_motion_ask_for_no_frames() {
        let still = || ActivityBar::new(Some(0.4));
        assert!(repaint_delay(0.1, still) > Duration::from_secs(60));
        let held = || ActivityBar::new(Some(0.4)).motion(BarMotion::Held);
        assert!(repaint_delay(0.1, held) > Duration::from_secs(60));
        let full = || ActivityBar::new(Some(1.0)).motion(BarMotion::Forward);
        assert!(repaint_delay(0.1, full) > Duration::from_secs(60));
        assert!(repaint_delay(0.1, || ActivityBar::new(None)) > Duration::from_secs(60));
        let held_unknown = || ActivityBar::new(None).motion(BarMotion::Held);
        assert!(repaint_delay(0.1, held_unknown) > Duration::from_secs(60));
        let forward = || ActivityBar::new(Some(0.4)).motion(BarMotion::Forward);
        assert!(repaint_delay(0.0, forward) > Duration::from_secs(60));
        assert!(
            repaint_delay(0.0, || ActivityBar::new(None).motion(BarMotion::Forward))
                > Duration::from_secs(60)
        );
    }
}
