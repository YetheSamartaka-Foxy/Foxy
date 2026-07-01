use crate::ui::app::{
    Foxy, RepositoryContextConfirmAction, RepositoryListContextAction, RepositoryListRow,
    RepositoryListSection,
};
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::search_filter::MultiEntryFilter;
use crate::ui::views::galley_cache;
use eframe::egui::{self, Align, Button, Margin, RichText, ScrollArea, TextEdit, Ui, Vec2};
use log::{info, warn};
use std::time::{Duration, Instant};

impl Foxy {
    fn repository_list_row_id_salt(&self, row: RepositoryListRow) -> (String, String) {
        match row {
            RepositoryListRow::SectionLabel(section) => (
                "repository_list_section".to_string(),
                match section {
                    RepositoryListSection::Spaces => "spaces".to_string(),
                    RepositoryListSection::Repositories => "repositories".to_string(),
                },
            ),
            RepositoryListRow::SpaceHeader(space_idx) => (
                "repository_list_space".to_string(),
                self.repository_spaces
                    .get(space_idx)
                    .map(|space| space.id.clone())
                    .unwrap_or_else(|| space_idx.to_string()),
            ),
            RepositoryListRow::Repository(repo_idx) => (
                "repository_list_repository".to_string(),
                self.repository_view_state
                    .repositories
                    .get(repo_idx)
                    .map(|repo| Self::normalize_repo_url(&repo.address))
                    .unwrap_or_else(|| repo_idx.to_string()),
            ),
        }
    }

