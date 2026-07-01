use super::AddonContextAction;
use crate::ui::app::{AddonDestructiveConfirmAction, Foxy, RepositoryAddonListKind};
use crate::ui::context_menu::{ContextMenuItem, attach_context_menu};
use crate::ui::i18n::{fmt_bytes, tr, tr_fmt};
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align, Align2, AsIdSalt, Button, Color32, CornerRadius, FontId, Layout, RichText,
    ScrollArea, TextEdit, Ui, Vec2,
};
use log::warn;

const ADDON_ROW_HEIGHT: f32 = 40.0;
const ADDON_ROW_INNER_MARGIN: f32 = 5.0;
const ADDON_ICON_BUTTON_SIZE: f32 = 28.0;
const ADDON_ACTION_WIDTH: f32 = 56.0;
const OPTIONAL_ADDON_ACTION_WIDTH: f32 = 112.0;
const MAX_ADDON_PATH_OVERLAY_WIDTH: f32 = 440.0;
/// Rows whose name/path galleys are shaped per frame by the background prewarm.
const ADDON_GALLEY_PREWARM_ROWS_PER_FRAME: usize = 128;

struct AddonRowTooltips {
    add_favorite: String,
    remove_favorite: String,
    mark_client_side: String,
    remove_client_side: String,
    forced_client_side: String,
}

struct AddonContextMenuLabels {
    open_directory: String,
    backup: String,
    restore_backup: String,
    recheck_integrity: String,
    standalone_download: String,
    force_redownload: String,
    delete: String,
}

struct AddonIconCellStyle {
    fill: Color32,
    stroke: Color32,
    text_color: Color32,
}

