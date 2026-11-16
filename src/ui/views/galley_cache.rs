use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{
    Align, Align2, Color32, FontId, Galley, Pos2, Rect, Ui,
    text::{LayoutJob, TextFormat, TextWrapping},
};

pub(crate) fn lazy_galley(
    ui: &Ui,
    slot: &mut Option<Arc<Galley>>,
    font_id: FontId,
    text: impl FnOnce() -> String,
) -> Arc<Galley> {
    if let Some(galley) = slot {
        return galley.clone();
    }
    let galley = ui
        .painter()
        .layout_no_wrap(text(), font_id, Color32::PLACEHOLDER);
    *slot = Some(galley.clone());
    galley
}

pub(crate) fn lazy_galley_colored(
    ui: &Ui,
    slot: &mut Option<Arc<Galley>>,
    font_id: FontId,
    color: Color32,
    text: impl FnOnce() -> String,
) -> Arc<Galley> {
    if let Some(galley) = slot {
        return galley.clone();
    }
    let galley = ui.painter().layout_no_wrap(text(), font_id, color);
    *slot = Some(galley.clone());
    galley
}

pub(crate) fn fingerprint(values: impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    values.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn truncated_galley(
    ui: &Ui,
    slot: &mut Option<Arc<Galley>>,
    font_id: FontId,
    max_width: f32,
    text: impl FnOnce() -> String,
) -> Arc<Galley> {
    lazy_galley_truncated(ui, slot, font_id, Color32::PLACEHOLDER, max_width, text)
}

pub(crate) fn lazy_galley_truncated(
    ui: &Ui,
    slot: &mut Option<Arc<Galley>>,
    font_id: FontId,
    color: Color32,
    max_width: f32,
    text: impl FnOnce() -> String,
) -> Arc<Galley> {
    if let Some(galley) = slot
        && galley.job.wrap.max_width.to_bits() == max_width.to_bits()
    {
        return galley.clone();
    }
    let mut job = LayoutJob::single_section(
        text(),
        TextFormat {
            font_id,
            color,
            valign: Align::Center,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job.halign = Align::LEFT;
    job.justify = false;
    let galley = ui.painter().layout_job(job);
    *slot = Some(galley.clone());
    galley
}

pub(crate) fn paint_centered(ui: &Ui, rect: Rect, galley: Arc<Galley>, color: Color32) {
    let size = galley.size();
    let pos = Pos2::new(
        rect.center().x - size.x / 2.0,
        rect.center().y - size.y / 2.0,
    );
    ui.painter().galley(pos, galley, color);
}

pub(crate) fn paint_overlay_left(ui: &Ui, rect: Rect, galley: Arc<Galley>, color: Color32) -> f32 {
    let width = galley.size().x;
    let pos = Pos2::new(rect.left(), rect.center().y - galley.size().y / 2.0);
    ui.painter().with_clip_rect(rect).galley(pos, galley, color);
    width
}

pub(crate) fn paint_anchored(
    ui: &Ui,
    pos: Pos2,
    anchor: Align2,
    galley: Arc<Galley>,
    color: Color32,
    clip: Option<Rect>,
) {
    let rect = anchor.anchor_size(pos, galley.size());
    match clip {
        Some(clip) => ui
            .painter()
            .with_clip_rect(clip)
            .galley(rect.min, galley, color),
        None => ui.painter().galley(rect.min, galley, color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_truncation_width_reuses_the_cached_layout() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut slot = None;
            let first = truncated_galley(ui, &mut slot, FontId::proportional(20.0), 120.0, || {
                "Repository with a long label".to_owned()
            });
            let cached = truncated_galley(ui, &mut slot, FontId::proportional(20.0), 120.0, || {
                panic!("unchanged layout should stay cached")
            });
            assert!(Arc::ptr_eq(&first, &cached));
        });
        output.textures_delta.clear();
    }

    #[test]
    fn changed_truncation_width_reflows_without_a_content_generation_change() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut slot = None;
            let mut previous = None;
            for width in [200.0_f32, 80.0, 240.0] {
                let galley =
                    truncated_galley(ui, &mut slot, FontId::proportional(20.0), width, || {
                        "WWWWWWWWWWWWWWWWWWWW".to_owned()
                    });
                assert!(galley.size().x <= width);
                assert_eq!(galley.job.wrap.max_width.to_bits(), width.to_bits());
                if let Some(previous) = previous {
                    assert!(!Arc::ptr_eq(&previous, &galley));
                }
                previous = Some(galley);
            }
        });
        output.textures_delta.clear();
    }
}
