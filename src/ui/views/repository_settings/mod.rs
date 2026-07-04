mod addon_cache;
mod addons;
mod cached_views;
mod config_identity;
mod config_profiles;
mod config_sync;
mod configuration;
mod export_structure;
mod external_addon_views;
mod external_addons;
mod profiles;

use crate::ui::app::{AddonDestructiveConfirmAction, Foxy, ProfileConfirmAction};
use crate::ui::i18n::{fmt_bytes, tr, tr_fmt};
use crate::ui::types::{FoxyView, Repository, RepositoryProfile, RepositorySettingsTab};
use eframe::egui::{
    self, Align, Align2, Button, CornerRadius, Frame, Label, Layout, Margin, RichText, ScrollArea,
    TextEdit, Ui, Vec2,
};
use log::{info, warn};
use std::path::PathBuf;

pub(super) const PROFILE_CLIPBOARD_HEADER: &str = "FOXYPROF01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AddonContextAction {
    OpenDirectory,
    Backup,
    RestoreBackup,
    RecheckIntegrity,
    StandaloneDownload,
    ForceRedownload,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExternalAddonContextAction {
    OpenDirectory,
    Delete,
}

pub(super) fn normalize_local_path_for_compare(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let path = PathBuf::from(trimmed);
    let normalized_path = path.canonicalize().unwrap_or(path);
    let mut normalized = normalized_path.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("//?/UNC/") {
        normalized = format!("//{stripped}");
    } else if let Some(stripped) = normalized.strip_prefix("//?/") {
        normalized = stripped.to_string();
    }
    while normalized.ends_with('/') {
        normalized.pop();
    }
    if cfg!(windows) {
        normalized = normalized.to_lowercase();
    }
    normalized
}

pub(super) fn should_wipe_cache_after_local_path_change(
    original_path: &str,
    current_path: &str,
) -> bool {
    !original_path.trim().is_empty()
        && normalize_local_path_for_compare(original_path)
            != normalize_local_path_for_compare(current_path)
}

/// Width (in points) that Body-font `text` occupies on a single unwrapped line.
pub(super) fn filter_controls_text_width(ui: &Ui, text: &str) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    ui.painter()
        .layout_no_wrap(text.to_owned(), font_id, egui::Color32::PLACEHOLDER)
        .size()
        .x
}

/// Approximate width a checkbox labelled `text` occupies, using egui's own
/// spacing metrics so the estimate tracks the active theme and DPI.
pub(super) fn filter_controls_checkbox_width(ui: &Ui, text: &str) -> f32 {
    let spacing = ui.spacing();
    spacing.icon_width + spacing.icon_spacing + filter_controls_text_width(ui, text) + 4.0
}

/// Approximate width a combo box button showing `selected` occupies.
pub(super) fn filter_controls_combo_width(ui: &Ui, selected: &str) -> f32 {
    let spacing = ui.spacing();
    (filter_controls_text_width(ui, selected)
        + spacing.icon_width
        + spacing.button_padding.x * 2.0
        + spacing.item_spacing.x)
        .max(spacing.combo_width)
}

/// Width for the filter text field on the combined filter/controls row.
///
/// When the trailing controls (`controls_width`) still leave room for a usable
/// filter field, the field expands to consume the slack so the controls sit
/// toward the right edge. Otherwise it fills the line, which pushes the controls
/// onto their own wrapped line(s) below instead of clipping them off the edge.
///
/// A fixed slack is held back from the filter so estimation error or combo box
/// padding can never pack the trailing controls flush against the right edge
/// (which egui would clip rather than wrap); if that slack cannot be afforded we
/// fall through to filling the line and wrapping the controls below.
pub(super) fn responsive_filter_field_width(available_width: f32, controls_width: f32) -> f32 {
    const MIN_FILTER_WIDTH: f32 = 220.0;
    const CONTROLS_SLACK: f32 = 48.0;
    let beside = available_width - controls_width - CONTROLS_SLACK;
    if beside >= MIN_FILTER_WIDTH {
        beside
    } else {
        available_width.max(MIN_FILTER_WIDTH)
    }
}

