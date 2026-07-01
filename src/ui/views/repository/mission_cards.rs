use super::{RepositoryMissionContextAction, arma3_editor_display_name};
use crate::core::arma3_missions::{
    EditorMission, format_mission_date, remove_mission_addon_dependencies,
};
use crate::ui::app::{
    Foxy, PendingMissionDeleteState, PendingMissionDuplicateState,
    PendingMissionRemoveDependenciesState,
};
use crate::ui::context_menu::{ContextMenuItem, attach_context_menu};
use crate::ui::i18n::{locale_compare, tr, tr_fmt};
use crate::ui::search_filter::MultiEntryFilter;
use crate::ui::types::{CachedMissionList, RepositorySelection};
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align2, CornerRadius, CursorIcon, ScrollArea, Sense, TextEdit, TextStyle, Ui, Vec2,
};
use log::{info, warn};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone)]
enum MissionListEntry<'a> {
    ParentFolder,
    Folder {
        name: String,
        path: String,
    },
    Mission {
        index: usize,
        mission: &'a EditorMission,
    },
}

const MISSION_ROW_INNER_HEIGHT: f32 = 42.0;
const MISSION_ROW_SPACING: f32 = 2.0;
const MISSION_ROW_HEIGHT: f32 = MISSION_ROW_INNER_HEIGHT + MISSION_ROW_SPACING;
const MISSION_SECTION_BOTTOM_MARGIN: f32 = 8.0;

impl Foxy {
    pub(super) fn visible_editor_mission_entry_count(
        &mut self,
        selected_idx: usize,
    ) -> Option<usize> {
        let profile_name = self.resolve_arma3_profile_for_repo(selected_idx)?;
        let missions = self.get_or_scan_missions(&profile_name);

        if missions.is_empty() {
            return Some(0);
        }

        Some(
            self.visible_mission_entries_for_current_filters(&missions)
                .len(),
        )
    }

    pub(super) fn repository_editor_mission_min_section_height(
        &self,
        ui: &Ui,
        visible_entry_count: Option<usize>,
    ) -> f32 {
        let heading_height =
            ui.text_style_height(&TextStyle::Heading) + ui.spacing().item_spacing.y;
        let body_height = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;

        match visible_entry_count {
            Some(count) if count > 0 => {
                14.0 + heading_height
                    + ui.spacing().interact_size.y
                    + 4.0
                    + count.min(2) as f32 * MISSION_ROW_HEIGHT
                    + MISSION_SECTION_BOTTOM_MARGIN
            }
            Some(_) | None => 14.0 + heading_height + body_height + MISSION_SECTION_BOTTOM_MARGIN,
        }
    }

