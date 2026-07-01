use super::ExternalAddonContextAction;
use crate::ui::app::{
    AddonDestructiveConfirmAction, ExternalAddonRowCache, Foxy, RepositoryExternalAddonsListCache,
};
use crate::ui::context_menu::{ContextMenuItem, attach_context_menu};
use crate::ui::i18n::{fmt_bytes, tr, tr_fmt};
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align, Align2, AsIdSalt, Button, Color32, CornerRadius, FontId, Layout, RichText,
    ScrollArea, TextEdit, Ui, Vec2,
};
use log::warn;
use std::ops::Range;

const EXTERNAL_ADDON_ROW_HEIGHT: f32 = 40.0;
const EXTERNAL_ADDON_WRAPPED_ROW_HEIGHT: f32 = 58.0;
const EXTERNAL_ADDON_INNER_MARGIN: f32 = 5.0;
const EXTERNAL_ADDON_ICON_BUTTON_SIZE: f32 = 28.0;
const EXTERNAL_ADDON_ACTION_GAP: f32 = 4.0;
const EXTERNAL_ADDON_ACTION_WIDTH: f32 =
    EXTERNAL_ADDON_ICON_BUTTON_SIZE * 3.0 + EXTERNAL_ADDON_ACTION_GAP;
const MIN_EXTERNAL_INLINE_SIDE_WIDTH: f32 = 180.0;
const MAX_EXTERNAL_OVERLAY_TEXT_CHARS: usize = 180;
const EXTERNAL_GALLEY_PREWARM_ROWS_PER_FRAME: usize = 128;

#[derive(Debug, PartialEq, Eq)]
enum ExternalAddonGroupedRow {
    Header {
        row_slot: usize,
        origin: String,
        collapsed: bool,
    },
    Addon {
        row_slot: usize,
        entry_index: usize,
    },
}

#[derive(Clone)]
struct ExternalAddonRowStyle {
    enabled_card_fill: Color32,
    disabled_card_fill: Color32,
    color_text_normal: Color32,
    color_text_gray: Color32,
    color_text_dim: Color32,
    name_font_id: FontId,
    path_font_id: FontId,
}

struct ExternalAddonRowTooltips {
    add_favorite: String,
    remove_favorite: String,
    mark_client_side: String,
    remove_client_side: String,
    forced_client_side: String,
}

struct ExternalAddonIconCellStyle {
    fill: Color32,
    stroke: Color32,
    text_color: Color32,
}

fn grouped_external_addon_row_count(cache: &RepositoryExternalAddonsListCache) -> usize {
    cache
        .grouped_filtered_indices
        .iter()
        .map(|(origin, entry_indices)| {
            1 + if cache.collapsed_origins.contains(origin) {
                0
            } else {
                entry_indices.len()
            }
        })
        .sum()
}

fn visible_grouped_external_addon_rows(
    cache: &RepositoryExternalAddonsListCache,
    row_range: Range<usize>,
) -> Vec<ExternalAddonGroupedRow> {
    let mut rows = Vec::with_capacity(row_range.len());
    let mut row_cursor = 0;

    for (origin, entry_indices) in &cache.grouped_filtered_indices {
        let collapsed = cache.collapsed_origins.contains(origin);
        let group_len = 1 + if collapsed { 0 } else { entry_indices.len() };
        let group_start = row_cursor;
        let group_end = group_start + group_len;
        row_cursor = group_end;

        if group_end <= row_range.start {
            continue;
        }
        if group_start >= row_range.end {
            break;
        }

        if row_range.contains(&group_start) {
            rows.push(ExternalAddonGroupedRow::Header {
                row_slot: group_start - row_range.start,
                origin: origin.clone(),
                collapsed,
            });
        }

        if collapsed {
            continue;
        }

        let entries_start_row = group_start + 1;
        let visible_start = row_range.start.max(entries_start_row);
        let visible_end = row_range.end.min(group_end);
        if visible_start >= visible_end {
            continue;
        }

        let entry_start = visible_start - entries_start_row;
        let entry_end = visible_end - entries_start_row;
        rows.extend(
            entry_indices[entry_start..entry_end]
                .iter()
                .enumerate()
                .map(|(offset, entry_index)| ExternalAddonGroupedRow::Addon {
                    row_slot: visible_start + offset - row_range.start,
                    entry_index: *entry_index,
                }),
        );
    }

    rows
}

fn compact_external_overlay_text(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= MAX_EXTERNAL_OVERLAY_TEXT_CHARS {
        return text.to_string();
    }

    let tail_chars = MAX_EXTERNAL_OVERLAY_TEXT_CHARS.saturating_sub(3);
    let mut tail = text.chars().rev().take(tail_chars).collect::<Vec<_>>();
    tail.reverse();

    let mut compact = String::with_capacity(MAX_EXTERNAL_OVERLAY_TEXT_CHARS);
    compact.push_str("...");
    compact.extend(tail);
    compact
}

fn external_text_area_width(available_width: f32, horizontal_padding: f32) -> f32 {
    let side_padding = external_row_side_padding(available_width, horizontal_padding);
    let card_width = (available_width - 2.0 * side_padding).max(0.0);
    let inner_width = (card_width - 2.0 * EXTERNAL_ADDON_INNER_MARGIN).max(0.0);
    (inner_width - EXTERNAL_ADDON_ACTION_WIDTH).max(0.0)
}

fn external_row_side_padding(available_width: f32, preferred_padding: f32) -> f32 {
    preferred_padding
        .min((available_width * 0.5 - 1.0).max(0.0))
        .round()
}

