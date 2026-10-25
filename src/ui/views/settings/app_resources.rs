use crate::core::utils::cpu_threads;
use crate::ui::app::Foxy;
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::types::{MemoryPressurePreference, StorageTypePreference, WorkPriorityPreference};
use eframe::egui::{self, RichText, Ui};

/// One labelled dropdown row; returns whether the choice changed.
fn render_choice_row<T: Copy + PartialEq>(
    ui: &mut Ui,
    horizontal_padding: f32,
    id_salt: &str,
    label: String,
    hover_text: String,
    value: &mut T,
    options: &[(T, String, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_space(horizontal_padding);
        let row_label = ui.label(label);
        let selected_text = options
            .iter()
            .find(|(option, _, _)| option == value)
            .map(|(_, text, _)| text.clone())
            .unwrap_or_default();
        let combo = egui::ComboBox::from_id_salt(id_salt)
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for (option, text, description) in options {
                    let response = ui
                        .selectable_value(value, *option, text.as_str())
                        .on_hover_text(description.as_str());
                    if response.changed() {
                        changed = true;
                    }
                }
            });
        let combo_response = combo
            .response
            .labelled_by(row_label.id)
            .on_hover_text(hover_text);
        if combo_response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        ui.add_space(horizontal_padding);
    });
    changed
}

