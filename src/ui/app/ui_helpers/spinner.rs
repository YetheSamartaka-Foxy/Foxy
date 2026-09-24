use std::time::Duration;

use egui::{Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Widget, WidgetInfo};
use egui::{WidgetType, emath::lerp, vec2};

/// How often a spinner or live progress asks for its next frame. egui's own
/// spinner asks for one on every paint, which keeps the whole UI redrawing at
/// the display rate for as long as any spinner is on screen.
pub(crate) const PROGRESS_FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Ask for the next frame no sooner than `delay` from now. egui starts a delayed
/// frame one predicted frame early (1/60 s unless the platform says otherwise),
/// so a 16 ms request would otherwise repaint at once.
pub(crate) fn request_frame_after(ctx: &egui::Context, delay: Duration) {
    let predicted = Duration::try_from_secs_f32(ctx.input(|i| i.predicted_dt)).unwrap_or_default();
    ctx.request_repaint_after(delay + predicted);
}

/// egui's spinner, drawn the same way, animated at [`PROGRESS_FRAME_INTERVAL`].
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
        request_frame_after(ui.ctx(), PROGRESS_FRAME_INTERVAL);
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
}