    pub(super) fn rebuild_repository_list_cache_if_needed(&mut self) {
        let filter_changed =
            self.repository_list_cache.filter_raw != self.repository_view_state.repository_filter;
        let repositories_changed =
            self.repository_list_cache.repositories_version != self.repository_list_data_version;
        let spaces_changed =
            self.repository_list_cache.spaces_version != self.repository_spaces_version;
        let repo_states_changed =
            self.repository_list_cache.repo_states_version != self.repo_states_version;
        let spaces_collapsed_changed = self.repository_list_cache.repository_spaces_collapsed
            != self.repository_view_state.repository_spaces_collapsed;
        let repositories_collapsed_changed = self.repository_list_cache.repositories_collapsed
            != self.repository_view_state.repositories_collapsed;
        if !filter_changed
            && !repositories_changed
            && !spaces_changed
            && !repo_states_changed
            && !spaces_collapsed_changed
            && !repositories_collapsed_changed
        {
            return;
        }

        let rebuild_started_at = Instant::now();

        if repositories_changed {
            self.repository_list_cache.repositories_version = self.repository_list_data_version;
            self.repository_list_cache.repository_names_lower.clear();
            self.repository_list_cache
                .repository_addresses_lower
                .clear();
            self.repository_list_cache
                .repository_names_lower
                .reserve(self.repository_view_state.repositories.len());
            self.repository_list_cache
                .repository_addresses_lower
                .reserve(self.repository_view_state.repositories.len());
            for repo in &self.repository_view_state.repositories {
                self.repository_list_cache
                    .repository_names_lower
                    .push(repo.name.to_lowercase());
                self.repository_list_cache
                    .repository_addresses_lower
                    .push(repo.address.to_lowercase());
            }
        }

        if spaces_changed {
            self.repository_list_cache.spaces_version = self.repository_spaces_version;
            self.repository_list_cache.space_index_by_id.clear();
            for (space_idx, space) in self.repository_spaces.iter().enumerate() {
                self.repository_list_cache
                    .space_index_by_id
                    .insert(space.id.clone(), space_idx);
            }
        }

        if filter_changed {
            self.repository_list_cache.filter_raw =
                self.repository_view_state.repository_filter.clone();
            self.repository_list_cache.filter_lower =
                self.repository_list_cache.filter_raw.to_lowercase();
        }
        self.repository_list_cache.repo_states_version = self.repo_states_version;
        self.repository_list_cache.repository_spaces_collapsed =
            self.repository_view_state.repository_spaces_collapsed;
        self.repository_list_cache.repositories_collapsed =
            self.repository_view_state.repositories_collapsed;

        self.repository_list_cache.filtered_indices.clear();
        let multi_filter = MultiEntryFilter::parse(&self.repository_list_cache.filter_raw);
        if multi_filter.is_empty() {
            self.repository_list_cache
                .filtered_indices
                .extend(0..self.repository_view_state.repositories.len());
        } else {
            // Gather matches without holding a mutable borrow of the cache, so we
            // can call `&self` state lookups (installed/attached tags) per repo.
            let mut matched: Vec<usize> = Vec::new();
            for (repo_idx, repo) in self.repository_view_state.repositories.iter().enumerate() {
                let repo_name_lower = self
                    .repository_list_cache
                    .repository_names_lower
                    .get(repo_idx)
                    .map(String::as_str)
                    .unwrap_or_default();
                let repo_address_lower = self
                    .repository_list_cache
                    .repository_addresses_lower
                    .get(repo_idx)
                    .map(String::as_str)
                    .unwrap_or_default();
                let installed_tag = self.repo_installed_state_tag(&repo.address, &repo.path);
                let attached_tag = if repo.repository_space_id.is_some() {
                    crate::ui::search_filter::STATE_KEYWORD_ATTACHED
                } else {
                    crate::ui::search_filter::STATE_KEYWORD_DETACHED
                };
                if multi_filter.matches_normalized_with_tags(
                    &[repo_name_lower, repo_address_lower],
                    &[installed_tag, attached_tag],
                ) {
                    matched.push(repo_idx);
                }
            }
            self.repository_list_cache.filtered_indices = matched;
        }

        let mut grouped_space_children = vec![Vec::new(); self.repository_spaces.len()];
        let mut ungrouped_indices = Vec::new();
        for &repo_idx in &self.repository_list_cache.filtered_indices {
            let repo = &self.repository_view_state.repositories[repo_idx];
            if let Some(space_id) = repo.repository_space_id.as_deref()
                && let Some(space_idx) = self
                    .repository_list_cache
                    .space_index_by_id
                    .get(space_id)
                    .copied()
                && let Some(children) = grouped_space_children.get_mut(space_idx)
            {
                children.push(repo_idx);
                continue;
            }
            ungrouped_indices.push(repo_idx);
        }

        self.repository_list_cache.rows.clear();
        if !self.repository_spaces.is_empty() {
            self.repository_list_cache
                .rows
                .push(RepositoryListRow::SectionLabel(
                    RepositoryListSection::Spaces,
                ));
            if !self.repository_view_state.repository_spaces_collapsed {
                for (space_idx, children) in grouped_space_children.iter().enumerate() {
                    self.repository_list_cache
                        .rows
                        .push(RepositoryListRow::SpaceHeader(space_idx));
                    if !self.repository_spaces[space_idx].collapsed {
                        for &repo_idx in children {
                            self.repository_list_cache
                                .rows
                                .push(RepositoryListRow::Repository(repo_idx));
                        }
                    }
                }
            }
        }

        if !ungrouped_indices.is_empty() {
            self.repository_list_cache
                .rows
                .push(RepositoryListRow::SectionLabel(
                    RepositoryListSection::Repositories,
                ));
            if !self.repository_view_state.repositories_collapsed {
                for repo_idx in ungrouped_indices {
                    self.repository_list_cache
                        .rows
                        .push(RepositoryListRow::Repository(repo_idx));
                }
            }
        }

        let rebuild_elapsed = rebuild_started_at.elapsed();
        if rebuild_elapsed > Duration::from_millis(2) {
            info!(
                "Repository list cache rebuild took {:.2?} (repos={}, shown={}, spaces={}, rows={}, filter='{}')",
                rebuild_elapsed,
                self.repository_view_state.repositories.len(),
                self.repository_list_cache.filtered_indices.len(),
                self.repository_spaces.len(),
                self.repository_list_cache.rows.len(),
                self.repository_list_cache.filter_lower
            );
        }
    }