fn external_addon_display_name(row: &ExternalAddonRowCache) -> String {
    match row.local_size_bytes.filter(|size| *size > 0) {
        Some(size_bytes) => format!("{}  ({})", row.addon_name.as_str(), fmt_bytes(size_bytes)),
        None => row.addon_name.clone(),
    }
}

fn external_centered_side_width(text_area_width: f32, name_width: f32, text_spacing: f32) -> f32 {
    ((text_area_width - name_width) * 0.5 - text_spacing)
        .max(0.0)
        .round()
}

fn external_metadata_lane_widths(
    text_area_width: f32,
    name_width: f32,
    text_spacing: f32,
    include_origin: bool,
    wrap_name: bool,
) -> (f32, f32) {
    if wrap_name && include_origin {
        let lane_width = ((text_area_width - text_spacing) * 0.5).max(0.0).round();
        (lane_width, lane_width)
    } else if wrap_name {
        (text_area_width.round(), 0.0)
    } else if include_origin {
        let side_width = external_centered_side_width(text_area_width, name_width, text_spacing);
        (side_width, side_width)
    } else {
        let name_left = (text_area_width - name_width) * 0.5;
        (name_left.max(0.0).round(), 0.0)
    }
}

fn external_addon_name_wraps(
    text_area_width: f32,
    name_width: f32,
    required_side_width: f32,
    text_spacing: f32,
    include_origin: bool,
) -> bool {
    if text_area_width <= 0.0 || name_width <= 0.0 {
        return false;
    }

    if name_width > text_area_width {
        return true;
    }

    include_origin
        && external_centered_side_width(text_area_width, name_width, text_spacing)
            < required_side_width.max(MIN_EXTERNAL_INLINE_SIDE_WIDTH)
}

fn external_addon_row_height(
    text_area_width: f32,
    max_name_width: f32,
    max_required_side_width: f32,
    text_spacing: f32,
    include_origin: bool,
) -> f32 {
    if external_addon_name_wraps(
        text_area_width,
        max_name_width,
        max_required_side_width,
        text_spacing,
        include_origin,
    ) {
        EXTERNAL_ADDON_WRAPPED_ROW_HEIGHT
    } else {
        EXTERNAL_ADDON_ROW_HEIGHT
    }
}

fn paint_external_addon_icon_cell(
    ui: &mut Ui,
    rect: egui::Rect,
    id_source: impl AsIdSalt,
    label: &str,
    font_id: FontId,
    style: ExternalAddonIconCellStyle,
) -> egui::Response {
    let response = ui.interact(rect, ui.make_persistent_id(id_source), egui::Sense::click());
    let fill = if response.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        style.fill
    };
    let stroke = if response.hovered() {
        ui.visuals().widgets.hovered.bg_stroke.color
    } else {
        style.stroke
    };
    ui.painter().rect_filled(rect, CornerRadius::same(3), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(3),
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font_id,
        style.text_color,
    );
    response
}

impl Foxy {
    fn ensure_repository_external_addon_layout_widths(
        ui: &Ui,
        cache: &mut RepositoryExternalAddonsListCache,
        row_style: &ExternalAddonRowStyle,
        include_origin: bool,
    ) -> (f32, f32) {
        let mut max_name_width: f32 = 0.0;
        let mut max_required_side_width: f32 = 0.0;

        for filtered_index in 0..cache.filtered_indices.len() {
            let entry_index = cache.filtered_indices[filtered_index];
            let row = &cache.rows[entry_index];
            let name_text =
                (!cache.galleys.has_name(entry_index)).then(|| external_addon_display_name(row));
            let name_galley = galley_cache::lazy_galley(
                ui,
                cache.galleys.name_slot(entry_index),
                row_style.name_font_id.clone(),
                || name_text.expect("missing external addon name galley text"),
            );
            max_name_width = max_name_width.max(name_galley.size().x);

            if include_origin {
                let path_width = ui
                    .painter()
                    .layout_no_wrap(
                        compact_external_overlay_text(&row.path),
                        row_style.path_font_id.clone(),
                        Color32::PLACEHOLDER,
                    )
                    .size()
                    .x;
                let origin_width = ui
                    .painter()
                    .layout_no_wrap(
                        tr_fmt("Origin: {origin}", &[("origin", tr(&row.origin))]),
                        row_style.path_font_id.clone(),
                        Color32::PLACEHOLDER,
                    )
                    .size()
                    .x;
                max_required_side_width = max_required_side_width.max(path_width.max(origin_width));
            }
        }

        (max_name_width, max_required_side_width)
    }

