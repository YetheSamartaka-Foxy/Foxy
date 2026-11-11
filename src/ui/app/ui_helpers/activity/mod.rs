mod bar;
mod beam;
mod chart;
mod glyph;
mod label;
mod map;
mod orb;
mod sprites;
mod steps;
mod textures;
mod triangles;

pub(crate) use bar::{ActivityBar, BarMotion, transfer_progress};
pub(crate) use beam::{paint_border_beam, paint_border_flare};
pub(crate) use chart::{paint_area_under, paint_live_marker};
pub(crate) use glyph::{ActivityGlyph, ActivityIcon};
pub(crate) use label::paint_bar_label;
pub(crate) use map::VerificationMap;
pub(crate) use orb::ProgressOrb;
pub(crate) use steps::StepTrack;

use std::f32::consts::{PI, TAU};
use std::ops::RangeInclusive;

use egui::emath::lerp;
use egui::{Color32, Id, Pos2, Rect, Ui, vec2};

use super::spinner::{progress_frame_interval, request_frame_after};
use crate::ui::app::Foxy;

/// The animation clock of one frame. Reduce motion zeroes egui's animation
/// time; the activity visuals then draw their current state without moving
/// and ask for no frames of their own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Motion {
    time: f64,
    animate: bool,
}

impl Motion {
    pub(crate) fn of(ui: &Ui) -> Self {
        let animate = ui.style().animation_time > 0.0;
        let time = if animate { ui.input(|i| i.time) } else { 0.0 };
        Self { time, animate }
    }

    #[cfg(test)]
    pub(crate) fn at(time: f64, animate: bool) -> Self {
        Self { time, animate }
    }

    pub(crate) fn animates(self) -> bool {
        self.animate
    }

    /// Seconds on the animation clock; 0 while motion is reduced.
    pub(crate) fn time(self) -> f64 {
        self.time
    }

    /// Position inside a cycle of `period` seconds, in `0.0..1.0`. Reduced in
    /// f64 first so the phase stays smooth after days of uptime.
    pub(crate) fn cycle(self, period: f64) -> f32 {
        self.cycle_ago(period, 0.0)
    }

    /// Where the same cycle stood `seconds` ago.
    pub(crate) fn cycle_ago(self, period: f64, seconds: f64) -> f32 {
        if period <= 0.0 {
            return 0.0;
        }
        ((self.time - seconds).rem_euclid(period) / period) as f32
    }

    /// Ask for the next animation frame while `rect` is on screen.
    pub(crate) fn keep_alive(self, ui: &Ui, rect: Rect) {
        if self.animate && ui.is_rect_visible(rect) {
            request_frame_after(ui.ctx(), progress_frame_interval());
        }
    }
}

pub(crate) fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

pub(crate) fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// 0 at both ends and 1 in the middle of `0.0..=1.0`, with flat shoulders.
pub(crate) fn bump(t: f32) -> f32 {
    (PI * t.clamp(0.0, 1.0)).sin().powi(2)
}