impl Foxy {
    fn ensure_valid_selected_profile_for_repository(repo: &mut Repository) -> bool {
        let Some(selected_name) = repo.selected_profile.as_ref() else {
            return false;
        };

        if repo.profiles.iter().any(|p| p.name == *selected_name) {
            return false;
        }

        warn!(
            "Selected profile '{}' was not found for repository '{}'; falling back to default settings",
            selected_name, repo.name
        );
        repo.selected_profile = None;
        true
    }

    pub fn render_repository_settings_view(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if self.selected_repository_for_settings.is_none() {
            self.restore_last_view_or_default();
            return;
        }
        let repo_index = self.selected_repository_for_settings.unwrap();
        if repo_index >= self.repository_view_state.repositories.len() {
            self.restore_last_view_or_default();
            return;
        }
        let current_repository_settings_tab = self.current_repository_settings_tab;
        let outer_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };
        if Self::ensure_valid_selected_profile_for_repository(
            &mut self.repository_view_state.repositories[repo_index],
        ) {
            self.save_repositories();
        }
        self.ensure_repository_addon_size_cache_loaded();
        Frame::NONE.inner_margin(outer_margin).show(ui, |ui| {
            ui.horizontal(|ui| {
                let close_icon_size = self
                    .settings_view_state
                    .font_sizes
                    .repository_settings_view
                    .close_icon as f32;
                let close_btn_size = Self::modal_icon_button_size(close_icon_size);
                let available = ui.available_width();
                let heading_max_width =
                    (available - close_btn_size.x - ui.spacing().item_spacing.x).max(0.0);

                let repository_name = &self.repository_view_state.repositories[repo_index].name;
                let title_text = RichText::new(tr_fmt(
                    "Repository Settings - {repository_name}",
                    &[("repository_name", repository_name.clone())],
                ))
                .size(
                    self.settings_view_state
                        .font_sizes
                        .repository_settings_view
                        .page_title as f32,
                );
                ui.scope(|ui| {
                    ui.set_max_width(heading_max_width);
                    ui.add(Label::new(title_text.strong()).truncate());
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let close_button = ui.add_sized(
                        close_btn_size,
                        Button::new(
                            RichText::new("X")
                                .color(self.color_text_normal())
                                .size(close_icon_size),
                        )
                        .fill(self.color_main_bg()),
                    );
                    if close_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if close_button.clicked() {
                        info!("Closing repository settings view");
                        self.selected_repository_for_settings = None;
                        self.current_view = FoxyView::RepositoryList;
                    }
                });
            });
            ui.separator();
            let mut repository_structure_export_clicked = false;
            {
                self.ensure_repository_addon_list_cache_cached(
                    repo_index,
                    crate::ui::app::RepositoryAddonListKind::Addons,
                );
                self.ensure_repository_addon_list_cache_cached(
                    repo_index,
                    crate::ui::app::RepositoryAddonListKind::OptionalAddons,
                );
                self.ensure_repository_external_addons_base_cache_cached(repo_index);
                let repo = &self.repository_view_state.repositories[repo_index];
                let total_size = self.repository_remote_size_bytes_by_address(&repo.address);
                let addon_count = self
                    .repository_addon_list_cache_cached(
                        crate::ui::app::RepositoryAddonListKind::Addons,
                    )
                    .source_names
                    .len();
                let optional_addon_count = self
                    .repository_addon_list_cache_cached(
                        crate::ui::app::RepositoryAddonListKind::OptionalAddons,
                    )
                    .source_names
                    .len();
                let enabled_repository_size = self
                    .repository_addon_list_enabled_size_bytes_cached(
                        crate::ui::app::RepositoryAddonListKind::Addons,
                    )
                    .saturating_add(self.repository_addon_list_enabled_size_bytes_cached(
                        crate::ui::app::RepositoryAddonListKind::OptionalAddons,
                    ));
                let external_cache = &self.repository_external_addons_list_cache;
                let external_enabled_count = external_cache.enabled_count;
                let external_count = external_cache.rows.len();
                let enabled_launch_size =
                    enabled_repository_size.saturating_add(external_cache.enabled_size_bytes);
                let total_size_text = if enabled_launch_size != total_size {
                    format!(
                        "{} / {}",
                        tr_fmt("Total size: {size}", &[("size", fmt_bytes(total_size))]),
                        fmt_bytes(enabled_launch_size)
                    )
                } else {
                    tr_fmt("Total size: {size}", &[("size", fmt_bytes(total_size))])
                };
                // The summary is split so the "External Addons" segment can carry
                // its own tooltip; the leading metrics share a single label.
                let summary_prefix_text = format!(
                    "{} - {}: {} - {}: {} - ",
                    total_size_text,
                    tr("Addons"),
                    addon_count,
                    tr("Optional Addons"),
                    optional_addon_count,
                );
                let external_addons_text = format!(
                    "{}: {} / {}",
                    tr("External Addons"),
                    external_enabled_count,
                    external_count
                );
                let color_text_dim = self.color_text_dim();
                let external_addons_help = tr("external_addons_quick_info_help");
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(RichText::new(summary_prefix_text).color(color_text_dim));
                    ui.label(RichText::new(external_addons_text).color(color_text_dim))
                        .on_hover_text(external_addons_help);
                    ui.add_space(12.0);
                    let export_structure = ui.button(tr("Export Repository Structure"));
                    if export_structure.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if export_structure.clicked() {
                        repository_structure_export_clicked = true;
                    }
                });
                ui.separator();
            }
            if repository_structure_export_clicked {
                self.export_repository_structure_to_file(repo_index);
            }