    fn prewarm_repository_external_addon_galleys(
        &mut self,
        ui: &Ui,
        row_style: &ExternalAddonRowStyle,
        text_area_width: f32,
        row_height: f32,
        include_origin: bool,
    ) {
        if text_area_width <= 0.0 {
            return;
        }

        let filtered_len = self
            .repository_external_addons_list_cache
            .filtered_indices
            .len();
        if filtered_len == 0 {
            return;
        }

        {
            let cache = &mut self.repository_external_addons_list_cache;
            let needs_origin_prewarm = include_origin && !cache.galley_prewarm_include_origin;
            if cache.galley_prewarm_path_width != Some(text_area_width) || needs_origin_prewarm {
                cache.galley_prewarm_path_width = Some(text_area_width);
                cache.galley_prewarm_include_origin = include_origin;
                cache.galley_prewarm_cursor = 0;
            }
        }

        let start = self
            .repository_external_addons_list_cache
            .galley_prewarm_cursor
            .min(filtered_len);
        if start >= filtered_len {
            return;
        }

        let end = (start + EXTERNAL_GALLEY_PREWARM_ROWS_PER_FRAME).min(filtered_len);
        for filtered_index in start..end {
            let entry_index =
                self.repository_external_addons_list_cache.filtered_indices[filtered_index];
            Self::prewarm_repository_external_addon_row_galleys(
                ui,
                &mut self.repository_external_addons_list_cache,
                entry_index,
                row_style,
                text_area_width,
                row_height,
                include_origin,
                ui.spacing().item_spacing.x,
            );
        }

        self.repository_external_addons_list_cache
            .galley_prewarm_cursor = end;
        if end < filtered_len {
            self.needs_repaint = true;
            ui.ctx().request_repaint();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prewarm_repository_external_addon_row_galleys(
        ui: &Ui,
        cache: &mut RepositoryExternalAddonsListCache,
        entry_index: usize,
        row_style: &ExternalAddonRowStyle,
        text_area_width: f32,
        row_height: f32,
        include_origin: bool,
        spacing: f32,
    ) {
        let name_text = (!cache.galleys.has_name(entry_index)).then(|| {
            let row = &cache.rows[entry_index];
            external_addon_display_name(row)
        });
        let name_galley = galley_cache::lazy_galley(
            ui,
            cache.galleys.name_slot(entry_index),
            row_style.name_font_id.clone(),
            || name_text.expect("missing external addon name prewarm text"),
        );
        let wrap_name = row_height > EXTERNAL_ADDON_ROW_HEIGHT;
        let (path_width, origin_width) = external_metadata_lane_widths(
            text_area_width,
            name_galley.size().x,
            spacing,
            include_origin,
            wrap_name,
        );

        let path_text = (!cache.galleys.has_path_for_width(entry_index, path_width))
            .then(|| compact_external_overlay_text(&cache.rows[entry_index].path));
        cache.galleys.ensure_path_width(path_width);
        galley_cache::truncated_galley(
            ui,
            cache.galleys.path_slot(entry_index),
            row_style.path_font_id.clone(),
            path_width,
            || path_text.expect("missing external addon path prewarm text"),
        );

        if include_origin && origin_width > 0.0 {
            let origin_text = (!cache
                .galleys
                .has_origin_for_width(entry_index, origin_width))
            .then(|| {
                let origin = &cache.rows[entry_index].origin;
                tr_fmt("Origin: {origin}", &[("origin", tr(origin))])
            });
            galley_cache::truncated_galley(
                ui,
                cache
                    .galleys
                    .origin_slot_for_width(entry_index, origin_width),
                row_style.path_font_id.clone(),
                origin_width,
                || origin_text.expect("missing external addon origin prewarm text"),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_repository_external_addon_row_cached(
        &mut self,
        ui: &mut Ui,
        entry_index: usize,
        _row_slot: usize,
        group_by_origin: bool,
        horizontal_padding: f32,
        row_height: f32,
        text_area_width: f32,
        row_style: &ExternalAddonRowStyle,
        repo_data_changed: &mut bool,
        external_addon_context_action: &mut Option<(String, String, ExternalAddonContextAction)>,
        context_items: &[ContextMenuItem<ExternalAddonContextAction>],
        tooltips: &ExternalAddonRowTooltips,
    ) {
        let (is_enabled, favorite, client_side, forced_client_side) = {
            let cache = &self.repository_external_addons_list_cache;
            (
                cache.enabled_by_row[entry_index],
                cache.favorite_by_row[entry_index],
                cache.client_side_by_row[entry_index],
                cache.forced_client_side_by_row[entry_index],
            )
        };

        let row_rect =
            egui::Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), row_height));
        ui.advance_cursor_after_rect(row_rect);
        let side_padding = external_row_side_padding(row_rect.width(), horizontal_padding);
        let card_rect = egui::Rect::from_min_max(
            egui::pos2(row_rect.left() + side_padding, row_rect.top() + 1.0),
            egui::pos2(row_rect.right() - side_padding, row_rect.bottom() - 1.0),
        );
        let inner_rect = card_rect.shrink(EXTERNAL_ADDON_INNER_MARGIN);
        let button_top = inner_rect.center().y - EXTERNAL_ADDON_ICON_BUTTON_SIZE * 0.5;
        let checkbox_rect = egui::Rect::from_min_size(
            egui::pos2(
                inner_rect.right() - EXTERNAL_ADDON_ICON_BUTTON_SIZE,
                button_top,
            ),
            Vec2::splat(EXTERNAL_ADDON_ICON_BUTTON_SIZE),
        );
        let client_side_button_rect = egui::Rect::from_min_size(
            egui::pos2(
                checkbox_rect.left() - EXTERNAL_ADDON_ICON_BUTTON_SIZE,
                button_top,
            ),
            Vec2::splat(EXTERNAL_ADDON_ICON_BUTTON_SIZE),
        );
        let favorite_button_rect = egui::Rect::from_min_size(
            egui::pos2(
                client_side_button_rect.left() - EXTERNAL_ADDON_ICON_BUTTON_SIZE,
                button_top,
            ),
            Vec2::splat(EXTERNAL_ADDON_ICON_BUTTON_SIZE),
        );
        let text_rect = egui::Rect::from_min_max(
            inner_rect.min,
            egui::pos2(
                (favorite_button_rect.left() - EXTERNAL_ADDON_ACTION_GAP).max(inner_rect.left()),
                inner_rect.bottom(),
            ),
        );
        let wrap_name = row_height > EXTERNAL_ADDON_ROW_HEIGHT;
        let (metadata_rect, name_rect) = if wrap_name {
            let split_y = inner_rect.center().y;
            (
                egui::Rect::from_min_max(text_rect.min, egui::pos2(text_rect.right(), split_y)),
                egui::Rect::from_min_max(egui::pos2(text_rect.left(), split_y), text_rect.max),
            )
        } else {
            (text_rect, text_rect)
        };

        let card_fill = if is_enabled {
            row_style.enabled_card_fill
        } else {
            row_style.disabled_card_fill
        };
        let text_color = if is_enabled {
            row_style.color_text_normal
        } else {
            row_style.color_text_gray
        };
        let path_color = if is_enabled {
            row_style.color_text_gray
        } else {
            row_style.color_text_dim
        };

        // Key the row by the stable addon index, not the visible-slot position
        // (`row_slot`): a position-based id changes for a given addon as the
        // scroll range shifts, which makes egui re-run extra layout passes
        // ("changed id between passes") and multiplies the per-frame scroll cost.
        let row_id = ui.make_persistent_id((
            "repository_external_addon_row_cached",
            group_by_origin,
            entry_index,
        ));
        let context_response = ui.interact(card_rect, row_id, egui::Sense::click());
        let card_fill = if context_response.hovered() {
            self.color_widget_bg_active()
        } else {
            card_fill
        };
        let card_stroke = if context_response.hovered() {
            self.color_primary_accent_hover()
        } else {
            row_style.color_text_gray
        };
        ui.painter()
            .rect_filled(card_rect, CornerRadius::same(5), card_fill);
        ui.painter().rect_stroke(
            card_rect,
            CornerRadius::same(5),
            egui::Stroke::new(1.0, card_stroke),
            egui::StrokeKind::Inside,
        );

        let name_text = {
            let cache = &self.repository_external_addons_list_cache;
            (!cache.galleys.has_name(entry_index)).then(|| {
                let row = &cache.rows[entry_index];
                external_addon_display_name(row)
            })
        };
        let name_galley = {
            let cache = &mut self.repository_external_addons_list_cache;
            galley_cache::lazy_galley(
                ui,
                cache.galleys.name_slot(entry_index),
                row_style.name_font_id.clone(),
                || name_text.expect("missing external addon name galley text"),
            )
        };
        // `text_area_width` is computed once outside the ScrollArea and passed
        // in, so the prewarm and render choose the same row text widths. Re-
        // computing from the scrollbar-adjusted inner width would diverge from
        // prewarm and force a re-shape on every reveal.
        let (path_width, origin_width) = external_metadata_lane_widths(
            text_area_width,
            name_galley.size().x,
            ui.spacing().item_spacing.x,
            !group_by_origin,
            wrap_name,
        );
        // The normal row is split into path | centered name | origin. When that
        // leaves too little side space, the row grows and the name uses a second
        // line while path/origin share the top line.
        let mut path_rect = metadata_rect;
        path_rect.set_width(path_width.min(metadata_rect.width()));
        let path_text = {
            let cache = &self.repository_external_addons_list_cache;
            (!cache.galleys.has_path_for_width(entry_index, path_width))
                .then(|| compact_external_overlay_text(&cache.rows[entry_index].path))
        };
        let path_galley = {
            let cache = &mut self.repository_external_addons_list_cache;
            cache.galleys.ensure_path_width(path_width);
            galley_cache::truncated_galley(
                ui,
                cache.galleys.path_slot(entry_index),
                row_style.path_font_id.clone(),
                path_width,
                || path_text.expect("missing external addon path galley text"),
            )
        };
        galley_cache::paint_overlay_left(ui, path_rect, path_galley, path_color);

        if !group_by_origin && origin_width > 0.0 {
            let origin_text = {
                let cache = &self.repository_external_addons_list_cache;
                (!cache
                    .galleys
                    .has_origin_for_width(entry_index, origin_width))
                .then(|| {
                    let origin = &cache.rows[entry_index].origin;
                    tr_fmt("Origin: {origin}", &[("origin", tr(origin))])
                })
            };
            let origin_galley = {
                let cache = &mut self.repository_external_addons_list_cache;
                galley_cache::truncated_galley(
                    ui,
                    cache
                        .galleys
                        .origin_slot_for_width(entry_index, origin_width),
                    row_style.path_font_id.clone(),
                    origin_width,
                    || origin_text.expect("missing external addon origin galley text"),
                )
            };
            let origin_rect = egui::Rect::from_min_max(
                egui::Pos2::new(
                    (metadata_rect.right() - origin_width).max(metadata_rect.left()),
                    metadata_rect.top(),
                ),
                metadata_rect.max,
            );
            galley_cache::paint_anchored(
                ui,
                egui::pos2(origin_rect.right(), origin_rect.center().y),
                Align2::RIGHT_CENTER,
                origin_galley,
                row_style.color_text_dim,
                Some(origin_rect),
            );
        }

        let inline_name_rect = if wrap_name {
            name_rect
        } else {
            let name_width = name_galley.size().x.min(name_rect.width());
            egui::Rect::from_center_size(
                name_rect.center(),
                egui::vec2(name_width, name_rect.height()),
            )
        };
        galley_cache::paint_anchored(
            ui,
            inline_name_rect.center(),
            Align2::CENTER_CENTER,
            name_galley,
            text_color,
            Some(inline_name_rect),
        );

        let favorite_button = paint_external_addon_icon_cell(
            ui,
            favorite_button_rect,
            ("repository_external_addon_favorite", entry_index),
            "\u{2605}",
            row_style.name_font_id.clone(),
            ExternalAddonIconCellStyle {
                fill: self.color_main_bg(),
                stroke: if favorite {
                    self.color_primary_accent()
                } else {
                    row_style.color_text_gray
                },
                text_color: if favorite {
                    self.color_primary_accent()
                } else if is_enabled {
                    row_style.color_text_normal
                } else {
                    row_style.color_text_gray
                },
            },
        );
        if favorite_button.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        let favorite_button = favorite_button.on_hover_text(if favorite {
            tooltips.remove_favorite.as_str()
        } else {
            tooltips.add_favorite.as_str()
        });
        let favorite_clicked = favorite_button.clicked();
        if favorite_clicked
            && self.set_repository_external_addon_row_favorite_cached(entry_index, !favorite)
            && let Some(repo_index) = self.repository_external_addons_list_cache.repo_index
        {
            self.persist_repository_external_addon_favorite_state_cached(repo_index);
        }

        let client_side_text_color = if forced_client_side || client_side || is_enabled {
            row_style.color_text_normal
        } else {
            row_style.color_text_gray
        };
        let client_side_button = paint_external_addon_icon_cell(
            ui,
            client_side_button_rect,
            ("repository_external_addon_client_side", entry_index),
            "C",
            row_style.name_font_id.clone(),
            ExternalAddonIconCellStyle {
                fill: if client_side && !forced_client_side {
                    self.color_primary_accent()
                } else if forced_client_side {
                    self.color_widget_bg_active()
                } else {
                    self.color_main_bg()
                },
                stroke: if client_side || forced_client_side {
                    self.color_primary_accent()
                } else {
                    row_style.color_text_gray
                },
                text_color: client_side_text_color,
            },
        );
        if client_side_button.hovered() && !forced_client_side {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        let client_side_button = client_side_button.on_hover_text(if forced_client_side {
            tooltips.forced_client_side.as_str()
        } else if client_side {
            tooltips.remove_client_side.as_str()
        } else {
            tooltips.mark_client_side.as_str()
        });
        let client_side_clicked = client_side_button.clicked();
        if !forced_client_side
            && client_side_clicked
            && self.set_repository_external_addon_row_client_side_cached(entry_index, !client_side)
            && let Some(repo_index) = self.repository_external_addons_list_cache.repo_index
        {
            self.persist_repository_external_addon_client_side_state_cached(repo_index);
        }

        let mut row_enabled = is_enabled;
        let checkbox_response = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .max_rect(checkbox_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| Self::ui_state_checkbox(ui, &mut row_enabled, ""),
            )
            .inner;
        let checkbox_clicked = checkbox_response.clicked();
        if checkbox_response.changed()
            && self.set_repository_external_addon_row_enabled_cached(entry_index, row_enabled)
        {
            *repo_data_changed = true;
        }
        if checkbox_response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }

        if context_response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        if context_response.clicked()
            && !checkbox_clicked
            && !favorite_clicked
            && !client_side_clicked
            && self.set_repository_external_addon_row_enabled_cached(entry_index, !is_enabled)
        {
            *repo_data_changed = true;
        }
        let mut context_action: Option<ExternalAddonContextAction> = None;
        attach_context_menu(&context_response, context_items, &mut context_action);
        if let Some(action) = context_action {
            let cache = &self.repository_external_addons_list_cache;
            let row = &cache.rows[entry_index];
            *external_addon_context_action =
                Some((row.addon_name.clone(), row.path.clone(), action));
        }
    }

    pub(super) fn render_repository_external_addons_list_cached(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        filter: &mut String,
        origin_filter: &mut String,
        group_by_origin: &mut bool,
        addon_state_filter: &mut String,
    ) {
        if repo_index >= self.repository_view_state.repositories.len() {
            return;
        }

        self.ensure_repository_external_addons_base_cache_cached(repo_index);

        let color_text_dim = self.color_text_dim();
        let horizontal_padding = 15.0;
        let mut ui_state_changed = false;
        let mut repo_data_changed = false;
        let mut external_addon_context_action: Option<(
            String,
            String,
            ExternalAddonContextAction,
        )> = None;

        if origin_filter.is_empty() {
            *origin_filter = "All".to_string();
        }

        // Built once per frame and shared by every row: the labels are constant,
        // so rebuilding this array per row only churned allocations and `tr`
        // lookups in the scroll hot path.
        let context_items = [
            ContextMenuItem::new(
                ExternalAddonContextAction::OpenDirectory,
                tr("Open addon directory"),
            ),
            ContextMenuItem::new(ExternalAddonContextAction::Delete, tr("Delete addon"))
                .separator_before()
                .danger(),
        ];
        let row_tooltips = ExternalAddonRowTooltips {
            add_favorite: tr("Add to favorites"),
            remove_favorite: tr("Remove from favorites"),
            mark_client_side: tr("Mark as client-side addon"),
            remove_client_side: tr("Remove from client-side addons"),
            forced_client_side: tr("Client-side addon defined by repository"),
        };

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!(
                    "{} {}",
                    '\u{2139}',
                    tr("Enable or disable external addons for this repository - mods found on disk that are not part of this repo's own addons.\nThey come from your other repositories, additional folders, and Steam Workshop. Steam Workshop mods appear only when \"Include Steam Addons\" is enabled.")
                );
                // Reserve the action buttons on the right first, then let the info
                // text fill the remaining width and wrap to as many lines as it
                // needs - that keeps the buttons fully visible on narrow windows.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(50.0);
                    let refresh_icon_size = self
                        .settings_view_state
                        .font_sizes
                        .repository_settings_view
                        .refresh_icon as f32;
                    let recheck_button = ui.add_sized(
                        Self::toolbar_icon_button_size(refresh_icon_size),
                        Button::new(RichText::new("\u{21bb}").size(refresh_icon_size)),
                    );

                    if recheck_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }

                    if recheck_button.clicked() {
                        self.invalidate_addon_inventory_cache();
                        self.show_success_toast(self.t("External addons rescanned."));
                    }

                    let disable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Disable all")));
                    if disable_all_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if disable_all_button.clicked()
                        && self.set_all_repository_external_addon_rows_enabled_cached(false)
                    {
                        self.persist_repository_external_addon_row_state_cached(repo_index);
                    }

                    ui.add_space(5.0);

                    let enable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Enable all")));
                    if enable_all_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if enable_all_button.clicked()
                        && self.set_all_repository_external_addon_rows_enabled_cached(true)
                    {
                        self.persist_repository_external_addon_row_state_cached(repo_index);
                    }

                    ui.add_space(10.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(info_text).italics().color(color_text_dim),
                        )
                        .wrap(),
                    );
                });
            });
            ui.separator();

            self.ensure_repository_external_addons_base_cache_cached(repo_index);

            let origin_options = self
                .repository_external_addons_list_cache
                .origin_options
                .clone();

            // Filter field + trailing controls share one wrapping row. On wide
            // windows the filter expands so the controls sit toward the right
            // edge; when the window is too narrow the controls collapse onto their
            // own line(s) below instead of being clipped off the edge.
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Filter:"));
                self.filter_help_icon(ui, &tr("addon_filter_help"));
                ui.add_space(horizontal_padding);

                let origin_selected = if origin_filter == "All" {
                    tr("All origins")
                } else {
                    tr(origin_filter)
                };
                let state_selected = match addon_state_filter.as_str() {
                    "Enabled" => tr("Enabled"),
                    "Disabled" => tr("Disabled"),
                    _ => tr("All"),
                };
                let group_gap = 16.0;
                let item_spacing = ui.spacing().item_spacing.x;
                let controls_width = super::filter_controls_checkbox_width(
                    ui,
                    &tr("Include Steam Addons"),
                ) + super::filter_controls_text_width(ui, &tr("Origin:"))
                    + super::filter_controls_combo_width(ui, &origin_selected)
                    + super::filter_controls_checkbox_width(ui, &tr("Group by origin"))
                    + super::filter_controls_checkbox_width(ui, &tr("Favorites only"))
                    + super::filter_controls_checkbox_width(ui, &tr("Client-side only"))
                    + super::filter_controls_text_width(ui, &tr("State:"))
                    + super::filter_controls_combo_width(ui, &state_selected)
                    // The explicit add_space() gaps (six 16 px group gaps and the
                    // two 6 px label→combo gaps) plus the item spacing egui inserts
                    // between each of the ~16 items in this region. Counting them
                    // in full keeps the filter from being sized too wide, which
                    // would otherwise shove the trailing controls off the edge.
                    + group_gap * 6.0
                    + 12.0
                    + item_spacing * 16.0;

                let filter_width =
                    super::responsive_filter_field_width(ui.available_width(), controls_width);
                let filter_edit =
                    ui.add(TextEdit::singleline(filter).desired_width(filter_width));
                if filter_edit.changed() {
                    ui_state_changed = true;
                }
                if filter_edit.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(group_gap);

                let mut include_steam_addons = self
                    .current_repository_include_steam_addons_cached(repo_index)
                    .unwrap_or(false);
                let include_steam_addons_checkbox =
                    ui.checkbox(&mut include_steam_addons, tr("Include Steam Addons"));
                if include_steam_addons_checkbox.changed()
                    && self.set_current_repository_include_steam_addons_cached(
                        repo_index,
                        include_steam_addons,
                    )
                {
                    repo_data_changed = true;
                    self.ensure_repository_external_addons_base_cache_cached(repo_index);
                }
                if include_steam_addons_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                ui.label(tr("Origin:"));
                ui.add_space(6.0);
                let origin_combo = egui::ComboBox::from_id_salt("external_addon_origin_filter")
                    .selected_text(origin_selected)
                    .show_ui(ui, |ui| {
                        let response_all =
                            ui.selectable_label(origin_filter == "All", tr("All origins"));
                        if response_all.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if response_all.clicked() {
                            *origin_filter = "All".to_string();
                            ui_state_changed = true;
                        }

                        for origin in &origin_options {
                            let response = ui.selectable_label(origin_filter == origin, tr(origin));
                            if response.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response.clicked() {
                                *origin_filter = origin.clone();
                                ui_state_changed = true;
                            }
                        }
                    });
                if origin_combo.response.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                let group_by_origin_checkbox =
                    ui.checkbox(group_by_origin, tr("Group by origin"));
                if group_by_origin_checkbox.changed() {
                    ui_state_changed = true;
                }
                if group_by_origin_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                let favorites_only_checkbox =
                    ui.checkbox(&mut self.addon_favorites_only_filter, tr("Favorites only"));
                if favorites_only_checkbox.changed() {
                    ui_state_changed = true;
                }
                if favorites_only_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                let client_side_only_checkbox = ui.checkbox(
                    &mut self.addon_client_side_only_filter,
                    tr("Client-side only"),
                );
                if client_side_only_checkbox.changed() {
                    ui_state_changed = true;
                }
                if client_side_only_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                ui.label(tr("State:"));
                ui.add_space(6.0);
                let combo_box_response =
                    egui::ComboBox::from_id_salt("external_addon_state_filter")
                        .selected_text(state_selected)
                        .show_ui(ui, |ui| {
                            let response_all =
                                ui.selectable_label(addon_state_filter == "All", tr("All"));
                            if response_all.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response_all.clicked() {
                                *addon_state_filter = "All".to_string();
                                ui_state_changed = true;
                            }

                            let response_enabled = ui
                                .selectable_label(addon_state_filter == "Enabled", tr("Enabled"));
                            if response_enabled.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response_enabled.clicked() {
                                *addon_state_filter = "Enabled".to_string();
                                ui_state_changed = true;
                            }

                            let response_disabled = ui.selectable_label(
                                addon_state_filter == "Disabled",
                                tr("Disabled"),
                            );
                            if response_disabled.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response_disabled.clicked() {
                                *addon_state_filter = "Disabled".to_string();
                                ui_state_changed = true;
                            }
                        });
                if combo_box_response.response.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
            });
            ui.separator();

            self.ensure_filtered_repository_external_addon_indices_cached(
                repo_index,
                filter,
                origin_filter,
                addon_state_filter,
                self.addon_favorites_only_filter,
                self.addon_client_side_only_filter,
            );

            let external_count = self.repository_external_addons_list_cache.rows.len();
            if external_count == 0 {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("No external addons were found. Shared addons are unavailable, no external addon folder is configured, or no Steam addons were detected."))
                        .color(color_text_dim)
                        .italics(),
                );
                return;
            }

            let filtered_len = self.repository_external_addons_list_cache.filtered_indices.len();
            if filtered_len == 0 {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("No external addons match the current filters."))
                        .color(color_text_dim)
                        .italics(),
                );
                return;
            }

            let name_font = ui
                .style()
                .text_styles
                .get(&egui::TextStyle::Body)
                .map(|font| font.size)
                .unwrap_or(14.0);
            let path_font = self
                .settings_view_state
                .font_sizes
                .repository_settings_view
                .addon_path as f32;
            let body_family = ui
                .style()
                .text_styles
                .get(&egui::TextStyle::Body)
                .map(|font| font.family.clone())
                .unwrap_or(egui::FontFamily::Proportional);
            let row_style = ExternalAddonRowStyle {
                enabled_card_fill: self.color_addon_row_enabled_bg(),
                disabled_card_fill: self.color_addon_row_disabled_bg(),
                color_text_normal: self.color_text_normal(),
                color_text_gray: self.color_text_gray(),
                color_text_dim: self.color_text_dim(),
                name_font_id: egui::FontId::new(name_font, body_family.clone()),
                path_font_id: egui::FontId::new(path_font, body_family),
            };

            {
                let cache = &mut self.repository_external_addons_list_cache;
                let row_count = cache.rows.len();
                if cache.galleys.ensure_rows(row_count, name_font, path_font) {
                    cache.galley_prewarm_cursor = 0;
                    cache.galley_prewarm_path_width = None;
                    cache.galley_prewarm_include_origin = false;
                }
            }

            let text_area_width = external_text_area_width(ui.available_width(), horizontal_padding);
            let include_origin = !*group_by_origin;
            let (max_name_width, max_required_side_width) = {
                let cache = &mut self.repository_external_addons_list_cache;
                Self::ensure_repository_external_addon_layout_widths(
                    ui,
                    cache,
                    &row_style,
                    include_origin,
                )
            };
            let row_height = external_addon_row_height(
                text_area_width,
                max_name_width,
                max_required_side_width,
                ui.spacing().item_spacing.x,
                include_origin,
            );
            self.prewarm_repository_external_addon_galleys(
                ui,
                &row_style,
                text_area_width,
                row_height,
                include_origin,
            );

            if *group_by_origin {
                let grouped_row_count = {
                    let cache = &self.repository_external_addons_list_cache;
                    grouped_external_addon_row_count(cache)
                };

                ScrollArea::vertical()
                    .id_salt(("repository_external_addons_grouped_list_cached", repo_index))
                    .show_rows(
                        ui,
                        row_height,
                        grouped_row_count,
                        |ui, row_range| {
                            let visible_rows = {
                                let cache = &self.repository_external_addons_list_cache;
                                visible_grouped_external_addon_rows(cache, row_range)
                            };

                            for visible_row in visible_rows {
                                match visible_row {
                                    ExternalAddonGroupedRow::Header {
                                        row_slot: _,
                                        origin,
                                        collapsed,
                                    } => {
                                        let mut collapse_clicked = false;
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(
                                                ui.available_width(),
                                                row_height,
                                            ),
                                            Layout::left_to_right(Align::Center),
                                            |ui| {
                                                ui.add_space(horizontal_padding);
                                                ui.heading(tr(&origin));
                                                ui.add_space(6.0);
                                                let collapse_button = ui.add_sized(
                                                    Vec2::new(28.0, 24.0),
                                                    Button::new(if collapsed { "+" } else { "-" }),
                                                );
                                                if collapse_button.hovered() {
                                                    ui.ctx().output_mut(
                                                        Foxy::set_pointing_cursor_output,
                                                    );
                                                }
                                                if collapse_button.clicked() {
                                                    collapse_clicked = true;
                                                }
                                            },
                                        );

                                        if collapse_clicked {
                                            if collapsed {
                                                self.repository_external_addons_list_cache
                                                    .collapsed_origins
                                                    .remove(&origin);
                                            } else {
                                                self.repository_external_addons_list_cache
                                                    .collapsed_origins
                                                    .insert(origin.clone());
                                            }
                                        }
                                    }
                                    ExternalAddonGroupedRow::Addon {
                                        row_slot,
                                        entry_index,
                                    } => {
                                        // Scope every widget id in the row under the
                                        // stable addon index so the icon buttons and
                                        // checkbox keep the same ids across egui layout
                                        // passes (otherwise their auto-ids shift with the
                                        // visible range and trigger "changed id between
                                        // passes" + extra passes while scrolling).
                                        ui.push_id(entry_index, |ui| {
                                            self.render_repository_external_addon_row_cached(
                                                ui,
                                                entry_index,
                                                row_slot,
                                                *group_by_origin,
                                                horizontal_padding,
                                                row_height,
                                                text_area_width,
                                                &row_style,
                                                &mut repo_data_changed,
                                                &mut external_addon_context_action,
                                                &context_items,
                                                &row_tooltips,
                                            );
                                        });
                                    }
                                }
                            }
                        },
                    );
            } else {
                ScrollArea::vertical()
                    .id_salt(("repository_external_addons_list_cached", repo_index))
                    .show_rows(ui, row_height, filtered_len, |ui, row_range| {
                        let row_start = row_range.start;
                        for filtered_index in row_range {
                            let row_slot = filtered_index - row_start;
                            let entry_index = self
                                .repository_external_addons_list_cache
                                .filtered_indices[filtered_index];
                            // Stable per-addon id scope, see the grouped branch above.
                            ui.push_id(entry_index, |ui| {
                                self.render_repository_external_addon_row_cached(
                                    ui,
                                    entry_index,
                                    row_slot,
                                    *group_by_origin,
                                    horizontal_padding,
                                    row_height,
                                    text_area_width,
                                    &row_style,
                                    &mut repo_data_changed,
                                    &mut external_addon_context_action,
                                    &context_items,
                                    &row_tooltips,
                                );
                            });
                        }
                    });
            }
        });

        if repo_data_changed {
            self.persist_repository_external_addon_row_state_cached(repo_index);
        } else if ui_state_changed {
            // UI-only filter controls should not persist repository data.
        }

        if let Some((addon_name, addon_directory_path, action)) = external_addon_context_action {
            match action {
                ExternalAddonContextAction::OpenDirectory => {
                    if !self.open_addon_directory(&addon_name, &addon_directory_path) {
                        warn!("Failed to open addon directory for {}", addon_name);
                        self.show_error_toast(self.t("Failed to open addon directory."));
                    }
                }
                ExternalAddonContextAction::Delete => {
                    self.pending_addon_destructive_confirmation =
                        Some(AddonDestructiveConfirmAction::Delete {
                            addon_name,
                            addon_path: addon_directory_path,
                        });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_grouped_external_addon_rows_slices_only_visible_range() {
        let mut cache = RepositoryExternalAddonsListCache {
            grouped_filtered_indices: vec![
                ("A".to_string(), vec![10, 11]),
                ("B".to_string(), vec![20, 21]),
                ("C".to_string(), vec![30]),
            ],
            ..Default::default()
        };
        cache.collapsed_origins.insert("B".to_string());

        assert_eq!(grouped_external_addon_row_count(&cache), 6);
        assert_eq!(
            visible_grouped_external_addon_rows(&cache, 1..5),
            vec![
                ExternalAddonGroupedRow::Addon {
                    row_slot: 0,
                    entry_index: 10,
                },
                ExternalAddonGroupedRow::Addon {
                    row_slot: 1,
                    entry_index: 11,
                },
                ExternalAddonGroupedRow::Header {
                    row_slot: 2,
                    origin: "B".to_string(),
                    collapsed: true,
                },
                ExternalAddonGroupedRow::Header {
                    row_slot: 3,
                    origin: "C".to_string(),
                    collapsed: false,
                },
            ]
        );
    }

    #[test]
    fn compact_external_overlay_text_preserves_short_text_and_caps_long_text() {
        let short = "C:\\Mods\\ace";
        assert_eq!(compact_external_overlay_text(short), short);

        let long = format!("C:\\{}", "nested\\".repeat(80));
        let compact = compact_external_overlay_text(&long);

        assert_eq!(compact.chars().count(), MAX_EXTERNAL_OVERLAY_TEXT_CHARS);
        assert!(compact.starts_with("..."));
        assert!(long.ends_with(compact.trim_start_matches("...")));
    }

    #[test]
    fn external_row_side_padding_clamps_for_narrow_rows() {
        assert_eq!(external_row_side_padding(300.0, 15.0), 15.0);
        assert_eq!(external_row_side_padding(20.0, 15.0), 9.0);
        assert_eq!(external_row_side_padding(1.0, 15.0), 0.0);
    }

    #[test]
    fn external_centered_side_width_uses_space_beside_centered_name() {
        assert_eq!(external_centered_side_width(10.0, 60.0, 8.0), 0.0);
        assert_eq!(external_centered_side_width(900.0, 300.0, 8.0), 292.0);
        assert_eq!(
            external_metadata_lane_widths(900.0, 300.0, 8.0, true, false),
            (292.0, 292.0)
        );
        assert_eq!(
            external_metadata_lane_widths(900.0, 300.0, 8.0, true, true),
            (446.0, 446.0)
        );
    }

    #[test]
    fn external_addon_name_wraps_when_metadata_lane_would_be_too_small() {
        assert!(external_addon_name_wraps(900.0, 700.0, 180.0, 8.0, true));
        assert!(!external_addon_name_wraps(900.0, 500.0, 180.0, 8.0, true));
        assert!(external_addon_name_wraps(900.0, 500.0, 260.0, 8.0, true));
        assert!(!external_addon_name_wraps(900.0, 700.0, 260.0, 8.0, false));
    }

    #[test]
    fn external_addon_row_height_grows_for_wrapped_name_lane() {
        assert_eq!(
            external_addon_row_height(900.0, 700.0, 180.0, 8.0, true),
            EXTERNAL_ADDON_WRAPPED_ROW_HEIGHT
        );
        assert_eq!(
            external_addon_row_height(900.0, 500.0, 180.0, 8.0, true),
            EXTERNAL_ADDON_ROW_HEIGHT
        );
        assert_eq!(
            external_addon_row_height(900.0, 500.0, 260.0, 8.0, true),
            EXTERNAL_ADDON_WRAPPED_ROW_HEIGHT
        );
    }
}
