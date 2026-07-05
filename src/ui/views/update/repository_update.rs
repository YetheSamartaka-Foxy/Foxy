use super::*;
use crate::ui::views::galley_cache;

impl Foxy {
    pub fn render_repository_update_view(&mut self, ui: &mut Ui) {
        if !self.update_modal_open {
            return;
        }
        if self.direct_download_update_view {
            self.render_direct_download_update_view(ui);
            return;
        }

        self.rebuild_update_modal_sort_cache_if_needed();

        let selected = self.repository_view_state.selected_repository;
        let downloading = self.current_sync_mode == Some(SyncMode::Download)
            && self.syncing_repository == selected;
        let download_finished_for_selected =
            self.download_finished && selected.is_some() && self.download_finished_repo == selected;

        let progress_pct: f32 = if let Some((_, percent)) = &self.download_progress {
            ((*percent).clamp(0.0, 1.0) * 1000.0).round() / 1000.0
        } else if download_finished_for_selected {
            1.0
        } else {
            0.0
        };
        let cancelling_label = self.t("Cancelling...");
        let reverting_label = self.t("Reverting changes");
        let in_cancel_stage = repository_update_cancel_stage(
            &self.download_progress,
            &cancelling_label,
            &reverting_label,
        );

        let mut start_download: Option<usize> = None;
        let mut set_download_paused: Option<bool> = None;
        let mut cancel_requested = false;

        let pending_mod_count = self
            .mod_diff_cache
            .iter()
            .filter(|m| m.needs_update)
            .count();
        let total_bytes: u64 = self
            .mod_diff_cache
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.total_bytes)
            .sum();
        let total_changed_files: usize = self
            .mod_diff_cache
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.files.len())
            .sum();
        let total_changed_parts: usize = self
            .mod_diff_cache
            .iter()
            .filter(|m| m.needs_update)
            .flat_map(|m| m.files.iter())
            .map(|f| f.changed_parts)
            .sum();

        let outer_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };
        let card_horizontal_padding = 15.0;
        let card_spacing = 8.0;
        let addon_card_scale = 0.75;
        let progress_footer_top_gap = 12.0;
        let progress_footer_bottom_gap = 24.0;

        Frame::NONE.inner_margin(outer_margin).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let close_icon_size =
                        self.settings_view_state.font_sizes.update_view.close_icon as f32;
                    let repo_name = selected.and_then(|idx| {
                        self.repository_view_state
                            .repositories
                            .get(idx)
                            .map(|r| r.name.clone())
                    });
                    let page_title = match repo_name {
                        Some(name) if !name.trim().is_empty() => {
                            format!("{} - {}", self.t("Repository Update"), name)
                        }
                        _ => self.t("Repository Update"),
                    };
                    ui.heading(RichText::new(page_title).size(
                        self.settings_view_state.font_sizes.update_view.page_title as f32,
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close_button = ui.add_enabled(
                            !in_cancel_stage,
                            Button::new(
                                RichText::new("X")
                                    .color(self.color_text_normal())
                                    .size(close_icon_size),
                            )
                            .min_size(Self::modal_icon_button_size(close_icon_size))
                            .fill(self.color_main_bg()),
                        );
                        if close_button.hovered() && !in_cancel_stage {
                            ui.ctx().output_mut(|o| {
                                Foxy::set_pointing_cursor_output(o);
                            });
                        }
                        if close_button.clicked() && !in_cancel_stage {
                            self.update_modal_open = false;
                            self.current_view = FoxyView::RepositoryList;
                            info!("Closed repository update modal");
                        }
                    });
                });

                ui.separator();
                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    ui.heading(RichText::new(self.t("Mods to be updated")).size(
                        self.settings_view_state.font_sizes.update_view.section_title as f32,
                    ));
                    ui.add_space(8.0);

                    let copy_manifest_button = ui.add_enabled(
                        pending_mod_count > 0,
                        Button::new(
                            RichText::new(self.t("Copy update manifest")).size(
                                self.settings_view_state.font_sizes.update_view.mod_status as f32,
                            ),
                        )
                        .min_size(Vec2::new(160.0, 28.0))
                        .fill(self.color_widget_bg()),
                    );
                    if copy_manifest_button.hovered() && pending_mod_count > 0 {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if copy_manifest_button.clicked() && pending_mod_count > 0 {
                        self.export_update_manifest_to_clipboard(total_bytes);
                    }
                    ui.add_space(4.0);
                    let save_manifest_button = ui.add_enabled(
                        pending_mod_count > 0,
                        Button::new(
                            RichText::new(self.t("Save update manifest to file")).size(
                                self.settings_view_state.font_sizes.update_view.mod_status as f32,
                            ),
                        )
                        .min_size(Vec2::new(160.0, 28.0))
                        .fill(self.color_widget_bg()),
                    );
                    if save_manifest_button.hovered() && pending_mod_count > 0 {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if save_manifest_button.clicked() && pending_mod_count > 0 {
                        self.export_update_manifest_to_file(total_bytes);
                    }
                });
                ui.separator();

                if pending_mod_count == 0 {
                    ui.label(self.t("No updates needed."));
                } else {
                    ui.label(
                        RichText::new(format!(
                            "{} - {}",
                            self.t_fmt(
                                "Total update size: {size} - Files: {files} - Parts: {parts}",
                                &[
                                    ("size", fmt_bytes(total_bytes)),
                                    ("files", total_changed_files.to_string()),
                                    ("parts", total_changed_parts.to_string())
                                ]
                            ),
                            self.i18n.tr_plural(
                                "Updates found in {count} addons",
                                pending_mod_count as u64,
                            ),
                        ))
                        .size(self.settings_view_state.font_sizes.update_view.total_size as f32),
                    );
                    ui.add_space(6.0);

                    // Show the live download summary (planned/downloaded, hash,
                    // and speed-history graph) while the transfer is running as
                    // well, not only after it finishes.
                    let summary_visible = (download_finished_for_selected || downloading)
                        && self.download_summary.is_some();
                    let available_height = ui.available_height();
                    let item_spacing = ui.spacing().item_spacing.y;
                    let separator_height = item_spacing + 1.0;
                    let button_height = 48.0;
                    let mut reserved_for_footer = separator_height
                        + progress_footer_top_gap
                        + button_height
                        + progress_footer_bottom_gap
                        + item_spacing;
                    if summary_visible {
                        let heading_font = egui::FontId::proportional(
                            self.settings_view_state.font_sizes.update_view.summary_heading as f32,
                        );
                        let heading_height = ui.fonts_mut(|fonts| fonts.row_height(&heading_font));
                        let body_font = ui
                            .style()
                            .text_styles
                            .get(&TextStyle::Body)
                            .cloned()
                            .unwrap_or_else(|| {
                                egui::FontId::proportional(
                                    self.settings_view_state
                                        .font_sizes
                                        .update_view
                                        .summary_body_fallback as f32,
                                )
                            });
                        let body_height = ui.fonts_mut(|fonts| fonts.row_height(&body_font));
                        let stat_block_height = (2.0 * body_height) + item_spacing + 18.0;
                        let graph_height = 172.0;
                        let summary_height = separator_height
                            + heading_height
                            + 8.0
                            + stat_block_height
                            + 4.0
                            + body_height
                            + item_spacing
                            + graph_height
                            + 6.0;
                        reserved_for_footer += summary_height;
                    }
                    // Reserve space for the TS3 plugin update banner when it
                    // will be visible below the summary.
                    let ts3_banner_visible =
                        download_finished_for_selected && self.ts3_plugin_update_prompt.is_some();
                    if ts3_banner_visible {
                        let body_font = ui
                            .style()
                            .text_styles
                            .get(&TextStyle::Body)
                            .cloned()
                            .unwrap_or_else(|| egui::FontId::proportional(14.0));
                        let line_height = ui.fonts_mut(|fonts| fonts.row_height(&body_font));
                        // Banner: inner_margin(8) top + line + inner_margin(8)
                        // bottom + stroke(1) + item spacing
                        let banner_height = 8.0 + line_height + 8.0 + 1.0 + item_spacing;
                        reserved_for_footer += banner_height;
                    }
                    let list_height = (available_height - reserved_for_footer).max(0.0);
                    ScrollArea::vertical()
                        .max_height(list_height)
                        .min_scrolled_height(list_height)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            let available_card_area_width = ui.available_width();
                            let card_outer_width = update_view_addon_card_outer_width(
                                available_card_area_width,
                                card_horizontal_padding,
                            );
                            let card_gutter_frame = Frame::NONE
                                .inner_margin(Margin::symmetric(card_horizontal_padding as i8, 0));
                            let mod_name_text_size = update_view_scaled_text_size(
                                self.settings_view_state.font_sizes.update_view.mod_name as f32,
                                addon_card_scale,
                            );
                            let mod_status_text_size = update_view_scaled_text_size(
                                self.settings_view_state.font_sizes.update_view.mod_status as f32,
                                addon_card_scale,
                            );
                            let mod_progress_text_size = update_view_scaled_text_size(
                                self.settings_view_state.font_sizes.update_view.mod_progress as f32,
                                addon_card_scale,
                            );
                            let file_details_text_size = update_view_scaled_text_size(
                                self.settings_view_state.font_sizes.update_view.file_details as f32,
                                addon_card_scale,
                            );

                            // Lift the per-file galley cache into a local for the
                            // loop: `m` borrows `self.mod_diff_cache` across the
                            // nested card closures, so the cache cannot be reached
                            // through `self` here. Restored after the loop.
                            let mut file_galleys =
                                std::mem::take(&mut self.update_detail_file_galleys);
                            for (list_position, &mod_idx) in self.update_modal_sorted_mod_indices.iter().enumerate() {
                                ui.push_id(list_position, |ui| {
                                let m = &self.mod_diff_cache[mod_idx];

                                card_gutter_frame.show(ui, |ui| {
                                    let card_frame = Frame {
                                        fill: self.color_card_bg(),
                                        stroke: egui::Stroke::new(1.0, self.color_text_gray()),
                                        corner_radius: CornerRadius::same(5),
                                        inner_margin: Margin::same(8),
                                        ..Default::default()
                                    };
                                    let card_width = update_view_inner_width(
                                        card_outer_width,
                                        card_frame.total_margin().sum().x,
                                    );

                                    card_frame.show(ui, |ui| {
                                        ui.set_width(card_width);

                                        ui.vertical(|ui| {
                                                // Row: [name] ... [Diff (X files) button (right)]
                                                let details_id =
                                                    Id::new(("update-details-toggle", mod_idx, &m.name));
                                                let diff_file_indices: Vec<usize> = m
                                                    .files
                                                    .iter()
                                                    .enumerate()
                                                    .filter_map(|(idx, file)| {
                                                        file.needs_update.then_some(idx)
                                                    })
                                                    .collect();
                                                ui.horizontal(|ui| {
                                                    ui.add(
                                                        eframe::egui::Label::new(
                                                            RichText::new(&m.name)
                                                                .size(mod_name_text_size)
                                                                .color(self.color_text_normal()),
                                                        )
                                                        .truncate(),
                                                    );

                                                    if !diff_file_indices.is_empty() {
                                                        ui.with_layout(
                                                            Layout::right_to_left(Align::Center),
                                                            |ui| {
                                                                let open: bool = ui.data(|d| {
                                                                    d.get_temp(details_id).unwrap_or(false)
                                                                });
                                                                let arrow = if open {
                                                                    "\u{25BC}"
                                                                } else {
                                                                    "\u{25B6}"
                                                                };
                                                                let details_label =
                                                                    self.i18n.tr_plural(
                                                                        "Details ({count} files)",
                                                                        diff_file_indices.len() as u64,
                                                                    );
                                                                let response = ui.add(
                                                                    eframe::egui::Button::new(
                                                                        RichText::new(format!(
                                                                            "{} {}",
                                                                            arrow,
                                                                            details_label
                                                                        ))
                                                                        .color(self.color_text_dim())
                                                                        .size(mod_status_text_size),
                                                                    )
                                                                    .frame(false),
                                                                );
                                                                if response.hovered() {
                                                                    ui.ctx().output_mut(
                                                                        Foxy::set_pointing_cursor_output,
                                                                    );
                                                                }
                                                                if response.clicked() {
                                                                    ui.data_mut(|d| {
                                                                        d.insert_temp(details_id, !open)
                                                                    });
                                                                }
                                                            },
                                                        );
                                                    }
                                                });

                                                // Diff panel (between name and progress bar)
                                                let details_open: bool = ui.data(|d| {
                                                    d.get_temp(details_id).unwrap_or(false)
                                                });
                                                if details_open && !diff_file_indices.is_empty() {
                                                    ui.add_space(2.0);
                                                    let file_font = egui::FontId::proportional(
                                                        file_details_text_size,
                                                    );
                                                    let file_row_height = ui
                                                        .fonts_mut(|fonts| {
                                                            fonts.row_height(&file_font)
                                                        })
                                                        .max(16.0);
                                                    let details_max_height =
                                                        (file_row_height * 10.0) + 4.0;
                                                    let added_color = self.color_success();
                                                    let modified_color = self.color_warn();
                                                    let deleted_color = self.color_error();
                                                    let file_generation =
                                                        update_file_diff_generation(mod_idx, m);
                                                    let file_fingerprint = galley_cache::fingerprint((
                                                        file_details_text_size.to_bits(),
                                                        added_color.to_array(),
                                                        modified_color.to_array(),
                                                        deleted_color.to_array(),
                                                    ));
                                                    file_galleys.ensure(
                                                        diff_file_indices.len(),
                                                        1,
                                                        file_generation,
                                                        file_fingerprint,
                                                    );
                                                    ScrollArea::vertical()
                                                        .id_salt(Id::new((
                                                            "update-details-files",
                                                            mod_idx,
                                                            &m.name,
                                                        )))
                                                        .max_height(details_max_height)
                                                        .auto_shrink([false; 2])
                                                        .show_rows(
                                                            ui,
                                                            file_row_height,
                                                            diff_file_indices.len(),
                                                            |ui, row_range| {
                                                                for row_idx in row_range {
                                                                    let file =
                                                                        &m.files[diff_file_indices[row_idx]];
                                                                    let file_text_color =
                                                                        update_file_diff_kind_color(
                                                                            file.change_kind,
                                                                            added_color,
                                                                            modified_color,
                                                                            deleted_color,
                                                                        );
                                                                    let kind_marker =
                                                                        update_file_diff_kind_marker(
                                                                            file.change_kind,
                                                                        );
                                                                    let row_text =
                                                                        update_file_diff_row_text(
                                                                            file,
                                                                            kind_marker,
                                                                        );
                                                                    let galley = galley_cache::lazy_galley_colored(
                                                                        ui,
                                                                        file_galleys.slot(row_idx, 0),
                                                                        file_font.clone(),
                                                                        file_text_color,
                                                                        || row_text,
                                                                    );
                                                                    ui.add(eframe::egui::Label::new(galley));
                                                                }
                                                            },
                                                        );
                                                }

                                                // Progress bar: status info or download progress
                                                if download_finished_for_selected {
                                                    let finished_text = self.t_fmt(
                                                        "Updated ({size})",
                                                        &[("size", fmt_bytes(m.total_bytes))],
                                                    );
                                                    let response = ui.add(
                                                        ProgressBar::new(1.0)
                                                            .fill(self.color_success()),
                                                    );
                                                    paint_update_progress_bar_text_left(
                                                        ui,
                                                        response.rect,
                                                        finished_text.as_str(),
                                                        mod_progress_text_size,
                                                        egui::Color32::WHITE,
                                                    );
                                                } else if downloading {
                                                    let per_mod =
                                                        self.mod_download_progress.get(&m.name);
                                                    let (pct, bar_text) = if let Some((
                                                        pct,
                                                        files_done,
                                                        files_total,
                                                        bytes_done,
                                                        bytes_total,
                                                    )) = per_mod
                                                    {
                                                        (
                                                            *pct,
                                                            self.t_fmt(
                                                                "Downloading {done}/{total} files ({done_size} / {total_size}) - {pct}",
                                                                &[
                                                                    ("done", files_done.to_string()),
                                                                    ("total", files_total.to_string()),
                                                                    ("done_size", fmt_bytes(*bytes_done)),
                                                                    ("total_size", fmt_bytes(*bytes_total)),
                                                                    ("pct", format!("{:.1}%", pct * 100.0)),
                                                                ],
                                                            ),
                                                        )
                                                    } else {
                                                        (0.0f32, self.t("Waiting to download"))
                                                    };

                                                    let mod_done = pct >= 1.0;
                                                    let fill = if mod_done {
                                                        self.color_success()
                                                    } else {
                                                        self.color_primary_accent()
                                                    };
                                                    let text = if mod_done {
                                                        self.t_fmt(
                                                            "Updated ({size})",
                                                            &[("size", fmt_bytes(m.total_bytes))],
                                                        )
                                                    } else {
                                                        bar_text
                                                    };
                                                    let response =
                                                        ui.add(ProgressBar::new(pct).fill(fill));
                                                    paint_update_progress_bar_text_left(
                                                        ui,
                                                        response.rect,
                                                        text.as_str(),
                                                        mod_progress_text_size,
                                                        egui::Color32::WHITE,
                                                    );
                                                } else if self
                                                    .mod_download_progress
                                                    .get(&m.name)
                                                    .is_some_and(|(pct, ..)| *pct >= 1.0)
                                                {
                                                    let text = self.t_fmt(
                                                        "Updated ({size})",
                                                        &[("size", fmt_bytes(m.total_bytes))],
                                                    );
                                                    let response = ui.add(
                                                        ProgressBar::new(1.0)
                                                            .fill(self.color_success()),
                                                    );
                                                    paint_update_progress_bar_text_left(
                                                        ui,
                                                        response.rect,
                                                        text.as_str(),
                                                        mod_progress_text_size,
                                                        egui::Color32::WHITE,
                                                    );
                                                } else if m.needs_update {
                                                    let changed_parts: usize = m
                                                        .files
                                                        .iter()
                                                        .map(|f| f.changed_parts)
                                                        .sum();
                                                    let status = self.t_fmt(
                                                        "Needs update - {files} files, {parts} parts ({size})",
                                                        &[
                                                            ("files", m.files.len().to_string()),
                                                            ("parts", changed_parts.to_string()),
                                                            ("size", fmt_bytes(m.total_bytes)),
                                                        ],
                                                    );
                                                    let response = ui.add(
                                                        ProgressBar::new(0.0)
                                                            .fill(self.color_text_error()),
                                                    );
                                                    paint_update_progress_bar_text_left(
                                                        ui,
                                                        response.rect,
                                                        status.as_str(),
                                                        mod_progress_text_size,
                                                        self.color_text_normal(),
                                                    );
                                                }
                                        });
                                    });
                                });

                                ui.add_space(card_spacing);
                                });
                            }
                            self.update_detail_file_galleys = file_galleys;
                        });
                }

                if (download_finished_for_selected || downloading)
                    && let Some(summary) = &self.download_summary {
                        ui.separator();
                        self.render_download_summary_stats(ui, summary);
                        self.render_download_summary_speed_graph(ui, summary);
                        ui.add_space(6.0);
                    }

                // TS3 plugin update banner (shown after download completes)
                if download_finished_for_selected {
                    self.render_ts3_plugin_update_banner(ui);
                }

                ui.separator();
                ui.add_space(progress_footer_top_gap);

                let start_disabled = if download_finished_for_selected {
                    false
                } else {
                    (downloading && !download_finished_for_selected)
                        || pending_mod_count == 0
                        || selected.is_none()
                };
                let base_orange = self.color_primary_accent();
                let bg_color = if download_finished_for_selected {
                    self.color_success()
                } else if downloading {
                    self.color_widget_bg()
                } else {
                    base_orange
                };
                let fill_color = if download_finished_for_selected {
                    self.color_success()
                } else {
                    base_orange
                };
                let text = if download_finished_for_selected {
                    self.t("Update finished - click to close")
                } else if downloading {
                    if self.download_paused {
                        self.t("Download paused")
                    } else {
                        let stage_text = if let Some((stage_label, _)) = &self.download_progress {
                            stage_label.clone()
                        } else {
                            self.t("Updating...")
                        };
                        self.update_download_estimate(total_bytes);
                        let speed_text = fmt_speed_mbps(self.download_speed_bps);
                        let elapsed_text = self
                            .download_started_at
                            .map(|t| fmt_duration(t.elapsed()))
                            .unwrap_or_else(|| "00:00".to_string());
                        let remaining_text = self
                            .download_eta_remaining
                            .unwrap_or_else(|| Duration::from_secs(0));
                        self.t_fmt(
                            "{stage} {percent}% - {speed} - {elapsed} elapsed / {remaining} remaining",
                            &[
                                ("stage", stage_text),
                                ("percent", format!("{:.1}", progress_pct * 100.0)),
                                ("speed", speed_text),
                                ("elapsed", elapsed_text),
                                ("remaining", fmt_duration(remaining_text)),
                            ],
                        )
                    }
                } else {
                    self.t("Start update")
                };

                let in_hash_stage = self
                    .download_progress
                    .as_ref()
                    .map(|(label, _)| label == "Hashing..." || label.starts_with("Hash "))
                    .unwrap_or(false);
                let can_toggle_pause = downloading
                    && !download_finished_for_selected
                    && selected.is_some()
                    && !in_hash_stage
                    && !in_cancel_stage;
                let can_cancel = downloading
                    && !download_finished_for_selected
                    && selected.is_some()
                    && !in_hash_stage
                    && !in_cancel_stage;
                let pause_button_font_size =
                    self.settings_view_state.font_sizes.update_view.pause_button as f32;
                let pause_button_width = if can_toggle_pause {
                    (pause_button_font_size * 8.5).max(150.0)
                } else {
                    0.0
                };
                let cancel_button_width = if can_cancel {
                    (pause_button_font_size * 6.5).max(120.0)
                } else {
                    0.0
                };
                let pause_spacing = if can_toggle_pause { 8.0 } else { 0.0 };
                let cancel_spacing = if can_cancel { 8.0 } else { 0.0 };

                // Use explicit footer rects so the progress bar and trailing controls share one inset.
                let footer_height = 48.0;
                let footer_width = ui.available_width().max(0.0);
                let (footer_rect, _) =
                    ui.allocate_exact_size(Vec2::new(footer_width, footer_height), Sense::hover());
                let footer_content_rect =
                    update_view_inset_rect(footer_rect, card_horizontal_padding);
                let mut trailing_x = footer_content_rect.max.x;

                let cancel_rect = if can_cancel {
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(trailing_x - cancel_button_width, footer_content_rect.min.y),
                        Vec2::new(cancel_button_width, footer_height),
                    );
                    trailing_x = rect.min.x - cancel_spacing;
                    Some(rect)
                } else {
                    None
                };
                let pause_rect = if can_toggle_pause {
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(trailing_x - pause_button_width, footer_content_rect.min.y),
                        Vec2::new(pause_button_width, footer_height),
                    );
                    trailing_x = rect.min.x - pause_spacing;
                    Some(rect)
                } else {
                    None
                };

                let progress_width = (trailing_x - footer_content_rect.min.x).max(0.0);
                let rect = egui::Rect::from_min_size(
                    footer_content_rect.min,
                    Vec2::new(progress_width, footer_height),
                );
                let sense = if start_disabled {
                    Sense::hover()
                } else {
                    Sense::click()
                };
                let response = ui.interact(rect, Id::new("repository-update-action"), sense);
                let bg_color = if response.hovered() && !start_disabled {
                    if download_finished_for_selected {
                        self.color_success_muted()
                    } else {
                        self.color_primary_accent_hover()
                    }
                } else {
                    bg_color
                };
                let fill_color = if response.hovered() && !start_disabled {
                    if download_finished_for_selected {
                        self.color_success_muted()
                    } else {
                        self.color_primary_accent_hover()
                    }
                } else {
                    fill_color
                };

                let rounding = CornerRadius::same(6);
                ui.painter().rect_filled(rect, rounding, bg_color);

                if progress_pct > 0.0 {
                    let mut fill_rect = rect;
                    fill_rect.max.x = rect.min.x + (rect.width() * progress_pct).min(rect.width());
                    ui.painter().rect_filled(fill_rect, rounding, fill_color);
                }

                ui.painter().rect_stroke(
                    rect,
                    rounding,
                    (1.0, self.color_text_dim()),
                    egui::StrokeKind::Outside,
                );

                let text_color = if downloading && !download_finished_for_selected {
                    egui::Color32::WHITE
                } else if start_disabled {
                    self.color_text_gray()
                } else {
                    self.color_text_normal()
                };
                if rect.width() > 0.0 {
                    let max_text_font_size = pause_button_font_size.min(18.0);
                    let max_text_font = egui::FontId::proportional(max_text_font_size);
                    let max_text_width = ui
                        .painter()
                        .layout_no_wrap(text.clone(), max_text_font, text_color)
                        .size()
                        .x;
                    let text_font_size = update_view_downloader_bar_text_size(
                        max_text_font_size,
                        max_text_width,
                        rect.width(),
                    );
                    paint_update_progress_bar_text_center(
                        ui,
                        rect,
                        text.as_str(),
                        text_font_size,
                        text_color,
                    );
                }

                if response.hovered() && !start_disabled {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if response.clicked() && !start_disabled {
                    if download_finished_for_selected {
                        if let Some(repo_idx) = self.download_finished_repo {
                            self.acknowledge_update_summary_for_repo(repo_idx);
                        }
                        self.download_finished = false;
                        self.download_finished_repo = None;
                        self.download_progress = None;
                        self.download_summary = None;
                        self.update_modal_open = false;
                        self.current_view = FoxyView::RepositoryList;
                        info!("Closed repository update modal after successful download");
                    } else if let Some(idx) = selected {
                        let repo_name = self
                            .repository_view_state
                            .repositories
                            .get(idx)
                            .map(|r| r.name.clone())
                            .unwrap_or_else(|| format!("index {}", idx));
                        info!("Starting update download for repository {}", repo_name);
                        self.update_ready_repo = None;
                        self.download_progress = None;
                        self.download_finished = false;
                        self.download_finished_repo = None;
                        start_download = Some(idx);
                    }
                }

                if let Some(pause_rect) = pause_rect {
                    let pause_label = if self.download_paused {
                        self.t("Resume download")
                    } else {
                        self.t("Pause download")
                    };
                    let pause_button = ui.put(
                        pause_rect,
                        Button::new(RichText::new(pause_label).size(pause_button_font_size)).fill(
                            if self.download_paused {
                                self.color_primary_accent()
                            } else {
                                self.color_widget_bg()
                            },
                        ),
                    );
                    if pause_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if pause_button.clicked() {
                        set_download_paused = Some(!self.download_paused);
                    }
                }

                if let Some(cancel_rect) = cancel_rect {
                    let cancel_label = self.t("Cancel");
                    let cancel_button = ui.put(
                        cancel_rect,
                        Button::new(RichText::new(cancel_label).size(pause_button_font_size))
                            .fill(self.color_action_destructive()),
                    );
                    if cancel_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_button.clicked() {
                        cancel_requested = true;
                    }
                }

                ui.add_space(progress_footer_bottom_gap);
            });
        });

        if cancel_requested {
            self.cancel_sync();
        }
        if let Some(paused) = set_download_paused {
            self.set_download_paused(paused);
        }
        if let Some(idx) = start_download {
            self.start_core_sync(idx, SyncMode::Download);
        }
    }

    fn render_download_summary_stats(
        &self,
        ui: &mut Ui,
        summary: &crate::ui::types::DownloadSummary,
    ) {
        let heading_size = self
            .settings_view_state
            .font_sizes
            .update_view
            .summary_heading as f32;
        let updated_text = self.t_fmt(
            "Updated: {mods} mods, {files} files, {parts} parts",
            &[
                ("mods", summary.mods_updated.to_string()),
                ("files", summary.files_updated.to_string()),
                ("parts", summary.parts_updated.to_string()),
            ],
        );

        ui.horizontal_wrapped(|ui| {
            ui.heading(RichText::new(self.t("Download Summary")).size(heading_size));
            ui.label(RichText::new("-").size(heading_size));
            ui.label(RichText::new(updated_text).size(heading_size * 0.62));
        });
        ui.add_space(8.0);

        let after_download_hash_duration = summary.after_download_or_legacy_hash_duration();
        let displayed_total_duration =
            summary.download_stage_duration + after_download_hash_duration;
        let stat_fill = self.color_main_bg();
        let stat_stroke = egui::Stroke::new(1.0, self.color_text_gray());
        let stat_frame = || Frame {
            fill: stat_fill,
            stroke: stat_stroke,
            corner_radius: CornerRadius::same(4),
            inner_margin: Margin::symmetric(10, 8),
            ..Default::default()
        };

        ui.columns(3, |columns| {
            columns[0].vertical(|ui| {
                stat_frame().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(self.t_fmt(
                        "Planned transfer: {size}",
                        &[("size", fmt_bytes(summary.planned_or_downloaded_bytes()))],
                    ));
                    ui.label(self.t_fmt(
                        "Downloaded: {size} at {speed} avg",
                        &[
                            ("size", fmt_bytes(summary.downloaded_bytes)),
                            ("speed", fmt_speed_mbps(summary.avg_speed_bps)),
                        ],
                    ));
                });
            });
            columns[1].vertical(|ui| {
                stat_frame().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(self.t_fmt(
                        "Cumulative hash: {duration}",
                        &[(
                            "duration",
                            fmt_duration_ms(summary.cumulative_or_after_download_hash_duration()),
                        )],
                    ));
                    ui.label(self.t_fmt(
                        "After download hash: {duration}",
                        &[("duration", fmt_duration_ms(after_download_hash_duration))],
                    ));
                });
            });
            columns[2].vertical(|ui| {
                stat_frame().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(self.t_fmt(
                        "Download stage: {duration}",
                        &[("duration", fmt_duration_ms(summary.download_stage_duration))],
                    ));
                    ui.label(self.t_fmt(
                        "Total duration: {duration}",
                        &[("duration", fmt_duration_ms(displayed_total_duration))],
                    ));
                });
            });
        });
    }

    fn render_download_summary_speed_graph(
        &self,
        ui: &mut Ui,
        summary: &crate::ui::types::DownloadSummary,
    ) {
        ui.add_space(4.0);
        ui.label(self.t("Speed history"));
        let graph_width = ui.available_width().max(0.0);
        let graph_height = 172.0;
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(graph_width, graph_height), Sense::hover());

        let painter = ui.painter_at(rect);
        let graph_bg = self.color_main_bg();
        painter.rect_filled(rect, CornerRadius::same(4), graph_bg);
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            (1.0, self.color_text_gray()),
            egui::StrokeKind::Outside,
        );

        if summary.telemetry_samples.len() < 2 {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                self.t("No throughput samples recorded"),
                egui::FontId::proportional(13.0),
                self.color_text_dim(),
            );
            return;
        }

        let content_rect = rect.shrink2(Vec2::new(10.0, 8.0));
        let time_axis_height = 18.0;
        let lanes_rect = egui::Rect::from_min_max(
            content_rect.min,
            egui::pos2(content_rect.max.x, content_rect.max.y - time_axis_height),
        );
        let label_width = (lanes_rect.width() * 0.24).clamp(104.0, 172.0);
        let plot_left = lanes_rect.left() + label_width;
        let plot_width = (lanes_rect.right() - plot_left).max(1.0);
        let lane_count = 5.0;
        let lane_gap = 5.0;
        let lane_height =
            ((lanes_rect.height() - (lane_gap * (lane_count - 1.0))) / lane_count).max(1.0);

        let first_ms = summary
            .telemetry_samples
            .first()
            .map(|sample| sample.elapsed_ms)
            .unwrap_or(0);
        let last_ms = summary
            .telemetry_samples
            .last()
            .map(|sample| sample.elapsed_ms)
            .unwrap_or(first_ms + 1)
            .max(first_ms + 1);

        let scale = SummaryGraphScale { first_ms, last_ms };
        let hovered_x = response
            .hover_pos()
            .map(|pos| pos.x.clamp(plot_left, plot_left + plot_width))
            .unwrap_or(plot_left + plot_width);
        let selected_idx = nearest_summary_sample_index(
            &summary.telemetry_samples,
            scale,
            plot_left,
            plot_width,
            hovered_x,
        );
        let selected_download_idx = nearest_summary_sample_index_with_value(
            &summary.telemetry_samples,
            scale,
            plot_left,
            plot_width,
            hovered_x,
            SummaryGraphValue::DownloadMbps,
        )
        .unwrap_or(selected_idx);
        let selected_disk_write_idx = nearest_summary_sample_index_with_value(
            &summary.telemetry_samples,
            scale,
            plot_left,
            plot_width,
            hovered_x,
            SummaryGraphValue::DiskWriteMbps,
        )
        .unwrap_or(selected_idx);
        let selected_hash_idx = nearest_summary_sample_index_with_value(
            &summary.telemetry_samples,
            scale,
            plot_left,
            plot_width,
            hovered_x,
            SummaryGraphValue::HashFilesPerSec,
        )
        .unwrap_or(selected_idx);
        let selected_cpu_idx = nearest_summary_sample_index_with_value(
            &summary.telemetry_samples,
            scale,
            plot_left,
            plot_width,
            hovered_x,
            SummaryGraphValue::CpuPercent,
        )
        .unwrap_or(selected_idx);
        let selected_memory_idx = nearest_summary_sample_index_with_value(
            &summary.telemetry_samples,
            scale,
            plot_left,
            plot_width,
            hovered_x,
            SummaryGraphValue::MemoryBytes,
        )
        .unwrap_or(selected_idx);
        let selected_sample = &summary.telemetry_samples[selected_idx];
        let selected_download_sample = &summary.telemetry_samples[selected_download_idx];
        let selected_disk_write_sample = &summary.telemetry_samples[selected_disk_write_idx];
        let selected_hash_sample = &summary.telemetry_samples[selected_hash_idx];
        let selected_cpu_sample = &summary.telemetry_samples[selected_cpu_idx];
        let selected_memory_sample = &summary.telemetry_samples[selected_memory_idx];
        let download_max = max_summary_value(&summary.telemetry_samples, |sample| {
            mbps_value(sample.download_bps)
        });
        let disk_write_max = max_summary_value(&summary.telemetry_samples, |sample| {
            mbps_value(sample.disk_write_bps)
        });
        let hash_max = max_summary_value(&summary.telemetry_samples, |sample| {
            sample.hash_files_per_sec
        });
        let cpu_max = max_summary_value(&summary.telemetry_samples, |sample| sample.cpu_percent);
        let memory_max = max_summary_value(&summary.telemetry_samples, |sample| {
            sample.memory_bytes as f64
        });

        let lane_specs = [
            SummaryGraphLaneSpec {
                row: 0,
                selected_idx: selected_download_idx,
                label: self.t("Download speed"),
                value_text: fmt_speed_mbps(selected_download_sample.download_bps),
                peak_text: self.t_fmt(
                    "Peak {value}",
                    &[("value", fmt_speed_mbps(download_max / 8.0 * 1_000_000.0))],
                ),
                max_value: download_max,
                color: self.color_primary_accent(),
            },
            SummaryGraphLaneSpec {
                row: 1,
                selected_idx: selected_disk_write_idx,
                label: self.t("Disk write speed"),
                value_text: fmt_speed_mbps(selected_disk_write_sample.disk_write_bps),
                peak_text: self.t_fmt(
                    "Peak {value}",
                    &[("value", fmt_speed_mbps(disk_write_max / 8.0 * 1_000_000.0))],
                ),
                max_value: disk_write_max,
                color: self.color_success(),
            },
            SummaryGraphLaneSpec {
                row: 2,
                selected_idx: selected_hash_idx,
                label: self.t("Hash files/s"),
                value_text: self.t_fmt(
                    "{value} files/s",
                    &[(
                        "value",
                        format!("{:.1}", selected_hash_sample.hash_files_per_sec),
                    )],
                ),
                peak_text: self.t_fmt(
                    "Peak {value}",
                    &[(
                        "value",
                        self.t_fmt("{value} files/s", &[("value", format!("{:.1}", hash_max))]),
                    )],
                ),
                max_value: hash_max,
                color: self.color_text_normal(),
            },
            SummaryGraphLaneSpec {
                row: 3,
                selected_idx: selected_cpu_idx,
                label: "CPU".to_string(),
                value_text: format_percent(selected_cpu_sample.cpu_percent),
                peak_text: self.t_fmt("Peak {value}", &[("value", format_percent(cpu_max))]),
                max_value: cpu_max,
                color: self.color_warn(),
            },
            SummaryGraphLaneSpec {
                row: 4,
                selected_idx: selected_memory_idx,
                label: self.t("Memory"),
                value_text: fmt_bytes(selected_memory_sample.memory_bytes),
                peak_text: self.t_fmt("Peak {value}", &[("value", fmt_bytes(memory_max as u64))]),
                max_value: memory_max,
                color: self.color_action_info(),
            },
        ];

        for spec in &lane_specs {
            let top = lanes_rect.top() + (spec.row as f32 * (lane_height + lane_gap));
            let lane_rect = egui::Rect::from_min_size(
                egui::pos2(lanes_rect.left(), top),
                Vec2::new(lanes_rect.width(), lane_height),
            );
            let plot_rect = egui::Rect::from_min_max(
                egui::pos2(plot_left, lane_rect.top()),
                egui::pos2(plot_left + plot_width, lane_rect.bottom()),
            );
            let value = match spec.row {
                0 => SummaryGraphValue::DownloadMbps,
                1 => SummaryGraphValue::DiskWriteMbps,
                2 => SummaryGraphValue::HashFilesPerSec,
                3 => SummaryGraphValue::CpuPercent,
                _ => SummaryGraphValue::MemoryBytes,
            };
            draw_summary_lane(
                SummaryGraphLaneContext {
                    painter: &painter,
                    lane_rect,
                    plot_rect,
                    scale,
                    samples: &summary.telemetry_samples,
                    selected_idx: spec.selected_idx,
                    value,
                },
                SummaryGraphLaneStyle {
                    label: &spec.label,
                    value_text: &spec.value_text,
                    peak_text: &spec.peak_text,
                    color: spec.color,
                    text_color: self.color_text_normal(),
                    dim_color: self.color_text_gray(),
                    max_value: spec.max_value,
                },
            );
        }

        let axis_y = content_rect.bottom() - 9.0;
        let start_label = format_summary_elapsed_seconds(first_ms);
        let middle_label = format_summary_elapsed_seconds(first_ms + ((last_ms - first_ms) / 2));
        let end_label = format_summary_elapsed_seconds(last_ms);
        painter.text(
            egui::pos2(plot_left, axis_y),
            Align2::LEFT_CENTER,
            start_label,
            egui::FontId::proportional(11.0),
            self.color_text_dim(),
        );
        painter.text(
            egui::pos2(plot_left + (plot_width * 0.5), axis_y),
            Align2::CENTER_CENTER,
            middle_label,
            egui::FontId::proportional(11.0),
            self.color_text_dim(),
        );
        painter.text(
            egui::pos2(plot_left + plot_width, axis_y),
            Align2::RIGHT_CENTER,
            end_label,
            egui::FontId::proportional(11.0),
            self.color_text_dim(),
        );
        painter.text(
            egui::pos2(lanes_rect.left(), axis_y),
            Align2::LEFT_CENTER,
            self.t("Elapsed time"),
            egui::FontId::proportional(11.0),
            self.color_text_dim(),
        );

        if response.hovered() {
            ui.ctx().output_mut(|output| {
                output.cursor_icon = CursorIcon::Crosshair;
            });
            let cpu_tooltip = format!("CPU: {}", format_percent(selected_cpu_sample.cpu_percent));
            let memory_tooltip = format!(
                "{}: {}",
                self.t("Memory"),
                fmt_bytes(selected_memory_sample.memory_bytes)
            );
            let tooltip = format!(
                "{}\n{}\n{}\n{}\n{}\n{}",
                self.t_fmt(
                    "Elapsed: {time}",
                    &[(
                        "time",
                        format_summary_elapsed_seconds(selected_sample.elapsed_ms)
                    )]
                ),
                self.t_fmt(
                    "Download: {speed}",
                    &[(
                        "speed",
                        fmt_speed_mbps(selected_download_sample.download_bps)
                    )]
                ),
                self.t_fmt(
                    "Disk write: {speed}",
                    &[(
                        "speed",
                        fmt_speed_mbps(selected_disk_write_sample.disk_write_bps)
                    )]
                ),
                self.t_fmt(
                    "Hash files: {speed}",
                    &[(
                        "speed",
                        self.t_fmt(
                            "{value} files/s",
                            &[(
                                "value",
                                format!("{:.1}", selected_hash_sample.hash_files_per_sec)
                            )],
                        ),
                    )]
                ),
                cpu_tooltip,
                memory_tooltip,
            );
            let _ = response.on_hover_text(tooltip);
        }
    }
}