impl Foxy {
    /// CPU threads, work and disk priority, storage type, memory pressure, and
    /// the progress frame rate. Every change applies to the running process.
    pub(super) fn render_application_settings_resources(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        changed: &mut bool,
    ) {
        let mut applied = self.render_cpu_threads_row(ui, horizontal_padding);
        ui.separator();

        if cfg!(windows) {
            applied |= render_choice_row(
                ui,
                horizontal_padding,
                "settings_work_priority",
                tr("Work priority"),
                tr(
                    "Processor priority of the threads that hash, scan, check and download. The interface and the programs Foxy launches keep normal priority.",
                ),
                &mut self.settings_view_state.work_priority,
                &[
                    (
                        WorkPriorityPreference::Normal,
                        tr("Normal"),
                        tr("Foxy's work competes with other programs as usual."),
                    ),
                    (
                        WorkPriorityPreference::BelowNormal,
                        tr("Below normal"),
                        tr(
                            "Programs at normal priority, such as a running game, get the processor first.",
                        ),
                    ),
                    (
                        WorkPriorityPreference::Lowest,
                        tr("Lowest"),
                        tr(
                            "Foxy's work runs only when the processor has nothing else to do. Checks can take much longer.",
                        ),
                    ),
                ],
            );
            ui.separator();

            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                let checkbox = Self::ui_state_checkbox(
                    ui,
                    &mut self.settings_view_state.low_disk_priority,
                    tr("Low disk priority"),
                )
                .on_hover_text(tr(
                    "Run Foxy's hashing, scanning and download threads in Windows background mode, which lowers their disk and memory priority so games and other programs load faster. Checks and downloads take longer while other programs use the disk.",
                ));
                if checkbox.changed() {
                    applied = true;
                }
                if checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(horizontal_padding);
            });
            ui.separator();
        }

        applied |= render_choice_row(
            ui,
            horizontal_padding,
            "settings_storage_type",
            tr("Storage type"),
            tr(
                "Foxy picks how many files to read at once from each drive's type. Override it when detection is wrong, for example a USB SSD detected as removable. Applies to every drive.",
            ),
            &mut self.settings_view_state.storage_type_override,
            &[
                (
                    StorageTypePreference::Auto,
                    tr("Auto-detect"),
                    tr("Detect each drive's type."),
                ),
                (
                    StorageTypePreference::Ssd,
                    tr("SSD"),
                    tr("Treat every drive as an SSD: many parallel reads."),
                ),
                (
                    StorageTypePreference::Hdd,
                    tr("HDD"),
                    tr(
                        "Treat every drive as a hard disk: few parallel reads, so the disk head does not jump between files.",
                    ),
                ),
                (
                    StorageTypePreference::Removable,
                    tr("Removable drive"),
                    tr("Treat every drive as a removable drive: few parallel reads."),
                ),
            ],
        );
        ui.separator();

        applied |= render_choice_row(
            ui,
            horizontal_padding,
            "settings_memory_pressure",
            tr("Memory pressure"),
            tr(
                "Foxy lowers hashing and download concurrency when memory runs low. Force a level when the measurement does not fit how you use this computer.",
            ),
            &mut self.settings_view_state.memory_pressure_override,
            &[
                (
                    MemoryPressurePreference::Auto,
                    tr("Auto"),
                    tr("Measure free memory before each check and download."),
                ),
                (
                    MemoryPressurePreference::Normal,
                    tr("Normal"),
                    tr("Plan as if plenty of memory is free."),
                ),
                (
                    MemoryPressurePreference::Constrained,
                    tr("Constrained"),
                    tr("Plan as if memory is limited: less parallel hashing and downloading."),
                ),
                (
                    MemoryPressurePreference::Severe,
                    tr("Severe"),
                    tr("Plan as if memory is very low: the least parallel work."),
                ),
            ],
        );
        ui.separator();

        let mut frame_rate_options: Vec<(Option<u32>, String, String)> = vec![
            (
                None,
                tr("Auto (30 fps)"),
                tr("Draw progress at 30 frames per second."),
            ),
            (
                Some(20),
                tr_fmt("{count} fps", &[("count", "20".to_string())]),
                tr("Slightly less smooth progress for a little less processor time."),
            ),
            (
                Some(10),
                tr_fmt("{count} fps", &[("count", "10".to_string())]),
                tr("Less smooth progress for less processor time."),
            ),
            (
                Some(5),
                tr_fmt("{count} fps", &[("count", "5".to_string())]),
                tr("The least processor time; progress updates in steps."),
            ),
        ];
        if let Some(rate) = self.settings_view_state.progress_frame_rate
            && !frame_rate_options
                .iter()
                .any(|(option, _, _)| *option == Some(rate))
        {
            frame_rate_options.push((
                Some(rate),
                tr_fmt("{count} fps", &[("count", rate.to_string())]),
                tr("Set in the settings file."),
            ));
        }
        if render_choice_row(
            ui,
            horizontal_padding,
            "settings_progress_frame_rate",
            tr("Progress frame rate"),
            tr(
                "How many frames per second Foxy draws while checks, downloads and other work show progress. Lower values use less processor time.",
            ),
            &mut self.settings_view_state.progress_frame_rate,
            &frame_rate_options,
        ) {
            crate::ui::app::set_progress_frame_rate(self.settings_view_state.progress_frame_rate);
            *changed = true;
        }

        if applied {
            crate::core::utils::runtime_settings::apply_runtime_settings(&self.settings_view_state);
            *changed = true;
        }
    }

    fn render_cpu_threads_row(&mut self, ui: &mut Ui, horizontal_padding: f32) -> bool {
        let mut cpu_changed = false;
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let threads_label = ui.label(tr("CPU Threads")).on_hover_text(tr(
                "Processor threads Foxy plans hashing, scanning and checking work around. Auto uses every thread the system reports.",
            ));

            let detected = cpu_threads::detected_cpu_threads();
            let mut auto = self.settings_view_state.cpu_thread_limit.is_none();
            let auto_checkbox = Self::ui_state_checkbox(ui, &mut auto, tr("Auto"))
                .on_hover_text(tr("Use every processor thread the system reports."));
            if auto_checkbox.changed() {
                self.settings_view_state.cpu_thread_limit = if auto {
                    None
                } else {
                    Some(cpu_threads::cpu_thread_limit_max(true, detected))
                };
                cpu_changed = true;
            }
            if auto_checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            if !auto {
                let safe_checkbox = Self::ui_state_checkbox(
                    ui,
                    &mut self.settings_view_state.cpu_thread_limit_safe,
                    tr("Safe limits"),
                )
                .on_hover_text(tr(
                    "Keep the thread count within the processor threads detected on this computer. Turn off to enter any value.",
                ));
                if safe_checkbox.changed() {
                    cpu_changed = true;
                }
                if safe_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }

                let max_threads = cpu_threads::cpu_thread_limit_max(
                    self.settings_view_state.cpu_thread_limit_safe,
                    detected,
                );
                let thread_limit = self.settings_view_state.cpu_thread_limit.get_or_insert(1);
                let clamped = (*thread_limit).clamp(1, max_threads);
                if clamped != *thread_limit {
                    *thread_limit = clamped;
                    cpu_changed = true;
                }
                let thread_input = ui
                    .add(egui::DragValue::new(thread_limit).range(1..=max_threads))
                    .labelled_by(threads_label.id);
                if thread_input.changed() {
                    cpu_changed = true;
                }
                if thread_input.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
            }

            if let Some(detected) = detected {
                ui.label(
                    RichText::new(tr_fmt(
                        "Detected: {count}",
                        &[("count", detected.to_string())],
                    ))
                    .color(self.color_text_dim()),
                );
            }
            ui.add_space(horizontal_padding);
        });
        cpu_changed
    }
}
