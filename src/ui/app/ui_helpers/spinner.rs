use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use egui::{Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Widget, WidgetInfo};
use egui::{WidgetType, emath::lerp, vec2};

/// How often a spinner or live progress asks for its next frame on Auto. egui's
/// own spinner asks for one on every paint, which keeps the whole UI redrawing
/// at the display rate for as long as any spinner is on screen.
pub(crate) const PROGRESS_FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Fastest and slowest progress frame rates the setting accepts.
const PROGRESS_FRAME_RATE_RANGE: std::ops::RangeInclusive<u32> = 1..=60;

/// Interval the user's progress frame rate asks for, in microseconds; 0 means Auto.
static PROGRESS_FRAME_INTERVAL_OVERRIDE_US: AtomicU64 = AtomicU64::new(0);

fn frame_interval_for_rate(frames_per_second: u32) -> Duration {
    let rate = frames_per_second.clamp(
        *PROGRESS_FRAME_RATE_RANGE.start(),
        *PROGRESS_FRAME_RATE_RANGE.end(),
    );
    Duration::from_micros(1_000_000 / u64::from(rate))
}

/// Pace progress animations and progress-driven repaints at `frames_per_second`,
/// or at [`PROGRESS_FRAME_INTERVAL`] with `None`.
pub(crate) fn set_progress_frame_rate(frames_per_second: Option<u32>) {
    let micros = frames_per_second
        .map(|rate| frame_interval_for_rate(rate).as_micros() as u64)
        .unwrap_or(0);
    PROGRESS_FRAME_INTERVAL_OVERRIDE_US.store(micros, Ordering::Relaxed);
}

/// Progress pacing while another program, such as a game, has the foreground.
const BACKGROUND_PROGRESS_FRAME_INTERVAL: Duration = Duration::from_millis(100);
/// Progress pacing while the window is minimized or hidden to the tray.
const HIDDEN_PROGRESS_FRAME_INTERVAL: Duration = Duration::from_secs(1);

/// Whether the window can be seen and has the user's attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowPresence {
    Foreground,
    Background,
    Hidden,
}

static WINDOW_PRESENCE: AtomicU8 = AtomicU8::new(0);

/// Record where the window is, once a frame; progress paces slower while it is
/// not in front, so Foxy's drawing does not compete with the program that is.
pub(crate) fn set_window_presence(presence: WindowPresence) {
    let encoded = match presence {
        WindowPresence::Foreground => 0,
        WindowPresence::Background => 1,
        WindowPresence::Hidden => 2,
    };
    WINDOW_PRESENCE.store(encoded, Ordering::Relaxed);
}

pub(crate) fn window_presence() -> WindowPresence {
    match WINDOW_PRESENCE.load(Ordering::Relaxed) {
        1 => WindowPresence::Background,
        2 => WindowPresence::Hidden,
        _ => WindowPresence::Foreground,
    }
}

fn presence_frame_interval(presence: WindowPresence) -> Option<Duration> {
    match presence {
        WindowPresence::Foreground => None,
        WindowPresence::Background => Some(BACKGROUND_PROGRESS_FRAME_INTERVAL),
        WindowPresence::Hidden => Some(HIDDEN_PROGRESS_FRAME_INTERVAL),
    }
}