#[derive(Clone, Copy)]
struct SummaryGraphScale {
    first_ms: u64,
    last_ms: u64,
}

struct SummaryGraphLaneSpec {
    row: usize,
    selected_idx: usize,
    label: String,
    value_text: String,
    peak_text: String,
    max_value: f64,
    color: egui::Color32,
}

struct SummaryGraphLaneContext<'a> {
    painter: &'a egui::Painter,
    lane_rect: egui::Rect,
    plot_rect: egui::Rect,
    scale: SummaryGraphScale,
    samples: &'a [crate::ui::types::DownloadTelemetrySample],
    selected_idx: usize,
    value: SummaryGraphValue,
}

struct SummaryGraphLaneStyle<'a> {
    label: &'a str,
    value_text: &'a str,
    peak_text: &'a str,
    color: egui::Color32,
    text_color: egui::Color32,
    dim_color: egui::Color32,
    max_value: f64,
}

#[derive(Clone, Copy)]
enum SummaryGraphValue {
    DownloadMbps,
    DiskWriteMbps,
    HashFilesPerSec,
    CpuPercent,
    MemoryBytes,
}

fn draw_summary_lane(context: SummaryGraphLaneContext<'_>, style: SummaryGraphLaneStyle<'_>) {
    let painter = context.painter;
    let lane_rect = context.lane_rect;
    let plot_rect = context.plot_rect;
    let scale = context.scale;
    let duration_ms = scale.last_ms.saturating_sub(scale.first_ms).max(1) as f32;
    let max_value = style.max_value.max(1.0);
    let points: Vec<_> = context
        .samples
        .iter()
        .enumerate()
        .filter_map(|(idx, sample)| {
            let value = summary_graph_value(sample, context.value);
            (value > 0.0).then_some((idx, sample, value))
        })
        .map(|(idx, sample, value)| {
            let x = plot_rect.left()
                + (sample.elapsed_ms.saturating_sub(scale.first_ms) as f32 / duration_ms)
                    * plot_rect.width();
            let normalized = (value / max_value).clamp(0.0, 1.0) as f32;
            let y = plot_rect.bottom() - normalized * plot_rect.height();
            (idx, egui::pos2(x, y))
        })
        .collect();

    painter.line_segment(
        [
            egui::pos2(plot_rect.left(), plot_rect.bottom()),
            egui::pos2(plot_rect.right(), plot_rect.bottom()),
        ],
        egui::Stroke::new(1.0, style.dim_color),
    );
    painter.text(
        lane_rect.left_top(),
        Align2::LEFT_TOP,
        style.label,
        egui::FontId::proportional(11.0),
        style.text_color,
    );
    painter.text(
        lane_rect.left_bottom(),
        Align2::LEFT_BOTTOM,
        style.value_text,
        egui::FontId::proportional(11.0),
        style.dim_color,
    );
    painter.text(
        plot_rect.right_top(),
        Align2::RIGHT_TOP,
        style.peak_text,
        egui::FontId::proportional(10.0),
        style.dim_color,
    );
    if points.len() >= 2 {
        painter.add(egui::Shape::line(
            points.iter().map(|(_, point)| *point).collect(),
            egui::Stroke::new(2.0, style.color),
        ));
    } else if let Some((_, point)) = points.first() {
        painter.circle_filled(*point, 2.0, style.color);
    }
    if let Some((_, point)) = points.iter().find(|(idx, _)| *idx == context.selected_idx) {
        painter.line_segment(
            [
                egui::pos2(point.x, plot_rect.top()),
                egui::pos2(point.x, plot_rect.bottom()),
            ],
            egui::Stroke::new(1.0, style.dim_color),
        );
        painter.circle_filled(*point, 3.0, style.color);
    }
}