fn paint_addon_icon_cell(
    ui: &mut Ui,
    rect: egui::Rect,
    id_source: impl AsIdSalt,
    label: &str,
    font_id: FontId,
    style: AddonIconCellStyle,
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

/// Path-overlay width for an addon row, computed from the list's content width
/// (outside the `ScrollArea`). Using one scrollbar-independent width for both the
/// prewarm and the row render keeps the truncated path galley a cache hit instead
/// of re-shaping at a slightly different inner width on every reveal.
fn addon_path_overlay_width(
    available_width: f32,
    horizontal_padding: f32,
    action_width: f32,
) -> f32 {
    let card_width = (available_width - 2.0 * horizontal_padding).max(0.0);
    let inner_width = (card_width - 2.0 * ADDON_ROW_INNER_MARGIN).max(0.0);
    let text_width = (inner_width - action_width).max(0.0);
    text_width.min(MAX_ADDON_PATH_OVERLAY_WIDTH).round()
}

impl Foxy {
    /// Shape a chunk of off-screen addon name/path galleys per frame so they are
    /// cached before the user scrolls to them, instead of being shaped on first
    /// reveal (the dominant per-frame cost while scrolling). Mirrors the external
    /// addon prewarm. Walks `filtered_indices` from a stored cursor that resets
    /// when the rows, filter, or overlay width change; requests another frame
    /// until the whole filtered list is warm, then becomes a no-op.
    fn prewarm_repository_addon_galleys(
        &mut self,
        ui: &Ui,
        kind: RepositoryAddonListKind,
        name_font_id: &egui::FontId,
        path_font_id: &egui::FontId,
        path_width: f32,
    ) {
        if path_width <= 0.0 {
            return;
        }
        let cache = self.repository_addon_list_cache_mut_cached(kind);
        let filtered_len = cache.filtered_indices.len();
        if filtered_len == 0 {
            return;
        }
        if cache.galley_prewarm_path_width != Some(path_width) {
            cache.galley_prewarm_path_width = Some(path_width);
            cache.galley_prewarm_cursor = 0;
        }
        let start = cache.galley_prewarm_cursor.min(filtered_len);
        if start >= filtered_len {
            return;
        }
        let end = (start + ADDON_GALLEY_PREWARM_ROWS_PER_FRAME).min(filtered_len);
        for filtered_index in start..end {
            let source_index = cache.filtered_indices[filtered_index];
            let name_text = (!cache.galleys.has_name(source_index)).then(|| {
                let size = cache
                    .remote_size_bytes_by_source
                    .get(source_index)
                    .copied()
                    .unwrap_or(0);
                let name = &cache.source_names[source_index];
                if size > 0 {
                    format!("{}  ({})", name, fmt_bytes(size))
                } else {
                    name.clone()
                }
            });
            galley_cache::lazy_galley(
                ui,
                cache.galleys.name_slot(source_index),
                name_font_id.clone(),
                || name_text.expect("missing addon name prewarm text"),
            );

            let path_text =
                (!cache.galleys.has_path_for_width(source_index, path_width)).then(|| {
                    cache.preferred_paths[source_index]
                        .clone()
                        .unwrap_or_else(|| tr("(path not found)"))
                });
            cache.galleys.ensure_path_width(path_width);
            galley_cache::truncated_galley(
                ui,
                cache.galleys.path_slot(source_index),
                path_font_id.clone(),
                path_width,
                || path_text.expect("missing addon path prewarm text"),
            );
        }
        cache.galley_prewarm_cursor = end;
        if end < filtered_len {
            self.needs_repaint = true;
            ui.ctx().request_repaint();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_repository_addon_row_fast(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        kind: RepositoryAddonListKind,
        source_index: usize,
        list_kind_salt: &str,
        name: &str,
        enabled: bool,
        favorite: bool,
        client_side: bool,
        forced_client_side: bool,
        addon_directory_path: Option<String>,
        remote_size_bytes: u64,
        backup_configured: bool,
        horizontal_padding: f32,
        path_width: f32,
        name_font_id: &egui::FontId,
        path_font_id: &egui::FontId,
        repo_data_changed: &mut bool,
        addon_context_action: &mut Option<(String, Option<String>, AddonContextAction)>,
        context_menu_labels: &AddonContextMenuLabels,
        tooltips: &AddonRowTooltips,
    ) {
        let available_width = ui.available_width();
        let row_size = Vec2::new(available_width, ADDON_ROW_HEIGHT);
        let (row_rect, _) = ui.allocate_exact_size(row_size, egui::Sense::hover());
        let card_rect = egui::Rect::from_min_max(
            egui::pos2(row_rect.left() + horizontal_padding, row_rect.top() + 1.0),
            egui::pos2(
                row_rect.right() - horizontal_padding,
                row_rect.bottom() - 1.0,
            ),
        );
        let inner_rect = card_rect.shrink(ADDON_ROW_INNER_MARGIN);
        let action_width = if kind == RepositoryAddonListKind::OptionalAddons {
            OPTIONAL_ADDON_ACTION_WIDTH
        } else {
            ADDON_ACTION_WIDTH
        };
        let text_rect = egui::Rect::from_min_max(
            inner_rect.min,
            egui::pos2(
                (inner_rect.right() - action_width).max(inner_rect.left()),
                inner_rect.bottom(),
            ),
        );
        let button_top = inner_rect.center().y - ADDON_ICON_BUTTON_SIZE * 0.5;
        let checkbox_rect = egui::Rect::from_min_size(
            egui::pos2(inner_rect.right() - ADDON_ICON_BUTTON_SIZE, button_top),
            Vec2::splat(ADDON_ICON_BUTTON_SIZE),
        );
        let client_side_button_rect =
            (kind == RepositoryAddonListKind::OptionalAddons).then(|| {
                egui::Rect::from_min_size(
                    egui::pos2(checkbox_rect.left() - ADDON_ICON_BUTTON_SIZE, button_top),
                    Vec2::splat(ADDON_ICON_BUTTON_SIZE),
                )
            });
        let favorite_button_rect = client_side_button_rect.map(|client_rect| {
            egui::Rect::from_min_size(
                egui::pos2(client_rect.left() - ADDON_ICON_BUTTON_SIZE, button_top),
                Vec2::splat(ADDON_ICON_BUTTON_SIZE),
            )
        });

        let card_fill = if enabled {
            self.color_addon_row_enabled_bg()
        } else {
            self.color_addon_row_disabled_bg()
        };
        let text_color = if enabled {
            self.color_text_normal()
        } else {
            self.color_text_gray()
        };
        let path_color = if enabled {
            self.color_text_gray()
        } else {
            self.color_text_dim()
        };
        let card_stroke = if client_side || forced_client_side {
            self.color_primary_accent()
        } else {
            self.color_text_gray()
        };

        let row_id =
            ui.make_persistent_id(("repository_addon_row_cached", list_kind_salt, source_index));
        let context_response = ui.interact(card_rect, row_id, egui::Sense::click());
        let card_fill = if context_response.hovered() {
            self.color_widget_bg_active()
        } else {
            card_fill
        };
        let card_stroke = if context_response.hovered() {
            self.color_primary_accent_hover()
        } else {
            card_stroke
        };

        ui.painter()
            .rect_filled(card_rect, CornerRadius::same(5), card_fill);
        ui.painter().rect_stroke(
            card_rect,
            CornerRadius::same(5),
            egui::Stroke::new(1.0, card_stroke),
            egui::StrokeKind::Inside,
        );

        let name_galley = {
            let cache = self.repository_addon_list_cache_mut_cached(kind);
            galley_cache::lazy_galley(
                ui,
                cache.galleys.name_slot(source_index),
                name_font_id.clone(),
                || {
                    if remote_size_bytes > 0 {
                        format!("{}  ({})", name, fmt_bytes(remote_size_bytes))
                    } else {
                        name.to_string()
                    }
                },
            )
        };
        galley_cache::paint_centered(ui, text_rect, name_galley, text_color);

        // `path_width` is computed once outside the ScrollArea and passed in, so
        // the prewarm and the render shape the truncated path galley at the same
        // width and it stays a cache hit across scroll.
        let mut path_rect = text_rect;
        path_rect.set_width(path_width.min(text_rect.width()));
        let path_galley = {
            let cache = self.repository_addon_list_cache_mut_cached(kind);
            cache.galleys.ensure_path_width(path_width);
            galley_cache::truncated_galley(
                ui,
                cache.galleys.path_slot(source_index),
                path_font_id.clone(),
                path_width,
                || {
                    addon_directory_path
                        .clone()
                        .unwrap_or_else(|| tr("(path not found)"))
                },
            )
        };
        galley_cache::paint_overlay_left(ui, path_rect, path_galley, path_color);

        let mut favorite_clicked = false;
        let mut client_side_clicked = false;

        if let Some(favorite_button_rect) = favorite_button_rect {
            let favorite_button = paint_addon_icon_cell(
                ui,
                favorite_button_rect,
                ("repository_optional_addon_favorite", source_index),
                "\u{2605}",
                name_font_id.clone(),
                AddonIconCellStyle {
                    fill: self.color_main_bg(),
                    stroke: if favorite {
                        self.color_primary_accent()
                    } else {
                        self.color_text_gray()
                    },
                    text_color: if favorite {
                        self.color_primary_accent()
                    } else if enabled {
                        self.color_text_normal()
                    } else {
                        self.color_text_gray()
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
            favorite_clicked = favorite_button.clicked();
            if favorite_clicked
                && self.set_repository_optional_addon_row_favorite_cached(source_index, !favorite)
            {
                self.persist_repository_optional_addon_favorite_state_cached(repo_index);
            }
        }

        if let Some(client_side_button_rect) = client_side_button_rect {
            let client_side_text_color = if forced_client_side {
                self.color_text_gray()
            } else if client_side || enabled {
                self.color_text_normal()
            } else {
                self.color_text_gray()
            };
            let client_side_button = paint_addon_icon_cell(
                ui,
                client_side_button_rect,
                ("repository_optional_addon_client_side", source_index),
                "C",
                name_font_id.clone(),
                AddonIconCellStyle {
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
                        self.color_text_gray()
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
            client_side_clicked = client_side_button.clicked();
            if !forced_client_side
                && client_side_clicked
                && self.set_repository_optional_addon_row_client_side_cached(
                    source_index,
                    !client_side,
                )
            {
                self.persist_repository_optional_addon_client_side_state_cached(repo_index);
            }
        }

        let mut row_enabled = enabled;
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
            && self.set_repository_addon_row_enabled_cached(kind, source_index, row_enabled)
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
            && self.set_repository_addon_row_enabled_cached(kind, source_index, !enabled)
        {
            *repo_data_changed = true;
        }

        let mut context_action: Option<AddonContextAction> = None;
        attach_context_menu(
            &context_response,
            &[
                ContextMenuItem::new(
                    AddonContextAction::OpenDirectory,
                    context_menu_labels.open_directory.clone(),
                )
                .disabled_if(addon_directory_path.is_none()),
                ContextMenuItem::new(
                    AddonContextAction::Backup,
                    context_menu_labels.backup.clone(),
                )
                .disabled_if(addon_directory_path.is_none() || !backup_configured)
                .separator_before(),
                ContextMenuItem::new(
                    AddonContextAction::RestoreBackup,
                    context_menu_labels.restore_backup.clone(),
                )
                .disabled_if(!backup_configured)
                .separator_before(),
                ContextMenuItem::new(
                    AddonContextAction::RecheckIntegrity,
                    context_menu_labels.recheck_integrity.clone(),
                )
                .separator_before(),
                ContextMenuItem::new(
                    AddonContextAction::StandaloneDownload,
                    context_menu_labels.standalone_download.clone(),
                )
                .separator_before(),
                ContextMenuItem::new(
                    AddonContextAction::ForceRedownload,
                    context_menu_labels.force_redownload.clone(),
                )
                .separator_before()
                .danger(),
                ContextMenuItem::new(
                    AddonContextAction::Delete,
                    context_menu_labels.delete.clone(),
                )
                .disabled_if(addon_directory_path.is_none())
                .separator_before()
                .danger(),
            ],
            &mut context_action,
        );
        if let Some(action) = context_action {
            *addon_context_action = Some((name.to_string(), addon_directory_path, action));
        }
    }

    pub(super) fn render_repository_addons_list_cached(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        kind: RepositoryAddonListKind,
        filter: &mut String,
        addon_state_filter: &mut String,
    ) {
        self.ensure_repository_addon_list_cache_cached(repo_index, kind);

        let label = match kind {
            RepositoryAddonListKind::Addons => "addons",
            RepositoryAddonListKind::OptionalAddons => "optional addons",
        };
        let backup_configured = self.configured_backup_directory().is_some();
        let horizontal_padding = 15.0;
        let addon_path_font = self
            .settings_view_state
            .font_sizes
            .repository_settings_view
            .addon_path as f32;
        // Compact addon cards: shrink all card text by 20% to reduce vertical size.
        let path_font = addon_path_font * 1.0;
        let name_font = ui
            .style()
            .text_styles
            .get(&egui::TextStyle::Body)
            .map(|font| font.size)
            .unwrap_or(14.0)
            * 1.0;
        let body_family = ui
            .style()
            .text_styles
            .get(&egui::TextStyle::Body)
            .map(|font| font.family.clone())
            .unwrap_or(egui::FontFamily::Proportional);
        let name_font_id = egui::FontId::new(name_font, body_family.clone());
        let path_font_id = egui::FontId::new(path_font, body_family);
        let mut ui_state_changed = false;
        let mut repo_data_changed = false;
        let mut addon_context_action: Option<(String, Option<String>, AddonContextAction)> = None;
        let row_tooltips = AddonRowTooltips {
            add_favorite: tr("Add to favorites"),
            remove_favorite: tr("Remove from favorites"),
            mark_client_side: tr("Mark as client-side addon"),
            remove_client_side: tr("Remove from client-side addons"),
            forced_client_side: tr("Client-side addon defined by repository"),
        };
        let context_menu_labels = AddonContextMenuLabels {
            open_directory: tr("Open addon directory"),
            backup: tr("Manual addon backup"),
            restore_backup: tr("Restore addon backup"),
            recheck_integrity: tr("Recheck addon integrity"),
            standalone_download: tr("Standalone download"),
            force_redownload: tr("Force redownload addon"),
            delete: tr("Delete addon"),
        };

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!(
                    "{} {}",
                    '\u{2139}',
                    tr_fmt(
                        "Here you can enable/disable {kind} for this repository.",
                        &[("kind", tr(label))],
                    )
                );
                let info_color = self.color_text_dim();

                // Reserve the action buttons on the right first, then let the info
                // text fill the remaining width and wrap to as many lines as it
                // needs so the buttons always stay fully visible.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(50.0);

                    let disable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Disable all")));
                    if disable_all_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if disable_all_button.clicked()
                        && self.set_all_repository_addon_rows_enabled_cached(kind, false)
                    {
                        self.persist_repository_addon_row_state_cached(repo_index, kind);
                    }

                    ui.add_space(5.0);

                    let enable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Enable all")));
                    if enable_all_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if enable_all_button.clicked()
                        && self.set_all_repository_addon_rows_enabled_cached(kind, true)
                    {
                        self.persist_repository_addon_row_state_cached(repo_index, kind);
                    }

                    ui.add_space(10.0);
                    ui.add(
                        egui::Label::new(RichText::new(info_text).italics().color(info_color))
                            .wrap(),
                    );
                });
            });
            ui.separator();

            // Filter field + trailing controls share one wrapping row. On a wide
            // window the filter expands so the controls sit toward the right edge;
            // when the window is too narrow the controls collapse onto their own
            // line(s) below instead of being clipped off the edge.
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Filter:"));
                self.filter_help_icon(ui, &tr("addon_filter_help"));
                ui.add_space(horizontal_padding);

                let state_selected = match addon_state_filter.as_str() {
                    "Enabled" => tr("Enabled"),
                    "Disabled" => tr("Disabled"),
                    _ => tr("All"),
                };
                let item_spacing = ui.spacing().item_spacing.x;
                // The explicit add_space() gaps plus egui's per-widget item spacing.
                // Counting them in full keeps the filter from being sized too wide,
                // which would otherwise shove the trailing controls off the edge.
                let mut controls_width = super::filter_controls_text_width(ui, &tr("State:"))
                    + super::filter_controls_combo_width(ui, &state_selected)
                    + 16.0
                    + 6.0
                    + item_spacing * 4.0;
                if kind == RepositoryAddonListKind::OptionalAddons {
                    controls_width +=
                        super::filter_controls_checkbox_width(ui, &tr("Favorites only"))
                            + super::filter_controls_checkbox_width(ui, &tr("Client-side only"))
                            + 16.0 * 2.0
                            + item_spacing * 4.0;
                }

                let filter_width =
                    super::responsive_filter_field_width(ui.available_width(), controls_width);
                let filter_edit = ui.add(TextEdit::singleline(filter).desired_width(filter_width));
                if filter_edit.changed() {
                    ui_state_changed = true;
                }
                if filter_edit.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                ui.label(tr("State:"));
                ui.add_space(6.0);
                let combo_box_response = egui::ComboBox::from_id_salt(match kind {
                    RepositoryAddonListKind::Addons => "repository_addon_state_filter",
                    RepositoryAddonListKind::OptionalAddons => {
                        "repository_optional_addon_state_filter"
                    }
                })
                .selected_text(state_selected)
                .show_ui(ui, |ui| {
                    let response_all = ui.selectable_label(addon_state_filter == "All", tr("All"));
                    if response_all.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if response_all.clicked() {
                        *addon_state_filter = "All".to_string();
                        ui_state_changed = true;
                    }

                    let response_enabled =
                        ui.selectable_label(addon_state_filter == "Enabled", tr("Enabled"));
                    if response_enabled.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if response_enabled.clicked() {
                        *addon_state_filter = "Enabled".to_string();
                        ui_state_changed = true;
                    }

                    let response_disabled =
                        ui.selectable_label(addon_state_filter == "Disabled", tr("Disabled"));
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

                if kind == RepositoryAddonListKind::OptionalAddons {
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
                }
            });
            ui.separator();

            self.ensure_filtered_repository_addon_indices_cached(
                repo_index,
                kind,
                filter,
                addon_state_filter,
                self.addon_favorites_only_filter && kind == RepositoryAddonListKind::OptionalAddons,
                self.addon_client_side_only_filter
                    && kind == RepositoryAddonListKind::OptionalAddons,
            );

            let addon_count = self
                .repository_addon_list_cache_cached(kind)
                .source_names
                .len();
            if addon_count == 0 {
                let empty_message = match kind {
                    RepositoryAddonListKind::OptionalAddons => {
                        tr("This repository does not provide any optional addons.")
                    }
                    RepositoryAddonListKind::Addons => {
                        tr("This repository does not provide any addons in this section.")
                    }
                };
                ui.add_space(8.0);
                ui.label(
                    RichText::new(empty_message)
                        .color(self.color_text_dim())
                        .italics(),
                );
                return;
            }

            let filtered_len = self
                .repository_addon_list_cache_cached(kind)
                .filtered_indices
                .len();
            if filtered_len == 0 {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("No addons match the current filters."))
                        .color(self.color_text_dim())
                        .italics(),
                );
                return;
            }

            let (enabled_count, filtered_size_bytes, total_size_bytes) = {
                let cache = self.repository_addon_list_cache_cached(kind);
                let enabled_count = cache
                    .enabled_by_source
                    .iter()
                    .filter(|enabled| **enabled)
                    .count();
                let filtered_size_bytes = cache
                    .filtered_indices
                    .iter()
                    .map(|idx| {
                        cache
                            .remote_size_bytes_by_source
                            .get(*idx)
                            .copied()
                            .unwrap_or(0)
                    })
                    .sum::<u64>();
                let total_size_bytes = cache.remote_size_bytes_by_source.iter().sum::<u64>();
                (enabled_count, filtered_size_bytes, total_size_bytes)
            };
            ui.label(
                RichText::new(format!(
                    "{} / {} {} - {}: {} - {} / {}",
                    filtered_len,
                    addon_count,
                    tr("Addons"),
                    tr("Enabled"),
                    enabled_count,
                    tr_fmt(
                        "Total size: {size}",
                        &[("size", fmt_bytes(filtered_size_bytes))]
                    ),
                    fmt_bytes(total_size_bytes),
                ))
                .color(self.color_text_dim()),
            );
            ui.add_space(6.0);

            let row_height = 40.0;
            let list_kind_salt = match kind {
                RepositoryAddonListKind::Addons => "addons",
                RepositoryAddonListKind::OptionalAddons => "optional_addons",
            };
            {
                let cache = self.repository_addon_list_cache_mut_cached(kind);
                let row_count = cache.source_names.len();
                if cache.galleys.ensure_rows(row_count, name_font, path_font) {
                    cache.galley_prewarm_cursor = 0;
                    cache.galley_prewarm_path_width = None;
                }
            }
            // One scrollbar-independent overlay width for both the prewarm and the
            // row render, computed here (outside the ScrollArea) so the truncated
            // path galleys stay cache hits while scrolling.
            let action_width = if kind == RepositoryAddonListKind::OptionalAddons {
                OPTIONAL_ADDON_ACTION_WIDTH
            } else {
                ADDON_ACTION_WIDTH
            };
            let path_overlay_width =
                addon_path_overlay_width(ui.available_width(), horizontal_padding, action_width);
            self.prewarm_repository_addon_galleys(
                ui,
                kind,
                &name_font_id,
                &path_font_id,
                path_overlay_width,
            );
            ScrollArea::vertical()
                .id_salt(("repository_addons_list_cached", repo_index, list_kind_salt))
                .show_rows(ui, row_height, filtered_len, |ui, row_range| {
                    for filtered_index in row_range {
                        let source_index = self
                            .repository_addon_list_cache_cached(kind)
                            .filtered_indices[filtered_index];
                        let (
                            name,
                            enabled,
                            favorite,
                            client_side,
                            forced_client_side,
                            addon_directory_path,
                            remote_size_bytes,
                        ) = {
                            let cache = self.repository_addon_list_cache_cached(kind);
                            (
                                cache.source_names[source_index].clone(),
                                cache.enabled_by_source[source_index],
                                cache.favorite_by_source[source_index],
                                cache.client_side_by_source[source_index],
                                cache.forced_client_side_by_source[source_index],
                                cache.preferred_paths[source_index].clone(),
                                cache
                                    .remote_size_bytes_by_source
                                    .get(source_index)
                                    .copied()
                                    .unwrap_or(0),
                            )
                        };
                        // Salt every widget in the row by the stable source index
                        // so the icon buttons/checkbox keep the same id when the
                        // visible range shifts. Auto (counter-based) ids shift with
                        // the range and make egui re-run extra layout passes
                        // ("changed id between passes"), multiplying the per-frame
                        // cost while scrolling.
                        ui.push_id(source_index, |ui| {
                            self.render_repository_addon_row_fast(
                                ui,
                                repo_index,
                                kind,
                                source_index,
                                list_kind_salt,
                                &name,
                                enabled,
                                favorite,
                                client_side,
                                forced_client_side,
                                addon_directory_path,
                                remote_size_bytes,
                                backup_configured,
                                horizontal_padding,
                                path_overlay_width,
                                &name_font_id,
                                &path_font_id,
                                &mut repo_data_changed,
                                &mut addon_context_action,
                                &context_menu_labels,
                                &row_tooltips,
                            );
                        });
                    }
                });
        });

        if repo_data_changed {
            self.persist_repository_addon_row_state_cached(repo_index, kind);
        } else if ui_state_changed {
            // UI-only filter controls should not persist repository data.
        }

        if let Some((addon_name, addon_directory_path, action)) = addon_context_action {
            match action {
                AddonContextAction::OpenDirectory => {
                    if let Some(path) = addon_directory_path {
                        if !self.open_addon_directory(&addon_name, &path) {
                            warn!("Failed to open addon directory for {}", addon_name);
                            self.show_error_toast(self.t("Failed to open addon directory."));
                        }
                    } else {
                        warn!(
                            "Open addon directory skipped: path not found for {}",
                            addon_name
                        );
                    }
                }
                AddonContextAction::Backup => {
                    if !self.start_manual_addon_backup(
                        repo_index,
                        &addon_name,
                        addon_directory_path.as_deref(),
                    ) {
                        warn!("Manual addon backup failed for {}", addon_name);
                    }
                }
                AddonContextAction::RestoreBackup => {
                    if !self.open_addon_backup_restore_selector(
                        repo_index,
                        &addon_name,
                        addon_directory_path.as_deref(),
                    ) {
                        warn!("Addon backup restore selection failed for {}", addon_name);
                    }
                }
                AddonContextAction::RecheckIntegrity => {
                    if !self.recalculate_addon_hashes(repo_index, &addon_name) {
                        warn!("Manual addon hash recalculation failed for {}", addon_name);
                    }
                }
                AddonContextAction::StandaloneDownload => {
                    if !self.standalone_download_addon(repo_index, &addon_name) {
                        warn!("Standalone download failed for addon {}", addon_name);
                    }
                }
                AddonContextAction::ForceRedownload => {
                    self.pending_addon_destructive_confirmation =
                        Some(AddonDestructiveConfirmAction::ForceRedownload {
                            repo_idx: repo_index,
                            addon_name,
                            addon_path: addon_directory_path,
                        });
                }
                AddonContextAction::Delete => {
                    if let Some(path) = addon_directory_path {
                        self.pending_addon_destructive_confirmation =
                            Some(AddonDestructiveConfirmAction::Delete {
                                addon_name,
                                addon_path: path,
                            });
                    }
                }
            }
        }
    }
}