    /// Render the "Editor Missions" section in the repository detail view.
    ///
    /// Called from `render_repository_view()` after `render_repository_servers_section()`.
    pub(super) fn render_editor_missions_section(
        &mut self,
        ui: &mut Ui,
        selected_idx: usize,
        max_section_height: Option<f32>,
    ) {
        let section_start_y = ui.cursor().min.y;
        ui.add_space(14.0);

        let profile_name = self.resolve_arma3_profile_for_repo(selected_idx);
        let Some(profile_name) = profile_name else {
            ui.heading(tr("Editor Missions"));
            ui.label(tr("No Arma 3 profile detected."));
            return;
        };

        let profile_display_name = arma3_editor_display_name(&profile_name);
        ui.heading(format!(
            "{} - {}",
            tr("Editor Missions"),
            profile_display_name
        ));

        let missions = self.get_or_scan_missions(&profile_name);

        if missions.is_empty() {
            ui.label(tr_fmt(
                "No editor missions found for profile \"{profile}\".",
                &[("profile", profile_display_name)],
            ));
            return;
        }

        let terrain_search_query = MultiEntryFilter::parse(&self.editor_mission_search);
        let show_folders = self.editor_mission_show_folders;
        let mut terrain_options: Vec<String> = missions
            .iter()
            .filter(|m| {
                (show_folders || m.relative_parent.as_os_str().is_empty())
                    && Self::mission_matches_query(m, &terrain_search_query)
            })
            .map(|m| m.world_name.clone())
            .filter(|w| !w.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        terrain_options.sort_by(|a, b| locale_compare(a, b));
        if !self.editor_mission_terrain_filter.is_empty()
            && !terrain_options
                .iter()
                .any(|w| w.eq_ignore_ascii_case(&self.editor_mission_terrain_filter))
        {
            self.editor_mission_terrain_filter.clear();
        }

        ui.horizontal(|ui| {
            let available = ui.available_width();
            let search_width = (available * 0.5).max(180.0);
            let search_response = ui.add(
                TextEdit::singleline(&mut self.editor_mission_search)
                    .hint_text(tr("Search missions"))
                    .desired_width(search_width),
            );
            if search_response.hovered() {
                ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
            }

            let folders_toggle = Self::ui_state_checkbox(
                ui,
                &mut self.editor_mission_show_folders,
                tr("Show folders"),
            );
            if folders_toggle.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if folders_toggle.changed() && !self.editor_mission_show_folders {
                self.editor_mission_folder.clear();
            }

            let selected_text = if self.editor_mission_terrain_filter.is_empty() {
                tr("All terrains")
            } else {
                self.editor_mission_terrain_filter.clone()
            };
            let terrain_combo =
                egui::ComboBox::from_id_salt(("editor_mission_terrain_filter", selected_idx))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        let all_response = ui.selectable_label(
                            self.editor_mission_terrain_filter.is_empty(),
                            tr("All terrains"),
                        );
                        if all_response.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if all_response.clicked() {
                            self.editor_mission_terrain_filter.clear();
                        }
                        for terrain in &terrain_options {
                            let is_selected = self
                                .editor_mission_terrain_filter
                                .eq_ignore_ascii_case(terrain);
                            let response = ui.selectable_label(is_selected, terrain);
                            if response.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response.clicked() {
                                self.editor_mission_terrain_filter = terrain.clone();
                            }
                        }
                    });
            if terrain_combo.response.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
        });
        ui.add_space(4.0);

        let current_folder = self.editor_mission_folder.clone();
        let entries = self.visible_mission_entries_for_current_filters(&missions);

        let content_height = entries.len().max(1) as f32 * MISSION_ROW_HEIGHT;
        let section_used_height = ui.cursor().min.y - section_start_y;
        let mission_height_budget = max_section_height
            .map(|height| {
                (height - section_used_height - MISSION_SECTION_BOTTOM_MARGIN)
                    .max(MISSION_ROW_HEIGHT)
            })
            .unwrap_or_else(|| ui.available_height().max(MISSION_ROW_HEIGHT));
        let mission_list_height = content_height.min(mission_height_budget);

        let (repo_name, effective) = {
            let repo = &self.repository_view_state.repositories[selected_idx];
            (
                repo.name.clone(),
                Self::build_effective_repository_snapshot(repo),
            )
        };
        let mut new_selection = self.repository_selection.clone();
        if let Some(RepositorySelection::Mission(selected_mission_idx)) = &new_selection {
            let selected_is_visible = entries.iter().any(|entry| {
                matches!(entry, MissionListEntry::Mission { index, .. } if *index == *selected_mission_idx)
            });
            if !selected_is_visible {
                new_selection = None;
            }
        }

        if entries.is_empty() {
            ui.label(tr("No editor missions match the current search."));
            ui.add_space(MISSION_SECTION_BOTTOM_MARGIN);
            self.repository_selection = new_selection;
            return;
        }

        // Mission rows paint their text from a persistent galley cache so
        // scrolling never re-shapes the name/terrain/date; see `galley_cache`.
        let name_font = TextStyle::Body.resolve(ui.style());
        let badge_base_font = TextStyle::Button.resolve(ui.style());
        let date_font = TextStyle::Small.resolve(ui.style());
        {
            let scanned_at = self.cached_missions.as_ref().map(|cache| cache.scanned_at);
            self.mission_row_galleys.ensure(
                scanned_at,
                missions.len(),
                name_font.size,
                badge_base_font.size,
                date_font.size,
            );
        }