fn summary_graph_value(
    sample: &crate::ui::types::DownloadTelemetrySample,
    value: SummaryGraphValue,
) -> f64 {
    match value {
        SummaryGraphValue::DownloadMbps => mbps_value(sample.download_bps),
        SummaryGraphValue::DiskWriteMbps => mbps_value(sample.disk_write_bps),
        SummaryGraphValue::HashFilesPerSec => sample.hash_files_per_sec,
        SummaryGraphValue::CpuPercent => sample.cpu_percent,
        SummaryGraphValue::MemoryBytes => sample.memory_bytes as f64,
    }
}

fn max_summary_value(
    samples: &[crate::ui::types::DownloadTelemetrySample],
    value: impl Fn(&crate::ui::types::DownloadTelemetrySample) -> f64,
) -> f64 {
    samples.iter().map(value).fold(0.0_f64, f64::max).max(1.0)
}

fn mbps_value(bytes_per_sec: f64) -> f64 {
    (bytes_per_sec * 8.0) / 1_000_000.0
}

fn format_percent(value: f64) -> String {
    format!("{:.1}%", value.max(0.0))
}

fn nearest_summary_sample_index(
    samples: &[crate::ui::types::DownloadTelemetrySample],
    scale: SummaryGraphScale,
    plot_left: f32,
    plot_width: f32,
    x: f32,
) -> usize {
    let duration_ms = scale.last_ms.saturating_sub(scale.first_ms).max(1) as f32;
    samples
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let ax = plot_left
                + (a.elapsed_ms.saturating_sub(scale.first_ms) as f32 / duration_ms) * plot_width;
            let bx = plot_left
                + (b.elapsed_ms.saturating_sub(scale.first_ms) as f32 / duration_ms) * plot_width;
            (ax - x).abs().total_cmp(&(bx - x).abs())
        })
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