            // The header (profile selector, status, tab bar) and the per-tab
            // content are laid out here without an outer ScrollArea. Addon list
            // tabs scroll internally through their own virtualized ScrollArea;
            // nesting them inside an outer vertical ScrollArea would hand the
            // inner list unbounded height and force every row to render each
            // frame (killing scroll performance). Tabs that need scrolling wrap
            // their own content in a ScrollArea below.
            ui.vertical(|ui| {
                ui.vertical(|ui| {
                    let mut profile_updated = false;
                    let mut export_clicked = false;
                    let mut import_clicked = false;
                    {
                        let repo = &mut self.repository_view_state.repositories[repo_index];
                        let old = repo.selected_profile.clone();
                        let default_profile_text = tr("Default");

                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
                            ui.horizontal_wrapped(|ui| {
                            ui.label(tr("Profile:"));

                            let combo = egui::ComboBox::from_label("")
                                .selected_text(
                                    repo.selected_profile
                                        .clone()
                                        .unwrap_or_else(|| default_profile_text.clone()),
                                )
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            repo.selected_profile.is_none(),
                                            default_profile_text.clone(),
                                        )
                                        .clicked()
                                    {
                                        repo.selected_profile = None;
                                    }
                                    for profile in &repo.profiles {
                                        if ui
                                            .selectable_label(
                                                repo.selected_profile.as_deref()
                                                    == Some(&profile.name),
                                                &profile.name,
                                            )
                                            .clicked()
                                        {
                                            repo.selected_profile = Some(profile.name.clone());
                                        }
                                    }
                                });
                            if combo.response.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }

                            let add = ui.button(tr("Add Profile"));
                            if add.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if add.clicked() {
                                self.show_add_profile_window = true;
                                self.new_profile_name.clear();
                                info!("Opening Add Profile dialog");
                            }

                            let copy = ui.button(tr("Copy Profile"));
                            if copy.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if copy.clicked() {
                                if let Some(sel_name) = repo.selected_profile.clone() {
                                    if let Some(orig) =
                                        repo.profiles.iter().find(|p| p.name == sel_name).cloned()
                                    {
                                        let mut new_profile = orig.clone();
                                        let base = format!("{} Copy", sel_name);
                                        let unique =
                                            Self::unique_profile_name(repo, &base, " ");
                                        new_profile.name = unique.clone();
                                        repo.profiles.push(new_profile);
                                        repo.selected_profile = Some(unique);
                                        profile_updated = true;
                                        info!("Copied profile {}", sel_name);
                                    }
                                } else {
                                    let unique =
                                        Self::unique_profile_name(repo, "Default Copy", " ");
                                    let new_profile =
                                        Self::profile_from_repository(repo, unique.clone());
                                    repo.profiles.push(new_profile);
                                    repo.selected_profile = Some(unique);
                                    profile_updated = true;
                                    info!(
                                        "Created profile copy from default repository configuration"
                                    );
                                }
                            }

                            let export = ui.button(tr("Export Profile"));
                            if export.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if export.clicked() {
                                export_clicked = true;
                            }

                            let import = ui.button(tr("Import Profile"));
                            if import.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if import.clicked() {
                                import_clicked = true;
                            }

                            let ren = ui.button(tr("Rename Profile"));
                            if ren.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if ren.clicked()
                                && let Some(sel) = &repo.selected_profile {
                                    self.show_rename_profile_window = true;
                                    self.new_profile_name = sel.clone();
                                    info!("Opening Rename Profile dialog for {}", sel);
                                }

                            let reset = ui.button(tr("Reset Profile"));
                            if reset.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if reset.clicked() {
                                self.pending_profile_confirm_action =
                                    Some(ProfileConfirmAction::Reset {
                                        profile_name: repo.selected_profile.clone(),
                                    });
                            }

                            let del = ui.button(tr("Delete Profile"));
                            if del.hovered() {
                                ui.ctx()
                                    .output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if del.clicked()
                                && let Some(sel_name) = repo.selected_profile.clone() {
                                    self.pending_profile_confirm_action =
                                        Some(ProfileConfirmAction::Delete {
                                            profile_name: sel_name,
                                        });
                                }
                            });
                        });

