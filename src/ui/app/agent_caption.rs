use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Id, LayerId, Order, Rect, StrokeKind, TextStyle, pos2, vec2};

use crate::ui::palette::AGENT_CAPTION_BACKDROP;

const FADE: Duration = Duration::from_millis(250);
const MAX_TEXT_WIDTH: f32 = 520.0;
const PADDING: egui::Vec2 = vec2(22.0, 16.0);
const LINE_GAP: f32 = 6.0;

/// A scene title an agent GUI scenario shows over the app before a scene, so
/// a person watching knows what comes next. Scenario text is English only and
/// never translated.
#[derive(Debug)]
pub(crate) struct AgentCaption {
    pub(crate) title: String,
    pub(crate) detail: Option<String>,
    pub(crate) shown_at: Instant,
    pub(crate) until: Instant,
}

impl AgentCaption {
    /// Paints the caption, or returns `false` once its time is up.
    pub(crate) fn paint(&self, ctx: &egui::Context, now: Instant) -> bool {
        if now >= self.until {
            return false;
        }
        let (elapsed, remaining) = (now - self.shown_at, self.until - now);
        let opacity = fade_opacity(elapsed, remaining);
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("agent_caption")));
        let screen = ctx.content_rect();
        painter.rect_filled(screen, 0.0, AGENT_CAPTION_BACKDROP.gamma_multiply(opacity));

        let style = ctx.global_style();
        let visuals = &style.visuals;
        let title = painter.layout(
            self.title.clone(),
            TextStyle::Heading.resolve(&style),
            Color32::PLACEHOLDER,
            MAX_TEXT_WIDTH,
        );
        let detail = self.detail.clone().map(|detail| {
            painter.layout(
                detail,
                TextStyle::Body.resolve(&style),
                Color32::PLACEHOLDER,
                MAX_TEXT_WIDTH,
            )
        });
        let text_size = match &detail {
            Some(detail) => vec2(
                title.size().x.max(detail.size().x),
                title.size().y + LINE_GAP + detail.size().y,
            ),
            None => title.size(),
        };
        let card = Rect::from_center_size(screen.center(), text_size + 2.0 * PADDING);
        painter.rect(
            card,
            visuals.window_corner_radius,
            visuals.window_fill().gamma_multiply(opacity),
            egui::Stroke::new(
                visuals.window_stroke.width,
                visuals.window_stroke.color.gamma_multiply(opacity),
            ),
            StrokeKind::Inside,
        );
        let top = card.top() + PADDING.y;
        let title_top = pos2(card.center().x - title.size().x / 2.0, top);
        let detail_top = top + title.size().y + LINE_GAP;
        painter.galley(
            title_top,
            title,
            visuals.strong_text_color().gamma_multiply(opacity),
        );
        if let Some(detail) = detail {
            painter.galley(
                pos2(card.center().x - detail.size().x / 2.0, detail_top),
                detail,
                visuals.text_color().gamma_multiply(opacity),
            );
        }

        let next = if elapsed < FADE || remaining < FADE {
            Duration::from_millis(16)
        } else {
            remaining - FADE
        };
        ctx.request_repaint_after(next);
        true
    }
}

/// Fades in over the first `FADE` and out over the last.
fn fade_opacity(elapsed: Duration, remaining: Duration) -> f32 {
    let ramp = |span: Duration| (span.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0);
    ramp(elapsed).min(ramp(remaining))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caption_fades_in_holds_and_fades_out() {
        let ms = Duration::from_millis;
        assert_eq!(fade_opacity(ms(0), ms(3000)), 0.0);
        assert!((fade_opacity(ms(125), ms(3000)) - 0.5).abs() < 0.01);
        assert_eq!(fade_opacity(ms(1000), ms(2000)), 1.0);
        assert!((fade_opacity(ms(3000), ms(125)) - 0.5).abs() < 0.01);
        assert_eq!(fade_opacity(ms(3000), ms(0)), 0.0);
    }
}