    pub(super) fn render_repository_sidebar(&mut self, ui: &mut Ui) {
        let mut sidepanel_frame = egui::containers::Frame::side_top_panel(&ui.ctx().global_style());
        sidepanel_frame.fill = self.color_main_bg();
        sidepanel_frame.inner_margin = Margin::same(10);
        egui::Panel::left("repository_list")
            .exact_size(250.0)
            .resizable(false)
            .frame(sidepanel_frame)
            .show(ui, |ui| {
                let add_repository_font_size = self
                    .settings_view_state
                    .font_sizes
                    .repository_view
                    .add_repository_button as f32;
                let size_list_element = Vec2::new(
                    ui.available_width(),
                    Self::adaptive_button_height(add_repository_font_size, 50.0),
                );
                let combined_text =
                    RichText::new(tr("+ Add repository")).size(add_repository_font_size);

                let add_repo_button = ui.add_sized(size_list_element, Button::new(combined_text))
                    .on_hover_text(self.t("Add a new repository or repository space by entering a URL."));
                if add_repo_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if add_repo_button.clicked() {
                    self.show_add_repository_modal = true;
                    self.add_repository_input_error = None;
                    self.add_repository_input_name.clear();
                    self.add_repository_input_path.clear();
                    self.pending_repository_duplicate_add = None;
                    info!("Opened add repository dialog from repository list view");
                }

                if self.repository_view_state.repositories.is_empty()
                    && self.repository_spaces.is_empty()
                {
                    ui.label(tr("No repositories"));
                    return;
                }

                let repository_filter_help = self.t("repository_filter_help");
                let repository_filter_hint = tr("Filter repositories");
                ui.vertical_centered(|ui| {
                    let full_width = ui.available_width();
                    ui.allocate_ui_with_layout(
                        Vec2::new(full_width * 0.9, 24.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add_sized(
                                Vec2::new(full_width * 0.8, 20.0),
                                TextEdit::singleline(
                                    &mut self.repository_view_state.repository_filter,
                                )
                                .id_salt("repository_filter_input")
                                .hint_text(repository_filter_hint)
                                .horizontal_align(Align::Center),
                            );
                            ui.add_space(6.0);
                            self.filter_help_icon(ui, &repository_filter_help);
                        },
                    );
                });

                self.rebuild_repository_list_cache_if_needed();

                ui.vertical_centered(|ui| {
                    ui.label(tr_fmt(
                        "Showing {shown} / {total} repositories",
                        &[
                            (
                                "shown",
                                self.repository_list_cache
                                    .filtered_indices
                                    .len()
                                    .to_string(),
                            ),
                            (
                                "total",
                                self.repository_view_state.repositories.len().to_string(),
                            ),
                        ],
                    ));
                });

                let mut repository_context_action: Option<(usize, RepositoryListContextAction)> =
                    None;
                let mut section_action: Option<RepositoryListSection> = None;
                let row_count = self.repository_list_cache.rows.len();
                let repository_rows_generation = galley_cache::fingerprint((
                    self.repository_list_data_version,
                    self.repository_spaces_version,
                    self.repository_view_state.repository_filter.as_str(),
                    self.repository_view_state.repository_spaces_collapsed,
                    self.repository_view_state.repositories_collapsed,
                    self.repository_list_cache
                        .rows
                        .iter()
                        .map(|row| match row {
                            RepositoryListRow::SectionLabel(section) => match section {
                                RepositoryListSection::Spaces => (0_u8, 0_usize, 0_u8),
                                RepositoryListSection::Repositories => (0_u8, 1_usize, 0_u8),
                            },
                            RepositoryListRow::SpaceHeader(space_idx) => (
                                1_u8,
                                *space_idx,
                                self.repository_spaces
                                    .get(*space_idx)
                                    .is_some_and(|space| space.collapsed)
                                    as u8,
                            ),
                            RepositoryListRow::Repository(repo_idx) => self
                                .repository_view_state
                                .repositories
                                .get(*repo_idx)
                                .map(|repo| {
                                    (
                                        2_u8,
                                        *repo_idx,
                                        self.repo_state_for_address(&repo.address, &repo.path) as u8,
                                    )
                                })
                                .unwrap_or((2_u8, *repo_idx, 0_u8)),
                        })
                        .collect::<Vec<_>>(),
                ));
                self.repository_list_galleys.ensure(
                    row_count,
                    1,
                    repository_rows_generation,
                    galley_cache::fingerprint((
                        (self.settings_view_state.font_sizes.repository_view.status_banner as f32)
                            .to_bits(),
                        self.color_text_normal().to_array(),
                    )),
                );
                ScrollArea::vertical().show_rows(
                    ui,
                    Self::repository_list_row_height(),
                    row_count,
                    |ui, row_range| {
                        ui.set_min_width(ui.available_width());
                        for row_idx in row_range {
                            let row = self.repository_list_cache.rows[row_idx];
                            let row_id_salt = self.repository_list_row_id_salt(row);
                            ui.push_id(row_id_salt, |ui| {
                                self.render_repository_list_cached_row(
                                    ui,
                                    row_idx,
                                    row,
                                    &mut repository_context_action,
                                    &mut section_action,
                                );
                            });
                        }
                    },
                );

                // Handle drag-and-drop completion
                if self.drag_source_repo_index.is_some()
                    && !ui.ctx().input(|i| i.pointer.any_down())
                {
                    if let (Some(from), Some(to)) =
                        (self.drag_source_repo_index, self.drag_drop_target_index)
                        && let Some(target) = Self::repository_drop_target_index(
                            from,
                            to,
                            self.repository_view_state.repositories.len(),
                        )
                    {
                        self.reorder_repository(from, target);
                    }
                    self.drag_source_repo_index = None;
                    self.drag_drop_target_index = None;
                }

                if let Some(section) = section_action {
                    self.toggle_repository_list_section_collapsed(section);
                }

                if let Some((repo_idx, action)) = repository_context_action {
                    match action {
                        RepositoryListContextAction::MoveUp => {
                            if repo_idx > 0 {
                                self.reorder_repository(repo_idx, repo_idx - 1);
                            }
                        }
                        RepositoryListContextAction::MoveDown => {
                            if repo_idx + 1
                                < self.repository_view_state.repositories.len()
                            {
                                self.reorder_repository(repo_idx, repo_idx + 1);
                            }
                        }
                        RepositoryListContextAction::GoToRepositorySpace => {
                            let target_space_id = self
                                .repository_view_state
                                .repositories
                                .get(repo_idx)
                                .and_then(|repo| repo.repository_space_id.clone());
                            if let Some(space_id) = target_space_id {
                                let space_name = self
                                    .repository_space_name_by_id(&space_id)
                                    .unwrap_or(space_id.as_str())
                                    .to_string();
                                self.selected_repository_space_id = Some(space_id);
                                self.repository_view_state.selected_repository = None;
                                self.repository_selection = None;
                                self.editor_mission_search.clear();
                                self.editor_mission_folder.clear();
                                self.editor_mission_terrain_filter.clear();
                                self.clear_completed_repository_check_banner_for_repo_change(None);
                                info!(
                                    "Navigated to repository space {} from repository list context menu",
                                    space_name
                                );
                            }
                        }
                        RepositoryListContextAction::CloneWithSuffix => {
                            self.repository_view_state.selected_repository = Some(repo_idx);
                            self.selected_repository_space_id = None;
                            self.clear_completed_repository_check_banner_for_repo_change(Some(
                                repo_idx,
                            ));
                            self.clone_repository_with_suffix(repo_idx);
                        }
                        RepositoryListContextAction::OpenLocalPath => {
                            self.repository_view_state.selected_repository = Some(repo_idx);
                            self.selected_repository_space_id = None;
                            self.clear_completed_repository_check_banner_for_repo_change(Some(
                                repo_idx,
                            ));
                            if !self.open_repository_local_path(repo_idx) {
                                warn!("Failed to open repository local path from context menu");
                                self.show_error_toast(self.t("Failed to open repository local path."));
                            }
                        }
                        RepositoryListContextAction::Delete => {
                            self.repository_view_state.selected_repository = Some(repo_idx);
                            self.selected_repository_space_id = None;
                            self.delete_repository_delete_files = false;
                            self.clear_completed_repository_check_banner_for_repo_change(Some(
                                repo_idx,
                            ));
                            self.pending_repository_context_confirmation =
                                Some(RepositoryContextConfirmAction::Delete(repo_idx));
                        }
                        RepositoryListContextAction::WipeRepositoryDb => {
                            self.repository_view_state.selected_repository = Some(repo_idx);
                            self.selected_repository_space_id = None;
                            self.clear_completed_repository_check_banner_for_repo_change(Some(
                                repo_idx,
                            ));
                            self.pending_repository_context_confirmation =
                                Some(RepositoryContextConfirmAction::WipeRepositoryDb(repo_idx));
                        }
                        RepositoryListContextAction::ForceRedownload => {
                            self.repository_view_state.selected_repository = Some(repo_idx);
                            self.selected_repository_space_id = None;
                            self.clear_completed_repository_check_banner_for_repo_change(Some(
                                repo_idx,
                            ));
                            self.pending_repository_context_confirmation =
                                Some(RepositoryContextConfirmAction::ForceRedownload(repo_idx));
                        }
                    }
                }
            });
    }
}
