use crate::core::api;
use crate::ui::app::Foxy;
use eframe::egui::{
    self, CentralPanel, CollapsingHeader, Frame, Grid, Id, Label, Margin, RichText, ScrollArea, Ui,
    Vec2, ViewportBuilder, ViewportClass, ViewportId,
};
use std::time::Instant;

impl Foxy {
    fn render_memory_comparison_row(
        &self,
        ui: &mut Ui,
        label: String,
        current: String,
        delta: String,
        tooltip: Option<&str>,
    ) {
        let label_response = ui.label(label);
        if let Some(tip) = tooltip {
            label_response.on_hover_text(tip);
        }
        ui.label(current);
        ui.label(delta);
        ui.end_row();
    }

    fn render_memory_metric_card(
        &self,
        ui: &mut Ui,
        title: String,
        value: String,
        note: Option<String>,
        tooltip: Option<&str>,
    ) {
        Frame::group(ui.style())
            .fill(self.color_card_bg())
            .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_min_size(Vec2::new(ui.available_width().max(0.0), 58.0));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 4.0);
                    let title_label =
                        ui.label(RichText::new(title).small().color(self.color_text_dim()));
                    if let Some(tip) = tooltip {
                        title_label.on_hover_text(tip);
                    }
                    ui.label(RichText::new(value).strong().size(18.0));
                });
                if let Some(note) = note {
                    ui.add_space(2.0);
                    ui.add(
                        Label::new(RichText::new(note).small().color(self.color_text_gray()))
                            .wrap(),
                    );
                }
            });
    }

    fn render_operation_diagnostics_summary(&self, ui: &mut Ui) {
        let logger_health = api::logger_health();
        let errors = self
            .activity_log_cache
            .iter()
            .filter(|entry| entry.level == "ERROR")
            .count();
        let warnings = self
            .activity_log_cache
            .iter()
            .filter(|entry| entry.level == "WARN")
            .count();
        let active_operation = if let Some(mode) = self.current_sync_mode {
            format!("Repository {mode:?}")
        } else if self.is_direct_download_running() {
            self.t("Direct download")
        } else {
            self.t("Idle")
        };
        let stage = self
            .recheck_stage_label
            .clone()
            .unwrap_or_else(|| self.t("Unknown"));

        ui.separator();
        ui.label(RichText::new(self.t("Operation diagnostics")).strong());
        Grid::new("operation_diagnostics_summary_grid")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label(self.t("Logging"));
                ui.label(logger_health.detail);
                ui.end_row();

                ui.label(self.t("Active operation"));
                ui.label(active_operation);
                ui.end_row();

                ui.label(self.t("Current stage"));
                ui.label(stage);
                ui.end_row();

                ui.label(self.t("Recent warnings"));
                ui.label(warnings.to_string());
                ui.end_row();

                ui.label(self.t("Recent errors"));
                ui.label(errors.to_string());
                ui.end_row();

                ui.label(self.t("Progress events"));
                ui.label(self.progress_events.len().to_string());
                ui.end_row();

                if let Some(completion) = self.completed_repository_check_banner.as_ref() {
                    ui.label(self.t("Last repository check"));
                    ui.label(if completion.success {
                        self.t("Finished")
                    } else {
                        self.t("Failed")
                    });
                    ui.end_row();
                }
            });
    }

    fn render_memory_diagnostics_contents(&mut self, ui: &mut Ui) {
        let report = self.build_memory_diagnostics_report();
        let now = Instant::now();
        let comparison_sample = self
            .memory_diagnostics_pinned_baseline
            .as_ref()
            .or_else(|| self.latest_sync_start_memory_sample())
            .cloned();

        ScrollArea::vertical()
            .id_salt("memory_diagnostics_outer_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width((ui.available_width() - 12.0).max(0.0));

                ui.label(
                    RichText::new(self.t(
                        "Current process memory and estimated Foxy-owned allocations. Values are approximate and exclude some allocator, driver, and OS caching overhead.",
                    ))
                    .color(self.color_text_dim()),
                );
                ui.add_space(8.0);
                self.render_operation_diagnostics_summary(ui);

                ui.horizontal(|ui| {
                    let snapshot_button = ui.button(self.t("Capture snapshot"));
                    if snapshot_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if snapshot_button.clicked() {
                        self.capture_memory_diagnostics_snapshot("manual snapshot", true);
                    }

                    let pin_baseline_button = ui.button(self.t("Pin current as baseline"));
                    if pin_baseline_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if pin_baseline_button.clicked() {
                        self.capture_memory_diagnostics_snapshot("manual baseline", true);
                        self.memory_diagnostics_pinned_baseline =
                            self.memory_diagnostics_history.back().cloned();
                    }

                    let refresh_map_button = ui.button(self.t("Refresh allocation map"));
                    if refresh_map_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if refresh_map_button.clicked() {
                        self.refresh_memory_diagnostics_process_map(true);
                    }

                    if self.memory_diagnostics_pinned_baseline.is_some() {
                        let clear_baseline_button = ui.button(self.t("Clear pinned baseline"));
                        if clear_baseline_button.hovered() {
                            ui.ctx()
                                .output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if clear_baseline_button.clicked() {
                            self.memory_diagnostics_pinned_baseline = None;
                        }
                    }
                });

                ui.separator();
                ui.label(RichText::new(self.t("At a glance")).strong());
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(0.0, 8.0);
                    Self::render_memory_metric_card(
                        self,
                        ui,
                        self.t("Task Manager memory"),
                        Self::format_optional_bytes(report.process.task_manager_memory_bytes()),
                        Some(self.t("Best match for the Windows Processes tab memory column")),
                        Some(&self.t("Total physical memory shown by Windows Task Manager. Normal: 100-400 MB.")),
                    );
                    Self::render_memory_metric_card(
                        self,
                        ui,
                        self.t("Working set"),
                        Self::format_optional_bytes(report.process.working_set_bytes),
                        Some(self.t("Resident physical memory currently mapped")),
                        Some(&self.t("Total physical memory used by Foxy. Normal: 100-400 MB.")),
                    );
                    Self::render_memory_metric_card(
                        self,
                        ui,
                        self.t("Commit / private bytes"),
                        Self::format_optional_bytes(report.process.private_bytes),
                        Some(self.t("Committed private memory, usually higher than Task Manager")),
                        Some(&self.t("Memory exclusively owned by Foxy (not shared). Normal: 50-300 MB.")),
                    );
                    Self::render_memory_metric_card(
                        self,
                        ui,
                        self.t("Tracked total"),
                        Self::format_bytes_short(report.tracked_total_bytes as u64),
                        Some(self.t("Estimated Foxy-owned caches and state tracked by this panel")),
                        Some(&self.t("Sum of all Foxy-tracked categories: caches, textures, database, and in-memory state.")),
                    );
                });

                ui.separator();
                ui.label(RichText::new(self.t("Process memory")).strong());
                Grid::new("memory_diagnostics_process_grid")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(self.t("Task Manager memory"));
                        ui.label(Self::format_optional_bytes(
                            report.process.task_manager_memory_bytes(),
                        ));
                        ui.end_row();

                        ui.label(self.t("Private working set"));
                        ui.label(Self::format_optional_bytes(
                            report.process.private_working_set_bytes,
                        ));
                        ui.end_row();

                        ui.label(self.t("Working set"));
                        ui.label(Self::format_optional_bytes(
                            report.process.working_set_bytes,
                        ));
                        ui.end_row();

                        ui.label(self.t("Commit / private bytes"));
                        ui.label(Self::format_optional_bytes(report.process.private_bytes));
                        ui.end_row();

                        ui.label(self.t("Peak working set"));
                        ui.label(Self::format_optional_bytes(
                            report.process.peak_working_set_bytes,
                        ));
                        ui.end_row();

                        ui.label(self.t("Virtual bytes"));
                        ui.label(Self::format_optional_bytes(report.process.virtual_bytes));
                        ui.end_row();

                        ui.label(self.t("Tracked total"));
                        ui.label(Self::format_bytes_short(report.tracked_total_bytes as u64));
                        ui.end_row();

                        ui.label(self.t("Untracked / external"));
                        let untracked = report
                            .untracked_bytes
                            .map(|bytes| {
                                if bytes >= 0 {
                                    Self::format_bytes_short(bytes as u64)
                                } else {
                                    format!("-{}", Self::format_bytes_short(bytes.unsigned_abs()))
                                }
                            })
                            .unwrap_or_else(|| "n/a".to_string());
                        ui.label(untracked);
                        ui.end_row();

                        ui.label(self.t("System available"));
                        let system_available = match (
                            report.process.available_system_bytes,
                            report.process.total_system_bytes,
                        ) {
                            (Some(available), Some(total)) => format!(
                                "{} / {}",
                                Self::format_bytes_short(available),
                                Self::format_bytes_short(total)
                            ),
                            (available, _) => Self::format_optional_bytes(available),
                        };
                        ui.label(system_available);
                        ui.end_row();
                    });

                if let Some(baseline) = comparison_sample.as_ref() {
                    ui.separator();
                    ui.label(
                        RichText::new(
                            self.t_fmt(
                                "Compared to {label}",
                                &[("label", baseline.label.clone())],
                            ),
                        )
                        .strong(),
                    );
                    Grid::new("memory_diagnostics_comparison_grid")
                        .num_columns(3)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new(self.t("Metric")).strong());
                            ui.label(RichText::new(self.t("Current")).strong());
                            ui.label(RichText::new(self.t("Delta")).strong());
                            ui.end_row();

                            Self::render_memory_comparison_row(
                                self,
                                ui,
                                self.t("Task Manager memory"),
                                Self::format_optional_bytes(report.process.task_manager_memory_bytes()),
                                Self::format_optional_bytes_delta(
                                    report.process.task_manager_memory_bytes(),
                                    baseline.process.task_manager_memory_bytes(),
                                ),
                                Some(&self.t("Total physical memory shown by Windows Task Manager. Normal: 100-400 MB.")),
                            );
                            Self::render_memory_comparison_row(
                                self,
                                ui,
                                self.t("Private working set"),
                                Self::format_optional_bytes(report.process.private_working_set_bytes),
                                Self::format_optional_bytes_delta(
                                    report.process.private_working_set_bytes,
                                    baseline.process.private_working_set_bytes,
                                ),
                                Some(&self.t("Private resident pages currently in physical memory.")),
                            );
                            Self::render_memory_comparison_row(
                                self,
                                ui,
                                self.t("Shared working set"),
                                Self::format_optional_bytes(report.process.shared_working_set_bytes()),
                                Self::format_optional_bytes_delta(
                                    report.process.shared_working_set_bytes(),
                                    baseline.process.shared_working_set_bytes(),
                                ),
                                Some(&self.t("Memory shared with other processes via DLLs or mapped files.")),
                            );
                            Self::render_memory_comparison_row(
                                self,
                                ui,
                                self.t("Commit / private bytes"),
                                Self::format_optional_bytes(report.process.private_bytes),
                                Self::format_optional_bytes_delta(
                                    report.process.private_bytes,
                                    baseline.process.private_bytes,
                                ),
                                Some(&self.t("Memory exclusively owned by Foxy (not shared). Normal: 50-300 MB.")),
                            );
                            Self::render_memory_comparison_row(
                                self,
                                ui,
                                self.t("Tracked total"),
                                Self::format_bytes_short(report.tracked_total_bytes as u64),
                                Self::format_bytes_delta(
                                    report.tracked_total_bytes as i64
                                        - baseline.tracked_total_bytes as i64,
                                ),
                                Some(&self.t("Sum of all Foxy-tracked categories: caches, textures, database, and in-memory state.")),
                            );
                            Self::render_memory_comparison_row(
                                self,
                                ui,
                                self.t("Untracked / external"),
                                report
                                    .untracked_bytes
                                    .map(|bytes| {
                                        if bytes >= 0 {
                                            Self::format_bytes_short(bytes as u64)
                                        } else {
                                            format!(
                                                "-{}",
                                                Self::format_bytes_short(bytes.unsigned_abs())
                                            )
                                        }
                                    })
                                    .unwrap_or_else(|| "n/a".to_string()),
                                match (report.untracked_bytes, baseline.untracked_bytes) {
                                    (Some(current), Some(previous)) => {
                                        Self::format_bytes_delta(current - previous)
                                    }
                                    _ => "n/a".to_string(),
                                },
                                Some(&self.t("Memory not accounted for by tracked categories. Includes stack, code, and external libraries.")),
                            );
                        });
                }

                ui.separator();
                ui.label(RichText::new(self.t("OS memory breakdown")).strong());
                Grid::new("memory_diagnostics_os_grid")
                    .num_columns(3)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new(self.t("Metric")).strong());
                        ui.label(RichText::new(self.t("Current")).strong());
                        ui.label(RichText::new(self.t("Notes")).strong());
                        ui.end_row();

                        ui.label(self.t("Private working set"));
                        ui.label(Self::format_optional_bytes(
                            report.process.private_working_set_bytes,
                        ));
                        ui.label(self.t("Private resident pages currently counted by Windows"));
                        ui.end_row();

                        ui.label(self.t("Shared working set"));
                        ui.label(Self::format_optional_bytes(
                            report.process.shared_working_set_bytes(),
                        ));
                        ui.label(self.t("Resident pages shared with code, DLLs, or mapped data"));
                        ui.end_row();

                        ui.label(self.t("Commit / private bytes"));
                        ui.label(Self::format_optional_bytes(report.process.private_bytes));
                        ui.label(self.t("Private committed memory, including pages not currently resident"));
                        ui.end_row();

                        ui.label(self.t("Shared commit"));
                        ui.label(Self::format_optional_bytes(report.process.shared_commit_bytes));
                        ui.label(self.t("Committed memory backed by shared sections"));
                        ui.end_row();

                        ui.label(self.t("Paged pool"));
                        ui.label(Self::format_optional_bytes(report.process.paged_pool_bytes));
                        ui.label(self.t("Kernel pool that can be paged out"));
                        ui.end_row();

                        ui.label(self.t("Non-paged pool"));
                        ui.label(Self::format_optional_bytes(report.process.non_paged_pool_bytes));
                        ui.label(self.t("Kernel pool that must stay resident"));
                        ui.end_row();

                        ui.label(self.t("Page faults"));
                        ui.label(Self::format_optional_count(report.process.page_fault_count));
                        ui.label(self.t("Lifetime fault count, useful as pressure context"));
                        ui.end_row();
                    });

                ui.separator();
                ui.label(RichText::new(self.t("OS allocation map")).strong());
                ui.label(
                    RichText::new(
                        self.t(
                            "This shows where Windows says Foxy mapped memory, not exact Rust types or functions.",
                        ),
                    )
                    .color(self.color_text_dim()),
                );
                if let Some(os_map) = report.os_virtual_memory_map.as_ref() {
                    if let Some(dominant_bucket) = os_map
                        .buckets
                        .iter()
                        .max_by_key(|bucket| bucket.committed_bytes)
                        .filter(|bucket| bucket.committed_bytes > 0)
                    {
                        ui.label(
                            RichText::new(
                                self.t_fmt(
                                    "Largest committed bucket: {label} ({size}).",
                                    &[
                                        ("label", dominant_bucket.label.clone()),
                                        (
                                            "size",
                                            Self::format_bytes_short(
                                                dominant_bucket.committed_bytes,
                                            ),
                                        ),
                                    ],
                                ),
                            )
                            .color(self.color_text_dim()),
                        );

                        let dominant_note = match dominant_bucket.label.as_str() {
                            "Private anonymous" => Some(self.t("Most untracked memory is currently in private anonymous pages. That usually means allocator heaps, thread stacks, SQLite temp memory, decompression buffers, or other runtime-private pages.")),
                            "Mapped sections" => Some(self.t("Most untracked memory is currently in mapped sections. That points more to memory-mapped files or shared sections than to Foxy-owned heap structures.")),
                            "Image / DLL" => Some(self.t("Most untracked memory is currently in image or DLL pages. That points more to executable code or loaded libraries than to Foxy-owned heap structures.")),
                            _ => None,
                        };
                        if let Some(dominant_note) = dominant_note {
                            ui.label(
                                RichText::new(dominant_note).color(self.color_text_dim()),
                            );
                        }
                    }

                    Grid::new("memory_diagnostics_os_map_grid")
                        .num_columns(5)
                        .striped(true)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new(self.t("Category")).strong());
                            ui.label(RichText::new(self.t("Committed")).strong());
                            ui.label(RichText::new(self.t("Reserved")).strong());
                            ui.label(RichText::new(self.t("Regions")).strong());
                            ui.label(RichText::new(self.t("Notes")).strong());
                            ui.end_row();

                            for bucket in &os_map.buckets {
                                ui.label(bucket.label.as_str());
                                ui.label(Self::format_bytes_short(bucket.committed_bytes));
                                ui.label(Self::format_bytes_short(bucket.reserved_bytes));
                                ui.label(bucket.region_count.to_string());
                                ui.label(bucket.note.as_str());
                                ui.end_row();
                            }
                        });

                    ui.add_space(8.0);
                    ui.label(RichText::new(self.t("Largest private regions")).strong());
                    if os_map.top_private_regions.is_empty() {
                        ui.label(
                            RichText::new(self.t("No committed private regions were captured."))
                                .color(self.color_text_dim()),
                        );
                    } else {
                        Grid::new("memory_diagnostics_os_map_regions_grid")
                            .num_columns(5)
                            .striped(true)
                            .spacing([16.0, 6.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new(self.t("Base address")).strong());
                                ui.label(RichText::new(self.t("Estimated size")).strong());
                                ui.label(RichText::new(self.t("Protection")).strong());
                                ui.label(RichText::new(self.t("Likely owner")).strong());
                                ui.label(RichText::new(self.t("Notes")).strong());
                                ui.end_row();

                                for region in &os_map.top_private_regions {
                                    ui.monospace(format!("0x{:X}", region.base_address));
                                    ui.label(Self::format_bytes_short(region.size_bytes));
                                    ui.label(region.protection.as_str());
                                    ui.label(region.usage.as_str());
                                    ui.label(region.note.as_str());
                                    ui.end_row();
                                }
                            });
                    }
                } else {
                    ui.label(
                        RichText::new(self.t("OS allocation map is currently unavailable."))
                            .color(self.color_text_dim()),
                    );
                }

                ui.separator();
                ui.label(RichText::new(self.t("Tracked memory map")).strong());
                Grid::new("memory_diagnostics_bucket_grid")
                    .num_columns(if comparison_sample.is_some() { 4 } else { 3 })
                    .striped(true)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new(self.t("Category")).strong());
                        ui.label(RichText::new(self.t("Estimated size")).strong());
                        if comparison_sample.is_some() {
                            ui.label(RichText::new(self.t("Delta")).strong());
                        }
                        ui.label(RichText::new(self.t("Notes")).strong());
                        ui.end_row();

                        for bucket in &report.buckets {
                            ui.label(bucket.label.as_str());
                            ui.label(Self::format_bytes_short(bucket.bytes as u64));
                            if let Some(baseline) = comparison_sample.as_ref() {
                                let baseline_bytes = baseline
                                    .buckets
                                    .iter()
                                    .find(|candidate| candidate.label == bucket.label)
                                    .map(|candidate| candidate.bytes)
                                    .unwrap_or_default();
                                ui.label(Self::format_bytes_delta(
                                    bucket.bytes as i64 - baseline_bytes as i64,
                                ));
                            }
                            ui.label(bucket.detail.as_str());
                            ui.end_row();
                        }
                    });

                ui.separator();
                ui.label(RichText::new(self.t("Recent memory samples")).strong());
                if self.memory_diagnostics_history.is_empty() {
                    ui.label(
                        RichText::new(self.t("No memory samples captured yet."))
                            .color(self.color_text_dim()),
                    );
                } else {
                    Grid::new("memory_diagnostics_samples_grid")
                        .num_columns(6)
                        .striped(true)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new(self.t("Age")).strong());
                            ui.label(RichText::new(self.t("Label")).strong());
                            ui.label(RichText::new(self.t("Task Manager memory")).strong());
                            ui.label(RichText::new(self.t("Working set")).strong());
                            ui.label(RichText::new(self.t("Commit / private bytes")).strong());
                            ui.label(RichText::new(self.t("Tracked total")).strong());
                            ui.end_row();

                            for sample in self.memory_diagnostics_history.iter().rev().take(24) {
                                ui.label(format!(
                                    "{:.1}s",
                                    now.duration_since(sample.captured_at).as_secs_f32()
                                ));
                                ui.label(sample.label.as_str());
                                ui.label(Self::format_optional_bytes(
                                    sample.task_manager_memory_bytes,
                                ));
                                ui.label(Self::format_optional_bytes(sample.working_set_bytes));
                                ui.label(Self::format_optional_bytes(sample.private_bytes));
                                ui.label(Self::format_bytes_short(
                                    sample.tracked_total_bytes as u64,
                                ));
                                ui.end_row();
                            }
                        });
                }

                ui.separator();
                CollapsingHeader::new(self.t("egui memory internals"))
                    .default_open(false)
                    .show(ui, |ui| {
                        let ctx = ui.ctx().clone();
                        ctx.memory_ui(ui);
                    });
            });
    }

    pub fn render_memory_diagnostics_window(&mut self, ctx: &egui::Context) {
        const MEMORY_DIAGNOSTICS_VIEWPORT_ID: &str = "memory_diagnostics_viewport";

        let viewport_id = ViewportId::from_hash_of(MEMORY_DIAGNOSTICS_VIEWPORT_ID);
        let builder = ViewportBuilder::default()
            .with_title(self.t("Memory diagnostics"))
            .with_inner_size([940.0, 720.0])
            .with_min_inner_size([820.0, 520.0])
            .with_resizable(true);
        let mut close_requested = false;

        ctx.show_viewport_immediate(viewport_id, builder, |viewport_ctx, viewport_class| {
            if viewport_ctx.input(|i| i.viewport().close_requested()) {
                close_requested = true;
            }

            match viewport_class {
                ViewportClass::EmbeddedWindow | ViewportClass::Root => {
                    let mut open = self.show_memory_diagnostics_window;
                    egui::Window::new(self.t("Memory diagnostics"))
                        .open(&mut open)
                        .default_size(Vec2::new(940.0, 720.0))
                        .min_width(820.0)
                        .frame(
                            Frame::window(&viewport_ctx.global_style())
                                .fill(self.color_main_bg())
                                .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                                .corner_radius(egui::CornerRadius::same(10)),
                        )
                        .show(viewport_ctx, |ui| {
                            Frame::NONE.inner_margin(Margin::same(12)).show(ui, |ui| {
                                self.refresh_memory_diagnostics_process_map(false);
                                self.render_memory_diagnostics_contents(ui);
                            });
                        });
                    self.show_memory_diagnostics_window = open;
                }
                ViewportClass::Immediate | ViewportClass::Deferred => {
                    let mut viewport_ui = egui::Ui::new(
                        viewport_ctx.clone(),
                        Id::new("memory_diagnostics_immediate_panel"),
                        egui::UiBuilder::new().max_rect(viewport_ctx.content_rect()),
                    );
                    CentralPanel::default()
                        .frame(
                            Frame::NONE
                                .fill(self.color_main_bg())
                                .inner_margin(Margin::same(12)),
                        )
                        .show(&mut viewport_ui, |ui| {
                            self.refresh_memory_diagnostics_process_map(false);
                            self.render_memory_diagnostics_contents(ui);
                        });
                }
            }
        });

        if close_requested {
            self.show_memory_diagnostics_window = false;
        }
    }
}