                        ui.separator();

                        if repo.selected_profile != old {
                            profile_updated = true;
                        }
                    }
                    if export_clicked {
                        self.export_profile_to_clipboard(repo_index);
                    }
                    if import_clicked {
                        self.import_profile_from_clipboard(repo_index);
                    }
                    if profile_updated {
                        self.save_repositories();
                    }

                    ui.separator();

                    if let Some(status) = self
                        .addon_backup_status
                        .as_ref()
                        .filter(|status| status.repo_index == repo_index)
                    {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(status.status_text.clone());
                        });
                        ui.separator();
                    } else if let Some(notice) = self
                        .addon_backup_notice
                        .as_ref()
                        .filter(|notice| notice.repo_index == repo_index)
                    {
                        ui.colored_label(
                            if notice.success {
                                self.color_text_dim()
                            } else {
                                self.color_text_error()
                            },
                            &notice.message,
                        );
                        ui.separator();
                    }

                    {
                        let repo = &self.repository_view_state.repositories[repo_index];
                        if self.is_repository_db_wipe_pending(&repo.address) {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                if self.is_repository_force_redownload_pending(&repo.address) {
                                    ui.label(tr("Force redownload repository"));
                                } else {
                                    ui.label(tr("Repository database wipe in progress"));
                                }
                            });
                            ui.separator();
                        }
                    }

                    let tabs = RepositorySettingsTab::all_tabs();
                    let labels: Vec<&str> = tabs.iter().map(|t| t.as_str()).collect();
                    let selected = tabs
                        .iter()
                        .position(|t| *t == current_repository_settings_tab)
                        .unwrap_or(0);
                    if let Some(idx) = self.render_adaptive_tab_bar(ui, &labels, selected) {
                        self.current_repository_settings_tab = tabs[idx];
                        info!("Switched repository settings tab to {}", tabs[idx].as_str());
                    }
                    ui.separator();

                    let render_tab_inner =
                        |this: &mut Self, ui: &mut Ui| match current_repository_settings_tab {
                            RepositorySettingsTab::Configuration => {
                                this.render_repository_configuration(ui);
                            }
                            RepositorySettingsTab::Addons => {
                                let mut addons_filter = this.addons_filter.clone();
                                let mut addon_state_filter = this.addon_state_filter.clone();
                                this.render_repository_addons_list_cached(
                                    ui,
                                    repo_index,
                                    crate::ui::app::RepositoryAddonListKind::Addons,
                                    &mut addons_filter,
                                    &mut addon_state_filter,
                                );
                                this.addons_filter = addons_filter;
                                this.addon_state_filter = addon_state_filter;
                            }
                            RepositorySettingsTab::OptionalAddons => {
                                let mut optional_addons_filter =
                                    this.optional_addons_filter.clone();
                                let mut addon_state_filter = this.addon_state_filter.clone();
                                this.render_repository_addons_list_cached(
                                    ui,
                                    repo_index,
                                    crate::ui::app::RepositoryAddonListKind::OptionalAddons,
                                    &mut optional_addons_filter,
                                    &mut addon_state_filter,
                                );
                                this.optional_addons_filter = optional_addons_filter;
                                this.addon_state_filter = addon_state_filter;
                            }
                            RepositorySettingsTab::ExternalAddons => {
                                let mut external_addons_filter =
                                    this.external_addons_filter.clone();
                                let mut external_addons_origin_filter =
                                    this.external_addons_origin_filter.clone();
                                let mut external_addons_group_by_origin =
                                    this.external_addons_group_by_origin;
                                let mut addon_state_filter = this.addon_state_filter.clone();
                                this.render_repository_external_addons_list_cached(
                                    ui,
                                    repo_index,
                                    &mut external_addons_filter,
                                    &mut external_addons_origin_filter,
                                    &mut external_addons_group_by_origin,
                                    &mut addon_state_filter,
                                );
                                this.external_addons_filter = external_addons_filter;
                                this.external_addons_origin_filter = external_addons_origin_filter;
                                this.external_addons_group_by_origin =
                                    external_addons_group_by_origin;
                                this.addon_state_filter = addon_state_filter;
                            }
                        };

                    let tab_panel_horizontal_inset = 15.0;
                    let corner_radius = CornerRadius::same(10);

                    // All tabs render in a fixed-height clipped panel (the white
                    // card). Addon list tabs scroll internally through their own
                    // virtualized ScrollArea, so they render directly. The
                    // Configuration tab has no internal scroll, so it wraps its
                    // content in a ScrollArea *inside* the panel, keeping the
                    // scrollbar within the white frame like the other tabs.
                    let scrolls_internally = !matches!(
                        current_repository_settings_tab,
                        RepositorySettingsTab::Addons
                            | RepositorySettingsTab::OptionalAddons
                            | RepositorySettingsTab::ExternalAddons
                    );
                    let stroke = egui::Stroke::new(1.0, self.color_text_gray());
                    let available_rect = ui.available_rect_before_wrap();
                    let panel_size = Vec2::new(
                        (available_rect.width() - (tab_panel_horizontal_inset * 2.0)).max(0.0),
                        available_rect.height().max(120.0),
                    );
                    let panel_rect = egui::Rect::from_min_size(
                        available_rect.min + egui::vec2(tab_panel_horizontal_inset, 0.0),
                        panel_size,
                    );
                    let frame_rect = panel_rect.shrink(1.0);
                    ui.allocate_rect(panel_rect, egui::Sense::hover());
                    let mut panel_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt((
                                "repository_settings_tab_panel",
                                current_repository_settings_tab.as_str(),
                            ))
                            .max_rect(frame_rect)
                            .layout(Layout::top_down(Align::Min)),
                    );
                    panel_ui.set_clip_rect(frame_rect);
                    Frame::NONE
                        .fill(self.color_card_bg())
                        .corner_radius(corner_radius)
                        .inner_margin(Margin::same(15))
                        .show(&mut panel_ui, |ui| {
                            if scrolls_internally {
                                ScrollArea::vertical()
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| render_tab_inner(self, ui));
                            } else {
                                render_tab_inner(self, ui);
                            }
                        });
                    ui.painter().rect_stroke(
                        frame_rect,
                        corner_radius,
                        stroke,
                        egui::StrokeKind::Inside,
                    );
                });
            });
        });

        if self.show_add_profile_window || self.show_rename_profile_window {
            let title = if self.show_rename_profile_window {
                tr("Rename Profile")
            } else {
                tr("Add New Profile")
            };
            let confirm = if self.show_rename_profile_window {
                tr("Rename")
            } else {
                tr("Create")
            };

            egui::Window::new(title)
                .frame(
                    egui::Frame::window(&ui.ctx().global_style())
                        .fill(self.color_card_bg())
                        .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                        .corner_radius(egui::CornerRadius::same(10)),
                )
                .title_bar(true)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(500.0)
                .default_height(250.0)
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(tr("Enter profile name:"));
                        ui.add_space(20.0);
                        ui.add(TextEdit::singleline(&mut self.new_profile_name).char_limit(60));
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                Layout::centered_and_justified(egui::Direction::TopDown),
                                |ui| {
                                    let confirm_btn = ui.button(confirm);
                                    if confirm_btn.hovered() {
                                        ui.ctx().output_mut(|o| {
                                            Foxy::set_pointing_cursor_output(o)
                                        });
                                    }
                                    if confirm_btn.clicked() {
                                        let new_name = self.new_profile_name.trim();
                                        let repo = &mut self.repository_view_state.repositories
                                            [repo_index];
                                        let duplicate =
                                            repo.profiles.iter().any(|p| p.name == new_name);

                                        if new_name.is_empty() || duplicate {
                                            ui.label(
                                                RichText::new(tr(
                                                    "Name must be unique and non-empty",
                                                ))
                                                .color(self.color_text_error()),
                                            );
                                        } else {
                                            if self.show_rename_profile_window {
                                                if let Some(old) = repo.selected_profile.clone() {
                                                    if let Some(p) = repo
                                                        .profiles
                                                        .iter_mut()
                                                        .find(|p| p.name == old)
                                                    {
                                                        p.name = new_name.to_string();
                                                        repo.selected_profile =
                                                            Some(new_name.to_string());
                                                    } else {
                                                        warn!(
                                                            "Rename profile skipped: selected profile '{}' was not found",
                                                            old
                                                        );
                                                    }
                                                } else {
                                                    warn!(
                                                        "Rename profile skipped: no selected profile"
                                                    );
                                                }
                                                self.show_rename_profile_window = false;
                                            } else {
                                                let p = RepositoryProfile {
                                                    name: new_name.to_string(),
                                                    addons: repo
                                                        .addons
                                                        .iter()
                                                        .map(|(name, _)| (name.clone(), true))
                                                        .collect(),
                                                    optional_addons: repo
                                                        .optional_addons
                                                        .iter()
                                                        .map(|(name, _)| (name.clone(), false))
                                                        .collect(),
                                                    optional_addon_favorites: repo
                                                        .optional_addon_favorites
                                                        .clone(),
                                                    optional_addon_client_side: repo
                                                        .optional_addon_client_side
                                                        .clone(),
                                                    external_addons: repo
                                                        .external_addons
                                                        .iter()
                                                        .map(|(name, _, path)| {
                                                            (name.clone(), false, path.clone())
                                                        })
                                                        .collect(),
                                                    external_addon_favorites: repo
                                                        .external_addon_favorites
                                                        .clone(),
                                                    external_addon_client_side: repo
                                                        .external_addon_client_side
                                                        .clone(),
                                                    ..Default::default()
                                                };
                                                repo.profiles.push(p);
                                                repo.selected_profile = Some(new_name.to_string());
                                                self.show_add_profile_window = false;
                                            }

                                            self.new_profile_name.clear();
                                            self.save_repositories();
                                        }
                                    }

                                    let cancel_btn = ui.button(tr("Cancel"));
                                    if cancel_btn.hovered() {
                                        ui.ctx().output_mut(|o| {
                                            Foxy::set_pointing_cursor_output(o)
                                        });
                                    }
                                    if cancel_btn.clicked() {
                                        self.show_add_profile_window = false;
                                        self.show_rename_profile_window = false;
                                        self.new_profile_name.clear();
                                    }
                                },
                            );
                        });
                    });
                });
        }

        self.render_profile_confirm_modal(ui, repo_index);
        self.render_addon_destructive_confirm_modal(ui);
        self.render_addon_backup_restore_modal(ui.ctx(), repo_index);
    }

    fn render_addon_destructive_confirm_modal(&mut self, ui: &mut Ui) {
        let Some(action) = self.pending_addon_destructive_confirmation.clone() else {
            return;
        };

        let (title, message, confirm_label) = match &action {
            AddonDestructiveConfirmAction::ForceRedownload { addon_name, .. } => (
                tr("Confirm Force Redownload"),
                tr_fmt(
                    "Force redownload {name}?\nThis will remove local files and re-download the addon.",
                    &[("name", addon_name.clone())],
                ),
                tr("Force redownload addon"),
            ),
            AddonDestructiveConfirmAction::Delete { addon_name, .. } => (
                tr("Confirm Deletion"),
                tr_fmt(
                    "Are you sure you want to delete {name}?",
                    &[("name", addon_name.clone())],
                ),
                tr("Delete addon"),
            ),
        };

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(560.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(message);
                    ui.add_space(20.0);
                    let confirm_btn =
                        ui.add(Button::new(confirm_label).fill(self.color_action_destructive()));
                    if confirm_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_btn.clicked() {
                        confirm = true;
                    }

                    let cancel_btn = ui.button(tr("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_addon_destructive_confirmation = None;
            return;
        }

        if !confirm {
            return;
        }

        self.pending_addon_destructive_confirmation = None;
        match action {
            AddonDestructiveConfirmAction::ForceRedownload {
                repo_idx,
                addon_name,
                addon_path,
            } => {
                if !self.force_redownload_addon(repo_idx, &addon_name, addon_path.as_deref()) {
                    warn!("Force redownload failed for addon {}", addon_name);
                }
            }
            AddonDestructiveConfirmAction::Delete {
                addon_name,
                addon_path,
            } => {
                self.delete_addon_from_storage_and_database(&addon_name, &addon_path);
            }
        }
    }

    fn render_profile_confirm_modal(&mut self, ui: &mut Ui, repo_index: usize) {
        let Some(action) = self.pending_profile_confirm_action.clone() else {
            return;
        };

        let (title, message, confirm_label) = match &action {
            ProfileConfirmAction::Delete { profile_name } => (
                tr("Confirm Profile Deletion"),
                tr_fmt(
                    "Are you sure you want to delete profile {name}?",
                    &[("name", profile_name.clone())],
                ),
                tr("Delete profile"),
            ),
            ProfileConfirmAction::Reset {
                profile_name: Some(name),
            } => (
                tr("Confirm Profile Reset"),
                tr_fmt(
                    "Are you sure you want to reset profile {name} to defaults?",
                    &[("name", name.clone())],
                ),
                tr("Reset profile"),
            ),
            ProfileConfirmAction::Reset {
                profile_name: None, ..
            } => (
                tr("Confirm Profile Reset"),
                tr("Are you sure you want to reset repository settings to defaults?"),
                tr("Reset profile"),
            ),
        };

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(480.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(message);
                    ui.add_space(20.0);
                    let confirm_btn = ui.button(confirm_label);
                    if confirm_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_btn.clicked() {
                        confirm = true;
                    }

                    let cancel_btn = ui.button(tr("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_profile_confirm_action = None;
            return;
        }

        if !confirm {
            return;
        }

        self.pending_profile_confirm_action = None;
        let success_message: &str;
        let repo = &mut self.repository_view_state.repositories[repo_index];
        match action {
            ProfileConfirmAction::Delete { profile_name } => {
                repo.profiles.retain(|p| p.name != profile_name);
                repo.selected_profile = None;
                self.save_repositories();
                info!("Deleted profile {}", profile_name);
                success_message = "Profile deleted.";
            }
            ProfileConfirmAction::Reset {
                profile_name: Some(name),
            } => {
                if let Some(p) = repo.profiles.iter_mut().find(|p| p.name == name) {
                    let profile_name = p.name.clone();
                    *p = RepositoryProfile::default();
                    p.name = profile_name;
                    p.addons = repo
                        .addons
                        .iter()
                        .map(|(name, _)| (name.clone(), true))
                        .collect();
                    p.optional_addons = repo
                        .optional_addons
                        .iter()
                        .map(|(name, _)| (name.clone(), false))
                        .collect();
                    p.optional_addon_favorites = repo.optional_addon_favorites.clone();
                    p.optional_addon_client_side = repo.optional_addon_client_side.clone();
                    p.external_addons = repo
                        .external_addons
                        .iter()
                        .map(|(name, _, path)| (name.clone(), false, path.clone()))
                        .collect();
                    p.external_addon_favorites = repo.external_addon_favorites.clone();
                    p.external_addon_client_side = repo.external_addon_client_side.clone();
                    self.save_repositories();
                    info!("Reset profile {} to defaults", name);
                }
                success_message = "Profile reset to defaults.";
            }
            ProfileConfirmAction::Reset {
                profile_name: None, ..
            } => {
                repo.csla = false;
                repo.ef = false;
                repo.gm = false;
                repo.rf = false;
                repo.spe = false;
                repo.vn = false;
                repo.ws = false;
                repo.skip_intro = false;
                repo.no_splash = false;
                repo.world_empty = false;
                repo.load_mission_to_memory = false;
                repo.enable_ht = false;
                repo.huge_pages = false;
                repo.no_logs = false;
                repo.additional_params.clear();
                repo.addons = repo
                    .addons
                    .iter()
                    .map(|(name, _)| (name.clone(), true))
                    .collect();
                repo.optional_addons = repo
                    .optional_addons
                    .iter()
                    .map(|(name, _)| (name.clone(), false))
                    .collect();
                repo.optional_addon_favorites.clear();
                repo.optional_addon_client_side.clear();
                repo.external_addons = repo
                    .external_addons
                    .iter()
                    .map(|(name, _, path)| (name.clone(), false, path.clone()))
                    .collect();
                repo.external_addon_favorites.clear();
                repo.external_addon_client_side.clear();
                self.save_repositories();
                info!("Reset repository settings to defaults");
                success_message = "Repository settings reset to defaults.";
            }
        }
        self.show_success_toast(self.t(success_message));
    }

    fn render_addon_backup_restore_modal(&mut self, ctx: &egui::Context, repo_index: usize) {
        let mut restore_request: Option<crate::ui::app::AddonBackupRestoreState> = None;
        let mut close_restore_modal = false;

        if let Some(mut restore_state) = self
            .addon_backup_restore_state
            .clone()
            .filter(|state| state.repo_index == repo_index)
        {
            egui::Window::new(tr("Restore addon backup"))
                .frame(
                    egui::Frame::window(&ctx.global_style())
                        .fill(self.color_card_bg())
                        .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                        .corner_radius(CornerRadius::same(10)),
                )
                .title_bar(true)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(560.0)
                .show(ctx, |ui| {
                    ui.label(tr_fmt(
                        "Choose addon backup to restore for {name}.",
                        &[("name", restore_state.addon_name.clone())],
                    ));
                    ui.add_space(10.0);

                    let selected_label = restore_state
                        .backups
                        .get(restore_state.selected_backup_index)
                        .map(|backup| backup.folder_name.clone())
                        .unwrap_or_default();
                    let combo = egui::ComboBox::from_id_salt((
                        "restore_addon_backup_selector",
                        repo_index,
                        restore_state.addon_name.clone(),
                    ))
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        for (index, backup) in restore_state.backups.iter().enumerate() {
                            let response = ui.selectable_label(
                                restore_state.selected_backup_index == index,
                                format!("{} ({})", backup.folder_name, backup.content_hash),
                            );
                            if response.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response.clicked() {
                                restore_state.selected_backup_index = index;
                            }
                        }
                    });
                    if combo.response.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }

                    if let Some(selected_backup) = restore_state
                        .backups
                        .get(restore_state.selected_backup_index)
                    {
                        ui.add_space(8.0);
                        ui.label(tr_fmt(
                            "Selected backup hash: {hash}",
                            &[("hash", selected_backup.content_hash.clone())],
                        ));
                    }

                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        let restore_button = ui.button(tr("Restore selected backup"));
                        if restore_button.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if restore_button.clicked() {
                            restore_request = Some(restore_state.clone());
                            close_restore_modal = true;
                        }

                        let cancel_button = ui.button(tr("Cancel"));
                        if cancel_button.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if cancel_button.clicked() {
                            close_restore_modal = true;
                        }
                    });
                });

            if close_restore_modal {
                self.addon_backup_restore_state = None;
            } else {
                self.addon_backup_restore_state = Some(restore_state);
            }
        }

        if let Some(restore_state) = restore_request {
            self.start_manual_addon_restore(restore_state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_local_path_for_compare_strips_trailing_separators() {
        assert_eq!(
            normalize_local_path_for_compare("C:/mods/tfr///"),
            normalize_local_path_for_compare("C:/mods/tfr")
        );
    }

    #[test]
    fn normalize_local_path_for_compare_canonicalizes_existing_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dotted = dir.path().join(".");

        assert_eq!(
            normalize_local_path_for_compare(&dotted.display().to_string()),
            normalize_local_path_for_compare(&dir.path().display().to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_local_path_for_compare_uses_windows_case_rules() {
        assert_eq!(
            normalize_local_path_for_compare("C:\\Mods\\TFR\\"),
            normalize_local_path_for_compare("c:/mods/tfr")
        );
    }

    #[test]
    fn initial_repository_path_assignment_does_not_request_cache_wipe() {
        assert!(!should_wipe_cache_after_local_path_change(
            "",
            "C:/mods/tfr"
        ));
    }

    #[test]
    fn existing_repository_path_change_requests_cache_wipe() {
        assert!(should_wipe_cache_after_local_path_change(
            "C:/mods/tfr-old",
            "C:/mods/tfr-new"
        ));
    }
}