fn nearest_summary_sample_index_with_value(
    samples: &[crate::ui::types::DownloadTelemetrySample],
    scale: SummaryGraphScale,
    plot_left: f32,
    plot_width: f32,
    x: f32,
    value: SummaryGraphValue,
) -> Option<usize> {
    let duration_ms = scale.last_ms.saturating_sub(scale.first_ms).max(1) as f32;
    samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| summary_graph_value(sample, value) > 0.0)
        .min_by(|(_, a), (_, b)| {
            let ax = plot_left
                + (a.elapsed_ms.saturating_sub(scale.first_ms) as f32 / duration_ms) * plot_width;
            let bx = plot_left
                + (b.elapsed_ms.saturating_sub(scale.first_ms) as f32 / duration_ms) * plot_width;
            (ax - x).abs().total_cmp(&(bx - x).abs())
        })
        .map(|(idx, _)| idx)
}

fn format_summary_elapsed_seconds(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms as f64 / 1_000.0;
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        let minutes = (seconds / 60.0).floor() as u64;
        let remaining = seconds - (minutes as f64 * 60.0);
        format!("{minutes}m {remaining:04.1}s")
    }
}

fn update_view_inset_width(available_width: f32, horizontal_padding: f32) -> f32 {
    (available_width - (2.0 * horizontal_padding)).max(0.0)
}

