use super::{
    RepositoryListSectionContextAction, RepositorySpaceRowContextAction,
    RepositoryVisualFolderRowContextAction,
};
use crate::core::api::SyncMode;
use crate::ui::app::{Foxy, RepositoryListContextAction, RepositoryListRow, RepositoryListSection};
use crate::ui::context_menu::{ContextMenuItem, attach_context_menu};
use crate::ui::types::Repository;
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align, Atom, Button, CornerRadius, CursorIcon, Frame, Image, Label, Layout, Margin,
    RichText, Sense, Stroke, TextStyle, Ui, Vec2,
};
use log::info;

impl Foxy {
    fn render_repository_list_section_row(
        &mut self,
        ui: &mut Ui,
        section: RepositoryListSection,
        section_action: &mut Option<(RepositoryListSection, RepositoryListSectionContextAction)>,
    ) {
        if section == RepositoryListSection::Repositories {
            ui.add_space(8.0);
        }

        let collapsed = self.is_repository_list_section_collapsed(section);
        let label = match section {
            RepositoryListSection::Spaces => self.t("Repository spaces"),
            RepositoryListSection::Repositories => self.t("Repositories"),
        };
        let toggle_label = if collapsed {
            self.t("Expand section")
        } else {
            self.t("Collapse section")
        };
        let section_font_size = (self
            .settings_view_state
            .font_sizes
            .repository_view
            .status_banner as f32
            - 2.0)
            .max(16.0);

        let response = Frame::NONE
            .fill(self.color_card_bg())
            .stroke(egui::Stroke::new(1.0, self.color_widget_bg()))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin {
                left: 10,
                right: 8,
                top: 6,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.set_min_height(Self::repository_list_section_row_height() - 12.0);
                ui.horizontal(|ui| {
                    ui.add(
                        Label::new(
                            RichText::new(label)
                                .strong()
                                .size(section_font_size)
                                .color(self.color_text_normal()),
                        )
                        .selectable(false),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(if collapsed { "+" } else { "-" })
                                .strong()
                                .size(section_font_size + 4.0)
                                .color(self.color_text_normal()),
                        );
                    });
                });
            })
            .response
            .interact(Sense::click());

        if response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        if response.clicked() {
            *section_action = Some((section, RepositoryListSectionContextAction::ToggleCollapsed));
        }

        let mut context_action = None;
        attach_context_menu(
            &response,
            &[
                ContextMenuItem::new(
                    RepositoryListSectionContextAction::ToggleCollapsed,
                    toggle_label,
                ),
                ContextMenuItem::new(
                    RepositoryListSectionContextAction::CreateFolder,
                    self.t("Create folder"),
                )
                .separator_before()
                .disabled_if(section != RepositoryListSection::Repositories),
            ],
            &mut context_action,
        );
        if let Some(action) = context_action {
            *section_action = Some((section, action));
        }
    }

    fn render_repository_list_space_row(&mut self, ui: &mut Ui, row_slot: usize, space_idx: usize) {
        let (space_id, icon_checksum, space_name, collapsed) = {
            let space = &self.repository_spaces[space_idx];
            (
                space.id.clone(),
                space.icon_image_checksum.clone(),
                Self::repository_space_display_name(space).to_string(),
                space.collapsed,
            )
        };
        let is_selected = self.selected_repository_space_id.as_deref() == Some(space_id.as_str());
        let fill = if is_selected {
            self.color_primary_accent()
        } else {
            self.color_main_bg()
        };
        let icon = self.cached_icons.get(&icon_checksum);
        let space_name_max_chars = if icon.is_some() { 20 } else { 24 };
        let truncated_space_name = Self::truncate_display_name(&space_name, space_name_max_chars);
        let display_font_size = (self
            .settings_view_state
            .font_sizes
            .repository_view
            .status_banner as f32
            - 6.0)
            .max(14.0);
        let text_color = self.color_text_normal();
        let display_text = galley_cache::lazy_galley_colored(
            ui,
            self.repository_list_galleys.slot(row_slot, 0),
            egui::FontId::proportional(display_font_size),
            text_color,
            || truncated_space_name.clone(),
        );
        let row_height = Self::repository_list_row_height();
        let stroke_color = self.color_widget_bg();
        let toggle_fill = self.color_widget_bg();
        let toggle_width = 28.0;
        let toggle_gap = 4.0;
        let main_width = (ui.available_width() - toggle_width - toggle_gap).max(40.0);
        let toggle_hover = if collapsed {
            self.t("Expand repository space")
        } else {
            self.t("Collapse repository space")
        };

        let mut toggle_clicked = false;
        let resp = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = toggle_gap;
                let area = Vec2::new(main_width, row_height);
                let resp = if let Some(tex) = icon {
                    let img = Image::new((tex.id(), Vec2::splat(24.0)));
                    ui.add_sized(
                        area,
                        Button::image_and_text(img, display_text)
                            .fill(fill)
                            .stroke(Stroke::new(1.0, stroke_color))
                            .truncate(),
                    )
                } else {
                    ui.add_sized(
                        area,
                        Button::new(display_text)
                            .fill(fill)
                            .stroke(Stroke::new(1.0, stroke_color))
                            .truncate(),
                    )
                }
                .interact(Sense::click());
                if resp.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }

                let toggle = ui
                    .add_sized(
                        Vec2::new(toggle_width, row_height),
                        Button::new(
                            RichText::new(if collapsed { "+" } else { "-" })
                                .strong()
                                .size(display_font_size + 4.0)
                                .color(text_color),
                        )
                        .fill(toggle_fill)
                        .stroke(Stroke::new(1.0, stroke_color)),
                    )
                    .on_hover_text(toggle_hover);
                if toggle.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if toggle.clicked() {
                    toggle_clicked = true;
                }

                resp
            })
            .inner;

        if toggle_clicked {
            self.set_repository_space_collapsed(&space_id, !collapsed);
        }
        if resp.clicked() {
            self.selected_repository_space_id = Some(space_id.clone());
            self.selected_repository_visual_folder_id = None;
            self.repository_view_state.selected_repository = None;
            self.clear_completed_repository_check_banner_for_repo_change(None);
            info!("Selected repository space {}", space_name);
        }

        let mut context_action = None;
        attach_context_menu(
            &resp,
            &[
                ContextMenuItem::new(
                    RepositorySpaceRowContextAction::ToggleCollapsed,
                    if collapsed {
                        self.t("Expand repository space")
                    } else {
                        self.t("Collapse repository space")
                    },
                ),
                ContextMenuItem::new(
                    RepositorySpaceRowContextAction::CreateFolder,
                    self.t("Create folder"),
                )
                .separator_before(),
                ContextMenuItem::new(
                    RepositorySpaceRowContextAction::Delete,
                    self.t("Delete repository space"),
                )
                .separator_before()
                .danger(),
            ],
            &mut context_action,
        );
        match context_action {
            Some(RepositorySpaceRowContextAction::ToggleCollapsed) => {
                self.set_repository_space_collapsed(&space_id, !collapsed);
            }
            Some(RepositorySpaceRowContextAction::CreateFolder) => {
                self.open_create_repository_visual_folder(Some(space_id));
            }
            Some(RepositorySpaceRowContextAction::Delete) => {
                self.pending_repository_space_delete_id = Some(space_id);
            }
            None => {}
        }
    }

    fn render_repository_visual_folder_row(
        &mut self,
        ui: &mut Ui,
        row_slot: usize,
        folder_idx: usize,
    ) {
        let (folder_id, folder_name, collapsed, color_rgb, repository_count) = {
            let folder = &self.repository_visual_folders[folder_idx];
            (
                folder.id.clone(),
                folder.name.clone(),
                folder.collapsed,
                folder.color_rgb,
                folder.repository_keys.len(),
            )
        };
        let is_selected =
            self.selected_repository_visual_folder_id.as_deref() == Some(folder_id.as_str());
        let folder_color = egui::Color32::from_rgb(color_rgb[0], color_rgb[1], color_rgb[2]);
        // Fill the whole row with the folder's color; the selection is shown via
        // a brighter, thicker accent border rather than by changing the fill.
        let fill = folder_color;
        let display_font_size = (self
            .settings_view_state
            .font_sizes
            .repository_view
            .status_banner as f32
            - 6.0)
            .max(14.0);
        // Choose black/white label text for readability over an arbitrary folder color.
        let luminance = 2126 * u32::from(color_rgb[0])
            + 7152 * u32::from(color_rgb[1])
            + 722 * u32::from(color_rgb[2]);
        let text_color = if luminance < 1_275_000 {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };
        let toggle_text_color = self.color_text_normal();
        let folder_label = self.t_fmt(
            "{name} ({count})",
            &[
                ("name", folder_name.clone()),
                ("count", repository_count.to_string()),
            ],
        );
        let text = galley_cache::lazy_galley_colored(
            ui,
            self.repository_list_galleys.slot(row_slot, 0),
            egui::FontId::proportional(display_font_size),
            text_color,
            || Self::truncate_display_name(&folder_label, 24),
        );
        let row_height = Self::repository_list_row_height();
        let toggle_width = 28.0;
        let toggle_gap = 4.0;
        let main_width = (ui.available_width() - toggle_width - toggle_gap).max(40.0);
        let (stroke_color, stroke_width) = if is_selected {
            (self.color_primary_accent(), 2.5)
        } else {
            (folder_color, 1.5)
        };
        let toggle_hover = if collapsed {
            self.t("Expand folder")
        } else {
            self.t("Collapse folder")
        };

        let mut toggle_clicked = false;
        let resp = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = toggle_gap;
                let area = Vec2::new(main_width, row_height);
                let resp = ui
                    .add_sized(
                        area,
                        Button::new(text)
                            .fill(fill)
                            .stroke(Stroke::new(stroke_width, stroke_color))
                            .truncate(),
                    )
                    .interact(Sense::click());
                if resp.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }

                let toggle = ui
                    .add_sized(
                        Vec2::new(toggle_width, row_height),
                        Button::new(
                            RichText::new(if collapsed { "+" } else { "-" })
                                .strong()
                                .size(display_font_size + 4.0)
                                .color(toggle_text_color),
                        )
                        .fill(self.color_widget_bg())
                        .stroke(Stroke::new(1.0, folder_color)),
                    )
                    .on_hover_text(toggle_hover);
                if toggle.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if toggle.clicked() {
                    toggle_clicked = true;
                }
                resp
            })
            .inner;

        if toggle_clicked {
            self.set_repository_visual_folder_collapsed(&folder_id, !collapsed);
        }
        if resp.clicked() {
            self.selected_repository_visual_folder_id = Some(folder_id.clone());
            self.selected_repository_space_id = None;
            self.repository_view_state.selected_repository = None;
            self.clear_completed_repository_check_banner_for_repo_change(None);
            info!("Selected repository folder {}", folder_name);
        }

        if self.drag_source_repo_index.is_some()
            && let Some(pointer_pos) = ui.ctx().pointer_latest_pos()
            && resp.rect.contains(pointer_pos)
        {
            self.drag_drop_target_visual_folder_id = Some(folder_id.clone());
            ui.painter().rect_stroke(
                resp.rect,
                CornerRadius::same(6),
                Stroke::new(2.0, stroke_color),
                egui::StrokeKind::Outside,
            );
        }

        let mut context_action = None;
        attach_context_menu(
            &resp,
            &[
                ContextMenuItem::new(
                    RepositoryVisualFolderRowContextAction::ToggleCollapsed,
                    if collapsed {
                        self.t("Expand folder")
                    } else {
                        self.t("Collapse folder")
                    },
                ),
                ContextMenuItem::new(
                    RepositoryVisualFolderRowContextAction::Rename,
                    self.t("Rename folder"),
                )
                .separator_before(),
                ContextMenuItem::new(
                    RepositoryVisualFolderRowContextAction::ChangeColor,
                    self.t("Change folder color"),
                ),
                ContextMenuItem::new(
                    RepositoryVisualFolderRowContextAction::QuickLocalCheck,
                    self.t("Quick local check folder"),
                )
                .separator_before(),
                ContextMenuItem::new(
                    RepositoryVisualFolderRowContextAction::RemoteRecheck,
                    self.t("Remote recheck folder"),
                ),
                ContextMenuItem::new(
                    RepositoryVisualFolderRowContextAction::Update,
                    self.t("Update folder"),
                ),
                ContextMenuItem::new(
                    RepositoryVisualFolderRowContextAction::Delete,
                    self.t("Delete folder"),
                )
                .separator_before()
                .danger(),
            ],
            &mut context_action,
        );
        match context_action {
            Some(RepositoryVisualFolderRowContextAction::ToggleCollapsed) => {
                self.set_repository_visual_folder_collapsed(&folder_id, !collapsed);
            }
            Some(RepositoryVisualFolderRowContextAction::Rename)
            | Some(RepositoryVisualFolderRowContextAction::ChangeColor) => {
                self.open_edit_repository_visual_folder(&folder_id);
            }
            Some(RepositoryVisualFolderRowContextAction::QuickLocalCheck) => {
                self.queue_repository_visual_folder_sync(&folder_id, SyncMode::QuickCheckOnly);
            }
            Some(RepositoryVisualFolderRowContextAction::RemoteRecheck) => {
                self.queue_repository_visual_folder_sync(&folder_id, SyncMode::RemoteRefreshOnly);
            }
            Some(RepositoryVisualFolderRowContextAction::Update) => {
                self.queue_repository_visual_folder_sync(&folder_id, SyncMode::Download);
            }
            Some(RepositoryVisualFolderRowContextAction::Delete) => {
                self.pending_repository_visual_folder_delete =
                    Some(crate::ui::app::RepositoryVisualFolderDeleteState {
                        folder_id,
                        delete_repositories: false,
                    });
            }
            None => {}
        }
    }

    pub(super) fn render_repository_list_cached_row(
        &mut self,
        ui: &mut Ui,
        row_slot: usize,
        row: RepositoryListRow,
        repository_context_action: &mut Option<(usize, RepositoryListContextAction)>,
        section_action: &mut Option<(RepositoryListSection, RepositoryListSectionContextAction)>,
    ) {
        match row {
            RepositoryListRow::SectionLabel(section) => {
                self.render_repository_list_section_row(ui, section, section_action);
            }
            RepositoryListRow::SpaceHeader(space_idx) => {
                self.render_repository_list_space_row(ui, row_slot, space_idx);
            }
            RepositoryListRow::FolderHeader(folder_idx) => {
                self.render_repository_visual_folder_row(ui, row_slot, folder_idx);
            }
            RepositoryListRow::Repository { repo_idx, indented } => {
                self.render_repository_list_row(
                    ui,
                    row_slot,
                    repo_idx,
                    indented,
                    repository_context_action,
                );
            }
        }
    }

    pub(crate) fn build_effective_repository_snapshot(repo: &Repository) -> Repository {
        let mut effective = repo.clone();
        if let Some(profile_name) = &repo.selected_profile
            && let Some(profile) = repo.profiles.iter().find(|p| &p.name == profile_name)
        {
            Self::apply_profile_to_repository(&mut effective, profile);
        }
        effective
    }

    pub(super) fn render_repository_list_row(
        &mut self,
        ui: &mut Ui,
        row_slot: usize,
        repo_index: usize,
        indented: bool,
        repository_context_action: &mut Option<(usize, RepositoryListContextAction)>,
    ) {
        let is_selected = self.repository_view_state.selected_repository == Some(repo_index);
        let is_dragged = self.drag_source_repo_index == Some(repo_index);
        let fill = if is_dragged {
            self.color_widget_bg_active()
        } else if is_selected {
            self.color_primary_accent()
        } else {
            self.color_widget_bg()
        };
        let (name, icon_checksum, address, repo_path, repo_has_local_path, repo_space_id) = {
            let repo = &self.repository_view_state.repositories[repo_index];
            (
                repo.name.clone(),
                repo.icon_image_checksum.clone(),
                repo.address.clone(),
                repo.path.clone(),
                !repo.path.trim().is_empty(),
                repo.repository_space_id.clone(),
            )
        };
        let can_go_to_space = repo_space_id.is_some();
        let can_remove_from_folder = self.repository_visual_folder_for_repo(repo_index).is_some();
        let db_wipe_pending = self.is_repository_db_wipe_pending(&address);
        let icon = self.cached_icons.get(&icon_checksum);
        let state = self.repo_state_for_address(&address, &repo_path);
        let active_operation_tooltip = self.active_repository_row_operation_tooltip(repo_index);

        let state_icon = match state {
            crate::ui::types::RepoState::Synced => "\u{2705}",
            crate::ui::types::RepoState::PendingUpdate => "\u{26A0}",
            crate::ui::types::RepoState::Updating => "\u{21BB}",
            crate::ui::types::RepoState::Unknown => "\u{2753}",
        };
        let name_max_chars = if icon.is_some() { 20 } else { 24 };
        let truncated_name = Self::truncate_display_name(&name, name_max_chars);
        let display_text = if active_operation_tooltip.is_some() {
            truncated_name
        } else {
            format!("{truncated_name}  {state_icon}")
        };

        let text_color = self.color_text_normal();
        let text = galley_cache::lazy_galley_colored(
            ui,
            self.repository_list_galleys.slot(row_slot, 0),
            TextStyle::Heading.resolve(ui.style()),
            text_color,
            || display_text,
        );
        let row_height = Self::repository_list_row_height();
        let indent = if indented { 18.0 } else { 0.0 };
        let row_width = (ui.available_width() - indent).max(80.0);
        let status_size = Vec2::splat(18.0);

        let resp = ui
            .horizontal(|ui| {
                if indented {
                    ui.add_space(indent);
                }
                let area = Vec2::new(row_width, row_height);
                if let Some(tooltip) = active_operation_tooltip.as_ref() {
                    let spinner_id = egui::Id::new(("repository_status_spinner", repo_index));
                    let atom_response = if let Some(tex) = icon {
                        let img = Image::new((tex.id(), Vec2::splat(28.0)));
                        Button::new((img, text, Atom::custom(spinner_id, status_size)))
                            .fill(fill)
                            .min_size(area)
                            .truncate()
                            .atom_ui(ui)
                    } else {
                        Button::new((text, Atom::custom(spinner_id, status_size)))
                            .fill(fill)
                            .min_size(area)
                            .truncate()
                            .atom_ui(ui)
                    };

                    if let Some(status_rect) = atom_response.rect(spinner_id) {
                        let status_response = ui.interact(
                            status_rect,
                            atom_response.response.id.with("operation_status"),
                            Sense::hover(),
                        );
                        egui::Spinner::new()
                            .size(status_rect.width().min(status_rect.height()))
                            .paint_at(ui, status_rect);
                        let _ = status_response.on_hover_text(tooltip);
                    }

                    atom_response.response
                } else if let Some(tex) = icon {
                    let img = Image::new((tex.id(), Vec2::splat(28.0)));
                    ui.add_sized(
                        area,
                        Button::image_and_text(img, text).fill(fill).truncate(),
                    )
                } else {
                    ui.add_sized(area, Button::new(text).fill(fill).truncate())
                }
            })
            .inner;

        // Drag-and-drop: the dragged row owns the source, while the hovered row owns the target.
        let drag_resp = resp.interact(Sense::drag());
        if drag_resp.drag_started() {
            self.drag_source_repo_index = Some(repo_index);
        }
        if drag_resp.dragged() && self.drag_source_repo_index == Some(repo_index) {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        if self.drag_source_repo_index.is_some()
            && let Some(pointer_pos) = ui.ctx().pointer_latest_pos()
            && resp.rect.contains(pointer_pos)
        {
            let row_center_y = resp.rect.center().y;
            if pointer_pos.y < row_center_y {
                self.drag_drop_target_index = Some(repo_index);
            } else {
                self.drag_drop_target_index = Some(repo_index + 1);
            }
        }
        // Draw drop indicator line when another row is being dragged over this one
        if self.drag_source_repo_index.is_some()
            && let Some(pointer_pos) = ui.ctx().pointer_latest_pos()
            && resp.rect.contains(pointer_pos)
        {
            let row_center_y = resp.rect.center().y;
            let indicator_y = if pointer_pos.y < row_center_y {
                resp.rect.top()
            } else {
                resp.rect.bottom()
            };
            let line_start = egui::pos2(resp.rect.left(), indicator_y);
            let line_end = egui::pos2(resp.rect.right(), indicator_y);
            ui.painter().line_segment(
                [line_start, line_end],
                Stroke::new(2.0, self.color_primary_accent()),
            );
        }

        if resp.clicked() {
            let prev_selected = self.repository_view_state.selected_repository;
            self.repository_view_state.selected_repository = Some(repo_index);
            self.selected_repository_space_id = None;
            self.selected_repository_visual_folder_id = None;
            self.clear_completed_repository_check_banner_for_repo_change(Some(repo_index));
            info!(
                "Selected repository {}",
                self.repository_view_state.repositories[repo_index].name
            );

            if prev_selected != Some(repo_index) || self.syncing_repository != Some(repo_index) {
                self.clear_mod_diff_cache();
                self.load_cached_updates_for_repo(repo_index);
            }
        }
        if resp.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }

        let is_first_repo = repo_index == 0;
        let is_last_repo = repo_index + 1 >= self.repository_view_state.repositories.len();
        let mut context_action: Option<RepositoryListContextAction> = None;
        attach_context_menu(
            &resp,
            &[
                ContextMenuItem::new(
                    RepositoryListContextAction::MoveUp,
                    self.t("Move repository up"),
                )
                .disabled_if(is_first_repo),
                ContextMenuItem::new(
                    RepositoryListContextAction::MoveDown,
                    self.t("Move repository down"),
                )
                .disabled_if(is_last_repo),
                ContextMenuItem::new(
                    RepositoryListContextAction::CloneWithSuffix,
                    self.t("Clone repository with suffix"),
                )
                .separator_before(),
                ContextMenuItem::new(
                    RepositoryListContextAction::GoToRepositorySpace,
                    self.t("Go to repository space"),
                )
                .disabled_if(!can_go_to_space),
                ContextMenuItem::new(
                    RepositoryListContextAction::RemoveFromVisualFolder,
                    self.t("Remove from folder"),
                )
                .disabled_if(!can_remove_from_folder),
                ContextMenuItem::new(
                    RepositoryListContextAction::OpenLocalPath,
                    self.t("Open repository local path"),
                )
                .separator_before()
                .disabled_if(!repo_has_local_path),
                ContextMenuItem::new(
                    RepositoryListContextAction::ForceRedownload,
                    self.t("Force redownload repository"),
                )
                .separator_before()
                .disabled_if(db_wipe_pending)
                .danger(),
                ContextMenuItem::new(
                    RepositoryListContextAction::WipeRepositoryDb,
                    self.t("Wipe repository database entries"),
                )
                .separator_before()
                .disabled_if(db_wipe_pending)
                .danger(),
                ContextMenuItem::new(
                    RepositoryListContextAction::Delete,
                    self.t("Delete repository"),
                )
                .separator_before()
                .danger(),
            ],
            &mut context_action,
        );
        if let Some(action) = context_action {
            *repository_context_action = Some((repo_index, action));
        }
    }
}
