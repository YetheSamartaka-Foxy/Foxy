use egui::{Color32, Rect, Response, Sense, Ui, Widget, WidgetInfo, WidgetType, pos2, vec2};

use super::{ActivityBar, BarMotion};

/// Above this many steps, or below this block width, the steps merge into one
/// bar: slivers stop reading as separate items.
const MAX_SEGMENTS: usize = 40;
const MIN_SEGMENT_WIDTH: f32 = 12.0;
const SEGMENT_GAP: f32 = 4.0;
const HEIGHT: f32 = 16.0;

/// A row of triangle blocks, one per item of a bulk run: finished items full of
/// mana, the running one filling, the rest empty.
#[must_use = "You should put this widget in a ui with `ui.add(widget);`"]
pub(crate) struct StepTrack {
    done: usize,
    total: usize,
    running: Option<(Option<f32>, BarMotion)>,
    fill: Color32,
    track: Color32,
}

impl StepTrack {
    pub(crate) fn new(done: usize, total: usize, fill: Color32, track: Color32) -> Self {
        Self {
            done,
            total,
            running: None,
            fill,
            track,
        }
    }

    /// The item after the finished ones is running, `progress` through, or
    /// with no known end when `None`.
    pub(crate) fn running(mut self, progress: Option<f32>, motion: BarMotion) -> Self {
        self.running = Some((progress, motion));
        self
    }

    fn overall(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        let current = self
            .running
            .and_then(|(progress, _)| progress)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        ((self.done.min(self.total) as f32 + current) / self.total as f32).min(1.0)
    }

    fn paint_at(&self, ui: &Ui, rect: Rect) {
        if self.total == 0 || !ui.is_rect_visible(rect) {
            return;
        }
        let motion = self.running.map_or(BarMotion::Still, |(_, motion)| motion);
        let Some(width) = segment_width(rect.width(), self.total) else {
            ActivityBar::new(Some(self.overall()))
                .fill(self.fill)
                .track(self.track)
                .motion(motion)
                .paint_at(ui, rect);
            return;
        };
        for step in 0..self.total {
            let left = rect.left() + step as f32 * (width + SEGMENT_GAP);
            let vial = Rect::from_min_size(pos2(left, rect.top()), vec2(width, rect.height()));
            match (step.cmp(&self.done), self.running) {
                (std::cmp::Ordering::Less, _) => {
                    ActivityBar::new(Some(1.0))
                        .fill(self.fill)
                        .track(self.track)
                        .paint_at(ui, vial);
                }
                (std::cmp::Ordering::Equal, Some((progress, _))) => {
                    ActivityBar::new(progress)
                        .fill(self.fill)
                        .track(self.track)
                        .motion(motion)
                        .paint_at(ui, vial);
                }
                _ => ActivityBar::new(Some(0.0))
                    .fill(self.fill)
                    .track(self.track)
                    .paint_at(ui, vial),
            }
        }
    }
}

impl Widget for StepTrack {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = ui.available_size_before_wrap().x.max(96.0);
        let (rect, response) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::hover());
        response.widget_info(|| {
            let mut info = WidgetInfo::new(WidgetType::ProgressIndicator);
            info.value = Some((f64::from(self.overall()) * 100.0).floor());
            info
        });
        self.paint_at(ui, rect);
        response
    }
}

/// Width of each of `count` segments across `width`, or `None` when they
/// would be too many or too thin to tell apart.
fn segment_width(width: f32, count: usize) -> Option<f32> {
    if count == 0 || count > MAX_SEGMENTS {
        return None;
    }
    let segment = (width - SEGMENT_GAP * (count - 1) as f32) / count as f32;
    (segment >= MIN_SEGMENT_WIDTH).then_some(segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_share_the_width_with_gaps_between_them() {
        let width = segment_width(300.0, 4).unwrap();
        assert!((width * 4.0 + SEGMENT_GAP * 3.0 - 300.0).abs() < 1e-4);
        assert_eq!(segment_width(300.0, 1), Some(300.0));
    }

    #[test]
    fn too_many_or_too_thin_segments_merge_into_one_bar() {
        assert_eq!(segment_width(300.0, MAX_SEGMENTS + 1), None);
        assert_eq!(segment_width(60.0, 10), None);
        assert_eq!(segment_width(60.0, 0), None);
    }

    #[test]
    fn overall_progress_counts_the_running_step() {
        let track = StepTrack::new(2, 4, Color32::WHITE, Color32::BLACK);
        assert_eq!(track.overall(), 0.5);
        assert_eq!(
            StepTrack::new(2, 4, Color32::WHITE, Color32::BLACK)
                .running(Some(0.5), BarMotion::Forward)
                .overall(),
            0.625
        );
        assert_eq!(
            StepTrack::new(9, 4, Color32::WHITE, Color32::BLACK).overall(),
            1.0
        );
        assert_eq!(
            StepTrack::new(0, 0, Color32::WHITE, Color32::BLACK).overall(),
            0.0
        );
    }
}