fn update_view_inner_width(outer_width: f32, frame_horizontal_margin: f32) -> f32 {
    (outer_width - frame_horizontal_margin).max(0.0)
}

fn update_view_addon_card_outer_width(available_width: f32, horizontal_padding: f32) -> f32 {
    update_view_inset_width(available_width, horizontal_padding)
}

fn update_view_scaled_text_size(font_size: f32, scale: f32) -> f32 {
    (font_size * scale).max(1.0)
}

fn update_file_diff_generation(mod_idx: usize, mod_summary: &ModDiffSummary) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    mod_idx.hash(&mut hasher);
    mod_summary.name.hash(&mut hasher);
    mod_summary.files.len().hash(&mut hasher);
    for file in &mod_summary.files {
        file.name.hash(&mut hasher);
        file.needs_update.hash(&mut hasher);
        file.total_bytes.hash(&mut hasher);
        file.changed_parts.hash(&mut hasher);
        update_file_diff_kind_code(file.change_kind).hash(&mut hasher);
    }
    hasher.finish()
}

fn update_file_diff_kind_code(kind: FileDiffKind) -> u8 {
    match kind {
        FileDiffKind::Added => 1,
        FileDiffKind::Modified => 2,
        FileDiffKind::Deleted => 3,
    }
}

fn update_file_diff_kind_marker(kind: FileDiffKind) -> &'static str {
    match kind {
        FileDiffKind::Added => "+",
        FileDiffKind::Modified => "~",
        FileDiffKind::Deleted => "-",
    }
}