        ScrollArea::vertical()
            .id_salt(("editor_missions_list", selected_idx))
            .max_height(mission_list_height)
            .auto_shrink([false, false])
            .show_rows(ui, MISSION_ROW_HEIGHT, entries.len(), |ui, row_range| {
                ui.set_min_width(ui.available_width());
                for row_idx in row_range {
                    let entry = &entries[row_idx];
                    let row_id =
                        ui.make_persistent_id(("editor_mission_row", selected_idx, row_idx));
                    let is_selected = match &entry {
                        MissionListEntry::Mission { index, .. } => matches!(
                            &self.repository_selection,
                            Some(RepositorySelection::Mission(idx)) if *idx == *index
                        ),
                        _ => false,
                    };

                    let row_size = Vec2::new(ui.available_width(), MISSION_ROW_INNER_HEIGHT);
                    let rect = egui::Rect::from_min_size(ui.cursor().min, row_size);
                    let response = ui.interact(rect, row_id, Sense::click());
                    ui.advance_cursor_after_rect(rect);

                    let painter = ui.painter();
                    let corner = CornerRadius::same(3);
                    let fill = if response.hovered() {
                        self.color_widget_bg_active()
                    } else {
                        self.color_widget_bg()
                    };
                    let stroke_color = if is_selected {
                        self.color_primary_accent()
                    } else if response.hovered() {
                        self.color_primary_accent_hover()
                    } else {
                        self.color_text_gray()
                    };
                    let stroke_width = if is_selected { 1.25 } else { 1.0 };

                    painter.rect_filled(rect, corner, fill);
                    painter.rect_stroke(
                        rect,
                        corner,
                        egui::Stroke::new(stroke_width, stroke_color),
                        egui::StrokeKind::Inside,
                    );

                    let padding = 10.0;

                    match entry {
                        MissionListEntry::ParentFolder => {
                            painter.text(
                                egui::pos2(rect.left() + padding, rect.center().y),
                                Align2::LEFT_CENTER,
                                "../",
                                TextStyle::Body.resolve(ui.style()),
                                self.color_text_normal(),
                            );
                        }
                        MissionListEntry::Folder { name, .. } => {
                            let folder_display_name = arma3_editor_display_name(name);
                            painter.text(
                                egui::pos2(rect.left() + padding, rect.center().y),
                                Align2::LEFT_CENTER,
                                format!("{}/", folder_display_name),
                                TextStyle::Body.resolve(ui.style()),
                                self.color_text_normal(),
                            );
                        }
                        MissionListEntry::Mission { index, mission } => {
                            let badge_horizontal_padding = 12.0;
                            let badge_min_width = 60.0;
                            let badge_max_width = (rect.width() * 0.5).max(badge_min_width);
                            let base_font_size = badge_base_font.size;
                            let min_font_size = (base_font_size * 0.6).max(9.0);
                            let badge_text_color = self.color_text_normal();
                            let badge_bg = self.color_server_offline_bg();
                            let date_color = self.color_text_dim();
                            let name_color = self.color_text_normal();
                            let mission_index = *index;

                            let world_base_galley = galley_cache::lazy_galley(
                                ui,
                                self.mission_row_galleys.world_slot(mission_index),
                                badge_base_font.clone(),
                                || mission.world_name.clone(),
                            );
                            let date_galley = galley_cache::lazy_galley(
                                ui,
                                self.mission_row_galleys.date_slot(mission_index),
                                date_font.clone(),
                                || format_mission_date(mission.last_modified),
                            );
                            let name_galley = galley_cache::lazy_galley(
                                ui,
                                self.mission_row_galleys.name_slot(mission_index),
                                name_font.clone(),
                                || arma3_editor_display_name(&mission.display_name),
                            );

                            let base_text_width = world_base_galley.size().x;
                            let desired_badge_width =
                                (base_text_width + badge_horizontal_padding).max(badge_min_width);
                            let badge_width = desired_badge_width.min(badge_max_width);
                            // A long terrain name on a narrow window needs a smaller
                            // font; that galley is width-dependent and uncommon, so it
                            // is shaped live rather than cached.
                            let badge_galley = if desired_badge_width > badge_max_width
                                && base_text_width > 0.0
                            {
                                let available_text_width =
                                    (badge_max_width - badge_horizontal_padding).max(0.0);
                                let scaled_size = (base_font_size
                                    * (available_text_width / base_text_width))
                                    .clamp(min_font_size, base_font_size);
                                ui.painter().layout_no_wrap(
                                    mission.world_name.clone(),
                                    egui::FontId::new(scaled_size, badge_base_font.family.clone()),
                                    egui::Color32::PLACEHOLDER,
                                )
                            } else {
                                world_base_galley
                            };
                            let badge_rect = egui::Rect::from_center_size(
                                egui::pos2(
                                    rect.right() - padding - badge_width * 0.5,
                                    rect.center().y,
                                ),
                                Vec2::new(badge_width, 22.0),
                            );
                            painter.rect_filled(badge_rect, CornerRadius::same(4), badge_bg);
                            galley_cache::paint_anchored(
                                ui,
                                badge_rect.center(),
                                Align2::CENTER_CENTER,
                                badge_galley,
                                badge_text_color,
                                Some(badge_rect),
                            );

                            let date_pos = egui::pos2(badge_rect.left() - 10.0, rect.center().y);
                            galley_cache::paint_anchored(
                                ui,
                                date_pos,
                                Align2::RIGHT_CENTER,
                                date_galley,
                                date_color,
                                None,
                            );

                            let name_rect = egui::Rect::from_min_max(
                                egui::pos2(rect.left() + padding, rect.top()),
                                egui::pos2(date_pos.x - 10.0, rect.bottom()),
                            );
                            galley_cache::paint_anchored(
                                ui,
                                egui::pos2(name_rect.left(), rect.center().y),
                                Align2::LEFT_CENTER,
                                name_galley,
                                name_color,
                                Some(name_rect),
                            );
                        }
                    }

                    if response.clicked() {
                        match entry {
                            MissionListEntry::ParentFolder => {
                                self.editor_mission_folder =
                                    Self::mission_parent_folder(&current_folder);
                                new_selection = None;
                            }
                            MissionListEntry::Folder { path, .. } => {
                                self.editor_mission_folder = path.clone();
                                new_selection = None;
                            }
                            MissionListEntry::Mission { index, .. } => {
                                new_selection = Some(RepositorySelection::Mission(*index));
                            }
                        }
                    }
                    if response.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }

                    if let MissionListEntry::Mission { index, mission } = entry {
                        let mut context_action = None;
                        attach_context_menu(
                            &response,
                            &[
                                ContextMenuItem::new(
                                    RepositoryMissionContextAction::OpenFolder,
                                    tr("Open folder"),
                                ),
                                ContextMenuItem::new(
                                    RepositoryMissionContextAction::OpenInEditor,
                                    tr("Launch Editor"),
                                )
                                .separator_before(),
                                ContextMenuItem::new(
                                    RepositoryMissionContextAction::Duplicate,
                                    tr("Duplicate mission"),
                                )
                                .separator_before(),
                                ContextMenuItem::new(
                                    RepositoryMissionContextAction::RemoveDependencies,
                                    tr("Remove dependencies"),
                                )
                                .separator_before(),
                                ContextMenuItem::new(
                                    RepositoryMissionContextAction::Delete,
                                    tr("Delete mission"),
                                )
                                .danger()
                                .separator_before(),
                            ],
                            &mut context_action,
                        );

                        match context_action {
                            Some(RepositoryMissionContextAction::OpenFolder) => {
                                new_selection = Some(RepositorySelection::Mission(*index));
                                let _ = self.open_editor_mission_folder(
                                    mission.display_name.as_str(),
                                    mission.path.as_path(),
                                );
                            }
                            Some(RepositoryMissionContextAction::OpenInEditor) => {
                                new_selection = Some(RepositorySelection::Mission(*index));
                                self.request_editor_mission_launch(
                                    ui.ctx(),
                                    &effective,
                                    mission,
                                    selected_idx,
                                    &repo_name,
                                );
                            }
                            Some(RepositoryMissionContextAction::RemoveDependencies) => {
                                new_selection = Some(RepositorySelection::Mission(*index));
                                self.begin_remove_mission_dependencies(selected_idx, mission);
                            }
                            Some(RepositoryMissionContextAction::Duplicate) => {
                                new_selection = Some(RepositorySelection::Mission(*index));
                                self.begin_duplicate_mission(selected_idx, &profile_name, mission);
                            }
                            Some(RepositoryMissionContextAction::Delete) => {
                                new_selection = Some(RepositorySelection::Mission(*index));
                                self.begin_delete_mission(selected_idx, &profile_name, mission);
                            }
                            None => {}
                        }
                    }

                    ui.add_space(MISSION_ROW_SPACING);
                }
            });
        ui.add_space(MISSION_SECTION_BOTTOM_MARGIN);

        self.repository_selection = new_selection;
    }

    pub(super) fn remove_mission_dependencies(&mut self, mission: &EditorMission) {
        match remove_mission_addon_dependencies(mission.sqm_path.as_path()) {
            Ok(_) => {
                info!(
                    "Removed editor mission addon dependencies for {}",
                    mission.folder_name
                );
                self.cached_missions = None;
                self.show_success_toast(self.t_fmt(
                    "Removed mission dependencies from {name}.",
                    &[("name", arma3_editor_display_name(&mission.display_name))],
                ));
            }
            Err(err) => {
                warn!(
                    "Failed to remove editor mission addon dependencies for {}: {}",
                    mission.folder_name, err
                );
                self.show_error_toast(self.t("Failed to remove mission dependencies."));
            }
        }
    }

    /// Determine which Arma 3 profile to use for a given repository.
    pub(crate) fn resolve_arma3_profile_for_repo(&self, repo_idx: usize) -> Option<String> {
        let repo = self.repository_view_state.repositories.get(repo_idx)?;

        // 1. Repository-specific override
        if let Some(ref profile) = repo.arma3_profile
            && self
                .detected_arma3_profiles
                .iter()
                .any(|p| &p.name == profile)
        {
            return Some(profile.clone());
        }

        // 2. Auto-detected active profile
        if let Some(ref active) = self.detected_active_arma3_profile {
            return Some(active.clone());
        }

        // 3. Default profile
        self.detected_arma3_profiles
            .iter()
            .find(|p| p.is_default)
            .map(|p| p.name.clone())
    }

    /// Get cached missions or scan the profile directory.
    pub(crate) fn get_or_scan_missions(&mut self, profile_name: &str) -> Vec<EditorMission> {
        let cache_ttl = Duration::from_secs(30);

        if let Some(ref cache) = self.cached_missions
            && cache.profile_name == profile_name
            && cache.scanned_at.elapsed() < cache_ttl
        {
            return cache.missions.clone();
        }

        let profile_path = self
            .detected_arma3_profiles
            .iter()
            .find(|p| p.name == profile_name)
            .map(|p| p.path.clone());

        let missions = match profile_path {
            Some(path) => crate::core::arma3_missions::scan_profile_missions(&path),
            None => Vec::new(),
        };

        self.cached_missions = Some(CachedMissionList {
            profile_name: profile_name.to_string(),
            missions: missions.clone(),
            scanned_at: Instant::now(),
        });

        missions
    }

    fn visible_mission_entries_for_current_filters<'a>(
        &self,
        missions: &'a [EditorMission],
    ) -> Vec<MissionListEntry<'a>> {
        let search_query = MultiEntryFilter::parse(&self.editor_mission_search);
        let current_folder = self.editor_mission_folder.clone();
        let terrain_filter = self.editor_mission_terrain_filter.as_str();

        if self.editor_mission_show_folders {
            if search_query.is_empty() && terrain_filter.is_empty() {
                return Self::mission_folder_entries(missions, current_folder.as_str());
            }

            return missions
                .iter()
                .enumerate()
                .filter(|(_, mission)| {
                    Self::mission_matches_query(mission, &search_query)
                        && Self::mission_matches_terrain(mission, terrain_filter)
                })
                .map(|(index, mission)| MissionListEntry::Mission { index, mission })
                .collect();
        }

        missions
            .iter()
            .enumerate()
            .filter(|(_, mission)| {
                mission.relative_parent.as_os_str().is_empty()
                    && Self::mission_matches_query(mission, &search_query)
                    && Self::mission_matches_terrain(mission, terrain_filter)
            })
            .map(|(index, mission)| MissionListEntry::Mission { index, mission })
            .collect()
    }

    fn mission_matches_terrain(mission: &EditorMission, terrain: &str) -> bool {
        terrain.is_empty() || mission.world_name.eq_ignore_ascii_case(terrain)
    }

    fn mission_folder_entries<'a>(
        missions: &'a [EditorMission],
        current_folder: &str,
    ) -> Vec<MissionListEntry<'a>> {
        let mut entries = Vec::new();
        if !current_folder.is_empty() {
            entries.push(MissionListEntry::ParentFolder);
        }

        let current_path = Path::new(current_folder);
        let mut folders = BTreeSet::new();
        for mission in missions {
            let relative_parent = mission.relative_parent.as_path();
            if let Ok(rest) = relative_parent.strip_prefix(current_path)
                && rest.components().count() > 0
                && let Some(first) = rest.components().next()
            {
                let folder_name = first.as_os_str().to_string_lossy().to_string();
                let folder_path = if current_folder.is_empty() {
                    folder_name.clone()
                } else {
                    Path::new(current_folder)
                        .join(&folder_name)
                        .to_string_lossy()
                        .to_string()
                };
                folders.insert((folder_name, folder_path));
            }
        }

        entries.extend(
            folders
                .into_iter()
                .map(|(name, path)| MissionListEntry::Folder { name, path }),
        );

        entries.extend(
            missions
                .iter()
                .enumerate()
                .filter(|(_, mission)| mission.relative_parent == current_path)
                .map(|(index, mission)| MissionListEntry::Mission { index, mission }),
        );

        entries
    }

    fn mission_matches_query(mission: &EditorMission, query: &MultiEntryFilter) -> bool {
        let relative_parent = mission.relative_parent.to_string_lossy();
        let display_name = arma3_editor_display_name(&mission.display_name);
        let folder_name = arma3_editor_display_name(&mission.folder_name);
        let parent_display_name = arma3_editor_display_name(&relative_parent);

        query.matches_any(&[
            mission.display_name.as_str(),
            mission.folder_name.as_str(),
            display_name.as_str(),
            folder_name.as_str(),
            mission.world_name.as_str(),
            relative_parent.as_ref(),
            parent_display_name.as_str(),
        ])
    }

    fn mission_parent_folder(current_folder: &str) -> String {
        Path::new(current_folder)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    pub(super) fn begin_duplicate_mission(
        &mut self,
        repo_idx: usize,
        profile_name: &str,
        mission: &EditorMission,
    ) {
        self.pending_mission_delete = None;
        self.pending_mission_remove_dependencies = None;
        let existing = self.get_or_scan_missions(profile_name);
        let suggested_name = Self::resolve_duplicate_prefix(mission, "", &existing)
            .unwrap_or_else(|_| mission.display_name.clone());
        self.pending_mission_duplicate = Some(PendingMissionDuplicateState {
            repo_idx,
            profile_name: profile_name.to_string(),
            mission: mission.clone(),
            name_input: String::new(),
            suggested_name,
            error: None,
        });
    }

    pub(super) fn begin_delete_mission(
        &mut self,
        repo_idx: usize,
        profile_name: &str,
        mission: &EditorMission,
    ) {
        self.pending_mission_duplicate = None;
        self.pending_mission_remove_dependencies = None;
        self.pending_mission_delete = Some(PendingMissionDeleteState {
            repo_idx,
            profile_name: profile_name.to_string(),
            mission: mission.clone(),
            error: None,
        });
    }

    pub(super) fn begin_remove_mission_dependencies(
        &mut self,
        repo_idx: usize,
        mission: &EditorMission,
    ) {
        self.pending_mission_duplicate = None;
        self.pending_mission_delete = None;
        self.pending_mission_remove_dependencies = None;
        self.pending_mission_remove_dependencies = Some(PendingMissionRemoveDependenciesState {
            repo_idx,
            mission: mission.clone(),
            error: None,
        });
    }

    pub(super) fn duplicate_mission_from_pending(
        &mut self,
        pending: &PendingMissionDuplicateState,
    ) -> Result<String, String> {
        let existing = self.get_or_scan_missions(&pending.profile_name);
        let (_, terrain_name) = Self::split_mission_folder_name(&pending.mission.folder_name)
            .ok_or_else(|| "Mission folder name format is invalid.".to_string())?;
        let target_prefix = Self::resolve_duplicate_prefix(
            &pending.mission,
            pending.name_input.as_str(),
            &existing,
        )?;
        let target_folder_name = format!("{}.{}", target_prefix, terrain_name);

        let source_folder = pending.mission.path.as_path();
        let target_parent = source_folder.parent().ok_or_else(|| {
            format!(
                "Cannot determine mission parent folder for {}",
                pending.mission.folder_name
            )
        })?;
        let target_folder = target_parent.join(&target_folder_name);
        if target_folder.exists() {
            return Err(format!(
                "Target mission folder already exists: {}",
                target_folder_name
            ));
        }

        Self::copy_directory_recursive(source_folder, target_folder.as_path())?;
        info!(
            "Duplicated mission {} to {}",
            pending.mission.folder_name, target_folder_name
        );

        self.cached_missions = None;
        let refreshed = self.get_or_scan_missions(&pending.profile_name);
        if let Some(new_idx) = refreshed.iter().position(|m| {
            m.folder_name.eq_ignore_ascii_case(&target_folder_name)
                && m.path.parent() == Some(target_parent)
        }) {
            self.repository_selection = Some(RepositorySelection::Mission(new_idx));
        }
        self.pending_mission_duplicate = None;
        Ok(target_folder_name)
    }

    pub(super) fn delete_mission_from_pending(
        &mut self,
        pending: &PendingMissionDeleteState,
    ) -> Result<String, String> {
        let mission_folder = pending.mission.path.as_path();
        if !mission_folder.exists() {
            return Err(format!(
                "Mission folder does not exist: {}",
                mission_folder.display()
            ));
        }
        if !mission_folder.is_dir() {
            return Err(format!(
                "Mission path is not a directory: {}",
                mission_folder.display()
            ));
        }

        let root_name = pending.mission.root_folder_name.to_ascii_lowercase();
        let valid_root = root_name == "missions" || root_name == "mpmissions";
        let under_root = mission_folder.ancestors().any(|ancestor| {
            ancestor
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(&root_name))
        });
        if !valid_root || !under_root {
            return Err("Mission folder is outside the expected missions directories.".to_string());
        }

        fs::remove_dir_all(mission_folder).map_err(|err| {
            format!(
                "Failed to delete mission folder {}: {}",
                mission_folder.display(),
                err
            )
        })?;
        info!("Deleted mission {}", pending.mission.folder_name);

        self.cached_missions = None;
        let refreshed = self.get_or_scan_missions(&pending.profile_name);
        if let Some(RepositorySelection::Mission(selected_idx)) = self.repository_selection.clone()
        {
            if refreshed.is_empty() {
                self.repository_selection = None;
            } else {
                self.repository_selection = Some(RepositorySelection::Mission(
                    selected_idx.min(refreshed.len() - 1),
                ));
            }
        }
        self.pending_mission_duplicate = None;
        self.pending_mission_delete = None;
        Ok(pending.mission.folder_name.clone())
    }

    fn split_mission_folder_name(folder_name: &str) -> Option<(&str, &str)> {
        let (name, terrain) = folder_name.rsplit_once('.')?;
        let name = name.trim();
        let terrain = terrain.trim();
        if name.is_empty() || terrain.is_empty() {
            return None;
        }
        Some((name, terrain))
    }

    fn strip_version_suffix(name: &str) -> &str {
        if let Some((base, suffix)) = name.rsplit_once("_v")
            && !base.is_empty()
            && !suffix.is_empty()
            && suffix.chars().all(|ch| ch.is_ascii_digit())
        {
            return base;
        }
        name
    }

    fn resolve_duplicate_prefix(
        mission: &EditorMission,
        requested_name: &str,
        existing: &[EditorMission],
    ) -> Result<String, String> {
        let (source_prefix, terrain_name) =
            Self::split_mission_folder_name(&mission.folder_name)
                .ok_or_else(|| "Mission folder name format is invalid.".to_string())?;
        let requested_trimmed = requested_name.trim();

        let mut candidate_prefix = if requested_trimmed.is_empty() {
            let base_prefix = Self::strip_version_suffix(source_prefix);
            let version = Self::next_duplicate_version(base_prefix, terrain_name, existing);
            format!("{}_v{}", base_prefix, version)
        } else {
            let custom = requested_trimmed
                .split('.')
                .next()
                .map(str::trim)
                .unwrap_or_default();
            if custom.is_empty() {
                let base_prefix = Self::strip_version_suffix(source_prefix);
                let version = Self::next_duplicate_version(base_prefix, terrain_name, existing);
                format!("{}_v{}", base_prefix, version)
            } else {
                custom.to_string()
            }
        };

        if candidate_prefix.contains(['\\', '/']) {
            return Err("Mission name cannot contain path separators.".to_string());
        }
        if candidate_prefix == "." || candidate_prefix == ".." {
            return Err("Mission name is invalid.".to_string());
        }

        let requested_folder = format!("{}.{}", candidate_prefix, terrain_name);
        if existing
            .iter()
            .any(|m| m.folder_name.eq_ignore_ascii_case(&requested_folder))
        {
            let version_base = Self::strip_version_suffix(&candidate_prefix).to_string();
            let version = Self::next_duplicate_version(&version_base, terrain_name, existing);
            candidate_prefix = format!("{}_v{}", version_base, version);
        }

        Ok(candidate_prefix)
    }

    fn next_duplicate_version(
        base_prefix: &str,
        terrain_name: &str,
        existing: &[EditorMission],
    ) -> u32 {
        existing
            .iter()
            .filter_map(|mission| {
                let (prefix, terrain) = Self::split_mission_folder_name(&mission.folder_name)?;
                if !terrain.eq_ignore_ascii_case(terrain_name) {
                    return None;
                }
                let (stem, suffix) = prefix.rsplit_once("_v")?;
                if !stem.eq_ignore_ascii_case(base_prefix) {
                    return None;
                }
                suffix.parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    fn copy_directory_recursive(source: &Path, target: &Path) -> Result<(), String> {
        if !source.is_dir() {
            return Err(format!(
                "Mission source path is not a directory: {}",
                source.display()
            ));
        }

        fs::create_dir_all(target)
            .map_err(|err| format!("Failed to create {}: {}", target.display(), err))?;
        let entries = fs::read_dir(source)
            .map_err(|err| format!("Failed to read {}: {}", source.display(), err))?;

        for entry in entries {
            let entry = entry.map_err(|err| format!("Failed to read directory entry: {}", err))?;
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            if source_path.is_dir() {
                Self::copy_directory_recursive(source_path.as_path(), target_path.as_path())?;
            } else {
                fs::copy(source_path.as_path(), target_path.as_path()).map_err(|err| {
                    format!(
                        "Failed to copy {} to {}: {}",
                        source_path.display(),
                        target_path.display(),
                        err
                    )
                })?;
            }
        }

        Ok(())
    }
}