/// Stable pseudo-random value in `0.0..1.0`, so per-cell jitter and phases
/// stay put from frame to frame.
pub(crate) fn noise(index: u32, seed: u32) -> f32 {
    let mut x = index.wrapping_mul(0x9E37_79B9) ^ seed.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

pub(crate) fn mix(from: Color32, to: Color32, amount: f32) -> Color32 {
    Foxy::blend_color(from, to, amount)
}

pub(crate) fn fade(color: Color32, opacity: f32) -> Color32 {
    Foxy::color_with_alpha(color, opacity)
}

/// `color` at `intensity`, added onto what lies below it so it glows instead
/// of covering.
pub(crate) fn additive(color: Color32, intensity: f32) -> Color32 {
    fade(color, intensity).additive()
}

/// Where one particle stands in its endlessly repeating life: `0.0..1.0`
/// through the current life, and which life it is, so each life can start in
/// a new place. Lives last between `periods` seconds, staggered per particle.
pub(crate) fn particle_life(
    index: u32,
    time: f64,
    periods: RangeInclusive<f32>,
    seed: u32,
) -> (f32, u32) {
    let period = f64::from(lerp(periods, noise(index, seed))).max(0.05);
    let phase = time / period + f64::from(noise(index, seed.wrapping_add(1)));
    (phase.fract() as f32, phase.floor() as u32)
}

fn point_at(center: Pos2, radius: f32, angle: f32) -> Pos2 {
    center + radius * vec2(angle.cos(), angle.sin())
}

/// Points along an arc from `start`, sweeping `sweep` radians clockwise on screen.
fn arc_points(center: Pos2, radius: f32, start: f32, sweep: f32) -> Vec<Pos2> {
    let steps = ((sweep.abs() / TAU) * 48.0).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|step| point_at(center, radius, start + sweep * step as f32 / steps as f32))
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct RiseMemory {
    on: bool,
    since: f64,
    pass: u64,
}

/// Seconds since `on` last turned true for `id`. A state that is already true
/// when first drawn, or again after its view was off screen, starts no
/// transition, so returning to a finished view does not replay its effects.
pub(crate) fn rise_age(ui: &Ui, id: Id, on: bool) -> Option<f64> {
    let now = ui.input(|i| i.time);
    let pass = ui.ctx().cumulative_pass_nr();
    let memory = ui.data_mut(|data| {
        let fresh = RiseMemory {
            on,
            since: f64::NEG_INFINITY,
            pass,
        };
        let entry = data.get_temp_mut_or_insert_with(id, || fresh);
        if pass.saturating_sub(entry.pass) > 2 {
            *entry = fresh;
        } else if entry.on != on {
            entry.on = on;
            entry.since = now;
        }
        entry.pass = pass;
        *entry
    });
    (memory.on && memory.since.is_finite()).then(|| (now - memory.since).max(0.0))
}

/// `0.0..1.0` through a one-shot effect of `duration` seconds that started when
/// `on` turned true, or `None` once it has played or when motion is reduced.
pub(crate) fn rise_effect(ui: &Ui, id: Id, on: bool, duration: f64) -> Option<f32> {
    let age = rise_age(ui, id, on)?;
    (Motion::of(ui).animates() && age < duration).then(|| (age / duration) as f32)
}

/// The repaint delay a frame asks for once fonts and layout have settled.
#[cfg(test)]
fn settled_repaint_delay(animation_time: f32, add: impl Fn(&mut Ui)) -> std::time::Duration {
    let ctx = egui::Context::default();
    ctx.all_styles_mut(|style| style.animation_time = animation_time);
    let mut delay = std::time::Duration::MAX;
    for _ in 0..3 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| add(ui));
        output.textures_delta.clear();
        delay = output
            .viewport_output
            .values()
            .map(|viewport| viewport.repaint_delay)
            .min()
            .unwrap_or(std::time::Duration::MAX);
    }
    delay
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_stable_and_stays_in_the_unit_range() {
        for index in 0..2_000 {
            let value = noise(index, 7);
            assert!((0.0..1.0).contains(&value));
            assert_eq!(value, noise(index, 7));
        }
        assert_ne!(noise(3, 1), noise(3, 2));
    }

    #[test]
    fn easing_curves_start_at_zero_and_end_at_one() {
        for curve in [ease_in_out, ease_out] {
            assert_eq!(curve(0.0), 0.0);
            assert!((curve(1.0) - 1.0).abs() < 1e-6);
            assert!(curve(0.25) < curve(0.75));
        }
        assert_eq!(bump(0.0), 0.0);
        assert!((bump(0.5) - 1.0).abs() < 1e-6);
        assert!(bump(1.0) < 1e-6);
    }

    #[test]
    fn an_arc_runs_from_its_start_to_its_end_on_the_circle() {
        let center = egui::pos2(10.0, 10.0);
        let points = arc_points(center, 8.0, -PI / 2.0, TAU * 0.25);
        let first = points.first().copied().unwrap();
        let last = points.last().copied().unwrap();
        assert!((first - egui::pos2(10.0, 2.0)).length() < 1e-4);
        assert!((last - egui::pos2(18.0, 10.0)).length() < 1e-4);
        assert!(
            points
                .iter()
                .all(|point| ((*point - center).length() - 8.0).abs() < 1e-4)
        );
        assert!(arc_points(center, 5.0, 0.0, 0.001).len() >= 2);
    }

    #[test]
    fn a_particle_lives_again_and_again() {
        let (life, cycle) = particle_life(3, 0.0, 1.0..=1.0, 9);
        assert!((0.0..1.0).contains(&life));
        let (later_life, later_cycle) = particle_life(3, 1.0, 1.0..=1.0, 9);
        assert_eq!(later_cycle, cycle + 1);
        assert!((later_life - life).abs() < 1e-4);
        for step in 0..200 {
            let (life, _) = particle_life(step, f64::from(step) * 0.37, 0.8..=1.6, 4);
            assert!((0.0..1.0).contains(&life));
        }
    }

    #[test]
    fn a_glow_adds_light_instead_of_covering() {
        let glow = additive(Color32::from_rgb(200, 100, 20), 0.5);
        assert_eq!(glow.a(), 0);
        assert!(glow.r() > 90 && glow.r() < 110);
    }

    #[test]
    fn the_cycle_wraps_and_stays_precise_after_long_uptime() {
        assert_eq!(Motion::at(0.5, true).cycle(2.0), 0.25);
        assert_eq!(Motion::at(2.5, true).cycle(2.0), 0.25);
        let late = Motion::at(30.0 * 86_400.0 + 0.5, true).cycle(2.0);
        assert!((late - 0.25).abs() < 1e-4);
        assert_eq!(Motion::at(1.0, true).cycle(0.0), 0.0);
        assert_eq!(Motion::at(2.5, true).cycle_ago(2.0, 1.0), 0.75);
    }

    fn pass(ctx: &egui::Context, time: f64, on: bool) -> Option<f64> {
        let mut age = None;
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            age = rise_age(ui, Id::unique("rise"), on);
        });
        output.textures_delta.clear();
        age
    }

    #[test]
    fn a_rise_is_timed_from_the_frame_it_happens() {
        let ctx = egui::Context::default();
        assert_eq!(pass(&ctx, 1.0, false), None);
        assert_eq!(pass(&ctx, 2.0, true), Some(0.0));
        assert_eq!(pass(&ctx, 2.5, true), Some(0.5));
        assert_eq!(pass(&ctx, 3.0, false), None);
    }

    #[test]
    fn a_state_already_on_when_first_drawn_starts_no_transition() {
        let ctx = egui::Context::default();
        assert_eq!(pass(&ctx, 1.0, true), None);
        assert_eq!(pass(&ctx, 2.0, true), None);
    }
}