fn update_file_diff_kind_color(
    kind: FileDiffKind,
    added_color: egui::Color32,
    modified_color: egui::Color32,
    deleted_color: egui::Color32,
) -> egui::Color32 {
    match kind {
        FileDiffKind::Added => added_color,
        FileDiffKind::Modified => modified_color,
        FileDiffKind::Deleted => deleted_color,
    }
}

fn update_file_diff_row_text(
    file: &crate::core::api::FileDiffSummary,
    kind_marker: &str,
) -> String {
    if file.change_kind == FileDiffKind::Deleted && file.total_bytes == 0 && file.changed_parts == 0
    {
        return format!("{} {}", kind_marker, file.name);
    }

    let parts_text = if file.changed_parts > 0 {
        format!(
            ", {} part{}",
            file.changed_parts,
            if file.changed_parts == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    format!(
        "{} {} ({}{})",
        kind_marker,
        file.name,
        fmt_bytes(file.total_bytes),
        parts_text
    )
}

fn update_view_inset_rect(rect: egui::Rect, horizontal_padding: f32) -> egui::Rect {
    let padding = horizontal_padding.min(rect.width() * 0.5);
    rect.shrink2(Vec2::new(padding, 0.0))
}

fn paint_update_progress_bar_text_left(
    ui: &Ui,
    rect: egui::Rect,
    text: &str,
    font_size: f32,
    color: egui::Color32,
) {
    let pos = rect.left_center() + Vec2::new(ui.spacing().item_spacing.x, 0.0);
    paint_update_progress_bar_text(ui, rect, pos, Align2::LEFT_CENTER, text, font_size, color);
}

fn paint_update_progress_bar_text_center(
    ui: &Ui,
    rect: egui::Rect,
    text: &str,
    font_size: f32,
    color: egui::Color32,
) {
    paint_update_progress_bar_text(
        ui,
        rect,
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font_size,
        color,
    );
}

fn paint_update_progress_bar_text(
    ui: &Ui,
    rect: egui::Rect,
    pos: egui::Pos2,
    align: Align2,
    text: &str,
    font_size: f32,
    color: egui::Color32,
) {
    let font_id = egui::FontId::proportional(font_size);
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        pos + Vec2::new(1.0, 1.0),
        align,
        text,
        font_id.clone(),
        egui::Color32::BLACK,
    );
    painter.text(pos, align, text, font_id, color);
}

fn update_view_downloader_bar_text_size(
    max_font_size: f32,
    measured_text_width: f32,
    available_width: f32,
) -> f32 {
    const TEXT_HORIZONTAL_PADDING: f32 = 8.0;
    const FIT_SAFETY_SCALE: f32 = 0.98;
    const MIN_FONT_SIZE: f32 = 8.0;

    let max_font_size = max_font_size.max(MIN_FONT_SIZE);
    if measured_text_width <= 0.0 {
        return max_font_size;
    }

    let target_width = (available_width - (TEXT_HORIZONTAL_PADDING * 2.0)).max(0.0);
    if measured_text_width <= target_width {
        return max_font_size;
    }

    (max_font_size * (target_width / measured_text_width) * FIT_SAFETY_SCALE)
        .clamp(MIN_FONT_SIZE, max_font_size)
}

fn repository_update_cancel_stage(
    download_progress: &Option<(String, f32)>,
    cancelling_label: &str,
    reverting_label: &str,
) -> bool {
    download_progress
        .as_ref()
        .map(|(label, _)| {
            label == "Cancelling..."
                || label == "Reverting changes"
                || label == cancelling_label
                || label == reverting_label
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_view_inset_width_preserves_equal_gutters() {
        let available_width = 1_000.0;
        let horizontal_padding = 15.0;

        let inset_width = update_view_inset_width(available_width, horizontal_padding);

        assert_eq!(inset_width, 970.0);
        assert_eq!(available_width - inset_width, horizontal_padding * 2.0);
    }

    #[test]
    fn update_view_inner_width_accounts_for_visible_frame_margin() {
        let visible_outer_width = 970.0;
        let frame_horizontal_margin = 18.0;

        let inner_width = update_view_inner_width(visible_outer_width, frame_horizontal_margin);

        assert_eq!(inner_width, 952.0);
        assert_eq!(inner_width + frame_horizontal_margin, visible_outer_width);
    }

    #[test]
    fn update_view_addon_card_outer_width_fills_inset_width() {
        let available_width = 1_000.0;
        let horizontal_padding = 15.0;

        let card_width = update_view_addon_card_outer_width(available_width, horizontal_padding);

        assert_eq!(card_width, 970.0);
    }

    #[test]
    fn update_view_scaled_text_size_halves_font_size() {
        let text_size = update_view_scaled_text_size(18.0, 0.5);

        assert_eq!(text_size, 9.0);
    }

    #[test]
    fn update_file_diff_row_text_includes_change_kind_size_and_parts() {
        let file = crate::core::api::FileDiffSummary {
            name: "addons/main.pbo".to_string(),
            needs_update: true,
            total_bytes: 2048,
            changed_parts: 2,
            change_kind: FileDiffKind::Modified,
        };

        let text = update_file_diff_row_text(&file, "~");

        assert_eq!(text, "~ addons/main.pbo (2.0 KB, 2 parts)");
    }

    #[test]
    fn update_file_diff_row_text_omits_empty_deleted_size() {
        let file = crate::core::api::FileDiffSummary {
            name: "addons/old.pbo".to_string(),
            needs_update: true,
            total_bytes: 0,
            changed_parts: 0,
            change_kind: FileDiffKind::Deleted,
        };

        let text = update_file_diff_row_text(&file, "-");

        assert_eq!(text, "- addons/old.pbo");
    }

    #[test]
    fn update_view_inset_rect_preserves_equal_left_and_right_edges() {
        let rect = egui::Rect::from_min_size(egui::pos2(50.0, 10.0), Vec2::new(1_000.0, 48.0));

        let inset_rect = update_view_inset_rect(rect, 15.0);

        assert_eq!(inset_rect.min.x - rect.min.x, 15.0);
        assert_eq!(rect.max.x - inset_rect.max.x, 15.0);
        assert_eq!(inset_rect.min.y, rect.min.y);
        assert_eq!(inset_rect.max.y, rect.max.y);
    }

    #[test]
    fn update_view_downloader_bar_text_size_uses_max_when_text_fits() {
        let size = update_view_downloader_bar_text_size(16.0, 200.0, 260.0);

        assert_eq!(size, 16.0);
    }

    #[test]
    fn update_view_downloader_bar_text_size_scales_down_when_space_is_tight() {
        let size = update_view_downloader_bar_text_size(16.0, 400.0, 316.0);

        assert!((size - 11.76).abs() < 0.001);
    }

    #[test]
    fn update_view_downloader_bar_text_size_keeps_readable_minimum() {
        let size = update_view_downloader_bar_text_size(16.0, 400.0, 100.0);

        assert_eq!(size, 8.0);
    }

    #[test]
    fn repository_update_cancel_stage_matches_raw_or_translated_labels() {
        assert!(repository_update_cancel_stage(
            &Some(("Cancelling...".to_string(), 0.84)),
            "Cancel translated",
            "Revert translated",
        ));
        assert!(repository_update_cancel_stage(
            &Some(("Revert translated".to_string(), 0.2)),
            "Cancel translated",
            "Revert translated",
        ));
        assert!(!repository_update_cancel_stage(
            &Some(("Download 1/2".to_string(), 0.5)),
            "Cancel translated",
            "Revert translated",
        ));
    }

    #[test]
    fn repository_update_cancel_stage_none_progress_returns_false() {
        assert!(!repository_update_cancel_stage(&None, "Cancel", "Revert"));
    }

    #[test]
    fn repository_update_cancel_stage_reverting_changes_raw() {
        assert!(repository_update_cancel_stage(
            &Some(("Reverting changes".to_string(), 0.1)),
            "Irrelevant",
            "Irrelevant",
        ));
    }

    // ── update_view_inset_width: additional ────────────────────────────

    #[test]
    fn update_view_inset_width_zero_available() {
        assert_eq!(update_view_inset_width(0.0, 15.0), 0.0);
    }

    #[test]
    fn update_view_inset_width_padding_exceeds_available() {
        // When padding * 2 > available, should clamp to 0
        assert_eq!(update_view_inset_width(20.0, 15.0), 0.0);
    }

    // ── update_view_inner_width: additional ────────────────────────────

    #[test]
    fn update_view_inner_width_zero_margin() {
        assert_eq!(update_view_inner_width(500.0, 0.0), 500.0);
    }

    #[test]
    fn update_view_inner_width_margin_exceeds_outer() {
        assert_eq!(update_view_inner_width(10.0, 20.0), 0.0);
    }

    // ── update_view_downloader_bar_text_size: additional ───────────────

    #[test]
    fn update_view_downloader_bar_text_size_zero_measured_width() {
        let size = update_view_downloader_bar_text_size(16.0, 0.0, 300.0);
        assert_eq!(size, 16.0);
    }

    #[test]
    fn update_view_downloader_bar_text_size_zero_available_width() {
        let size = update_view_downloader_bar_text_size(16.0, 200.0, 0.0);
        assert_eq!(size, 8.0); // MIN_FONT_SIZE
    }

    #[test]
    fn update_view_downloader_bar_text_size_max_below_min_uses_min() {
        let size = update_view_downloader_bar_text_size(5.0, 200.0, 300.0);
        assert_eq!(size, 8.0); // MIN_FONT_SIZE
    }

    // ── update_view_inset_rect: additional ─────────────────────────────

    #[test]
    fn update_view_inset_rect_zero_padding() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(100.0, 50.0));
        let inset = update_view_inset_rect(rect, 0.0);
        assert_eq!(inset, rect);
    }

    #[test]
    fn update_view_inset_rect_large_padding_clamps() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(20.0, 50.0));
        let inset = update_view_inset_rect(rect, 100.0);
        // Padding clamped to half of width
        assert!(inset.width() >= 0.0);
    }
}
