//! Lazily-built, frame-persistent text galleys for scrollable list rows.
//!
//! egui's built-in galley cache only retains galleys laid out in the current
//! frame, so scrolling re-shapes every row that scrolls into view. These helpers
//! shape each row's text once, store the `Arc<Galley>` in a per-row slot owned by
//! the view's cache, and recolor it at paint time so row state changes never
//! force a re-shape. Used by the addon list views and the editor-mission list.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{
    Align, Align2, Color32, FontId, Galley, Pos2, Rect, Ui,
    text::{LayoutJob, TextFormat, TextWrapping},
};

/// Reuse, or lay out once, a single-line unwrapped galley.
///
/// The galley is laid out with [`Color32::PLACEHOLDER`] and recolored at paint
/// time, so it survives color/state changes. The text closure only runs on a
/// cache miss, so per-row `format!`/`tr` work is skipped on the common hit path.
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

/// Like [`lazy_galley`] but bakes `color` into the galley so it can be handed to
/// a `Label`/`Button` (egui paints a provided galley with its own colors,
/// keeping every non-placeholder glyph). Use this when a row must stay a real
/// interactive widget (selectable text, button click) rather than being painted
/// directly. Fold `color` into the cache fingerprint so a theme change rebuilds.
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

/// Fold layout inputs into the `u64` fingerprint expected by
/// [`crate::ui::app::ListGalleyCache::ensure`]. Pass a tuple of `Hash` values;
/// `f32` inputs (font sizes, wrap widths) must be passed as `value.to_bits()`
/// and colors as `color.to_array()`.
pub(crate) fn fingerprint(values: impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    values.hash(&mut hasher);
    hasher.finish()
}

/// Reuse, or lay out once, a single-line galley truncated with an ellipsis to
/// `max_width` - used for the faint path/origin overlay behind an addon name.
/// Matches what `Label::truncate()` would produce for the same text and width.
pub(crate) fn truncated_galley(
    ui: &Ui,
    slot: &mut Option<Arc<Galley>>,
    font_id: FontId,
    max_width: f32,
    text: impl FnOnce() -> String,
) -> Arc<Galley> {
    if let Some(galley) = slot {
        return galley.clone();
    }
    let mut job = LayoutJob::single_section(
        text(),
        TextFormat {
            font_id,
            color: Color32::PLACEHOLDER,
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

/// Paint a galley centered on both axes within `rect`, recoloring placeholder
/// glyphs with `color`.
pub(crate) fn paint_centered(ui: &Ui, rect: Rect, galley: Arc<Galley>, color: Color32) {
    let size = galley.size();
    let pos = Pos2::new(
        rect.center().x - size.x / 2.0,
        rect.center().y - size.y / 2.0,
    );
    ui.painter().galley(pos, galley, color);
}

/// Paint a galley left-aligned and vertically centered within `rect`, clipped to
/// `rect`, recoloring placeholder glyphs with `color`. Returns the painted galley
/// width so callers can place a following overlay (e.g. the origin label).
pub(crate) fn paint_overlay_left(ui: &Ui, rect: Rect, galley: Arc<Galley>, color: Color32) -> f32 {
    let width = galley.size().x;
    let pos = Pos2::new(rect.left(), rect.center().y - galley.size().y / 2.0);
    ui.painter().with_clip_rect(rect).galley(pos, galley, color);
    width
}

/// Paint a galley anchored at `pos` per `anchor`, the cached-galley equivalent of
/// [`eframe::egui::Painter::text`], optionally clipped to `clip`.
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