/// The slower of two optional pacing limits.
fn slowest(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The slowest pacing in force beyond Auto: the user's progress frame rate,
/// or the slower pacing used while the window is not in front.
pub(crate) fn progress_frame_interval_override() -> Option<Duration> {
    let setting = match PROGRESS_FRAME_INTERVAL_OVERRIDE_US.load(Ordering::Relaxed) {
        0 => None,
        micros => Some(Duration::from_micros(micros)),
    };
    slowest(setting, presence_frame_interval(window_presence()))
}

/// How often live progress asks for its next frame.
pub(crate) fn progress_frame_interval() -> Duration {
    progress_frame_interval_override().unwrap_or(PROGRESS_FRAME_INTERVAL)
}

/// Ask for the next frame no sooner than `delay` from now. egui starts a delayed
/// frame one predicted frame early (1/60 s unless the platform says otherwise),
/// so a 16 ms request would otherwise repaint at once.
pub(crate) fn request_frame_after(ctx: &egui::Context, delay: Duration) {
    let predicted = Duration::try_from_secs_f32(ctx.input(|i| i.predicted_dt)).unwrap_or_default();
    ctx.request_repaint_after(delay + predicted);
}

/// egui's spinner, drawn the same way, animated at [`progress_frame_interval`].
#[must_use = "You should put this widget in a ui with `ui.add(widget);`"]
#[derive(Default)]
pub(crate) struct PacedSpinner {
    size: Option<f32>,
}

impl PacedSpinner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub(crate) fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    pub(crate) fn paint_at(&self, ui: &Ui, rect: Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        request_frame_after(ui.ctx(), progress_frame_interval());
        let color = ui.visuals().strong_text_color();
        let radius = (rect.height().min(rect.width()) / 2.0) - 2.0;
        let n_points = (radius.round() as u32).clamp(8, 128);
        let time = ui.input(|i| i.time);
        let start_angle = time * std::f64::consts::TAU;
        let end_angle = start_angle + 240f64.to_radians() * time.sin();
        let points: Vec<Pos2> = (0..n_points)
            .map(|i| {
                let angle = lerp(start_angle..=end_angle, i as f64 / n_points as f64);
                let (sin, cos) = angle.sin_cos();
                rect.center() + radius * vec2(cos as f32, sin as f32)
            })
            .collect();
        ui.painter()
            .add(Shape::line(points, Stroke::new(3.0, color)));
    }
}

impl Widget for PacedSpinner {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = self
            .size
            .unwrap_or_else(|| ui.style().spacing.interact_size.y);
        let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
        response.widget_info(|| WidgetInfo::new(WidgetType::ProgressIndicator));
        self.paint_at(ui, rect);
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_spinner(ctx: &egui::Context) -> egui::FullOutput {
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.add(PacedSpinner::new());
        })
    }

    #[test]
    fn a_visible_spinner_asks_for_its_next_frame_after_the_interval_not_at_once() {
        let ctx = egui::Context::default();
        // The first two passes always ask for another while fonts and layout settle.
        for _ in 0..2 {
            frame_with_spinner(&ctx).textures_delta.clear();
        }
        let mut output = frame_with_spinner(&ctx);
        output.textures_delta.clear();
        let delay = output
            .viewport_output
            .values()
            .map(|viewport| viewport.repaint_delay)
            .min()
            .unwrap();
        assert_eq!(delay, PROGRESS_FRAME_INTERVAL);
    }

    #[test]
    fn a_window_out_of_front_paces_progress_slower() {
        assert_eq!(presence_frame_interval(WindowPresence::Foreground), None);
        assert_eq!(
            presence_frame_interval(WindowPresence::Background),
            Some(BACKGROUND_PROGRESS_FRAME_INTERVAL)
        );
        assert_eq!(
            presence_frame_interval(WindowPresence::Hidden),
            Some(HIDDEN_PROGRESS_FRAME_INTERVAL)
        );
    }

    #[test]
    fn the_slower_of_the_setting_and_the_window_pacing_wins() {
        let ms = Duration::from_millis;
        assert_eq!(slowest(None, None), None);
        assert_eq!(slowest(Some(ms(200)), None), Some(ms(200)));
        assert_eq!(slowest(None, Some(ms(100))), Some(ms(100)));
        assert_eq!(slowest(Some(ms(50)), Some(ms(100))), Some(ms(100)));
        assert_eq!(slowest(Some(ms(200)), Some(ms(100))), Some(ms(200)));
    }

    #[test]
    fn frame_rates_become_intervals_within_the_accepted_range() {
        assert_eq!(frame_interval_for_rate(10), Duration::from_millis(100));
        assert_eq!(frame_interval_for_rate(0), Duration::from_secs(1));
        assert_eq!(frame_interval_for_rate(500), frame_interval_for_rate(60));
    }
}
