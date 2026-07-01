//! Settings -> Scheduling tab: list, create, edit, and run scheduled jobs.
//!
//! Jobs themselves execute in `src/ui/app/scheduling/`; this module is purely
//! the editor and list UI. The cancellable post-action countdown is a global
//! overlay rendered from the update loop, not here.

use crate::ui::app::Foxy;
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::types::{
    JobRunOutcome, JobSchedule, JobTarget, PostAction, RepoInstanceKey, ScheduleJobDraft,
    ScheduleKind, ScheduledJob, TargetKind, unix_ms_to_local,
};
use chrono::Local;
use eframe::egui::{self, Button, RichText, ScrollArea, Ui, Vec2};
use log::info;

/// Weekday bit labels in Monday..Sunday order to match the recurring bitset.
const WEEKDAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// A deferred action collected while iterating the job list, applied afterwards
/// so the list is not mutated mid-borrow.
enum JobListAction {
    Edit(usize),
    Delete(usize),
    ToggleEnabled(usize),
    RunNow(usize),
}

impl Foxy {
    pub(super) fn render_scheduling_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!(
                    "{} {}",
                    '\u{2139}',
                    tr("Scheduled jobs recheck, download, and optionally close Foxy or shut down the PC at a chosen time. They only run while Foxy is open.")
                );
                ui.label(RichText::new(info_text).italics().color(self.color_text_dim()));
            });
            ui.separator();

            self.render_scheduling_active_status(ui, horizontal_padding);

            if self.scheduling_editor.is_some() {
                self.render_scheduling_editor(ui, horizontal_padding);
            } else {
                self.render_scheduling_job_list(ui, horizontal_padding);
            }
        });
    }

    /// Status row shown while a scheduled job is executing or a post-action is
    /// counting down.
    fn render_scheduling_active_status(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        if let Some(run) = self.scheduler_active_run.as_ref() {
            let text = tr_fmt(
                "Running '{name}' - {stage}...",
                &[
                    ("name", run.job_name.clone()),
                    ("stage", tr(run.stage_label())),
                ],
            );
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(RichText::new(text).color(self.color_action_info()));
            });
            ui.separator();
        } else if let Some(pending) = self.scheduler_pending_post_action.as_ref() {
            let text = tr_fmt(
                "Job '{name}' finished - waiting to run its post-action.",
                &[("name", pending.job_name.clone())],
            );
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(RichText::new(text).color(self.color_warn()));
            });
            ui.separator();
        }
    }

    fn render_scheduling_job_list(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let button_width = ui.available_width() - 2.0 * horizontal_padding;
            let add_button = ui.add_sized(
                Vec2::new(button_width, 30.0),
                Button::new(tr("Add scheduled job")).fill(self.color_widget_bg()),
            );
            if add_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if add_button.clicked() {
                self.scheduling_editor = Some(self.new_scheduling_draft());
            }
            ui.add_space(horizontal_padding);
        });
        ui.separator();

        if self.settings_view_state.scheduled_jobs.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                ui.label(RichText::new(tr("No scheduled jobs yet.")).color(self.color_text_dim()));
            });
            return;
        }

        let mut action: Option<JobListAction> = None;
        ScrollArea::vertical().show(ui, |ui| {
            let job_count = self.settings_view_state.scheduled_jobs.len();
            for i in 0..job_count {
                let job = self.settings_view_state.scheduled_jobs[i].clone();
                if let Some(act) = self.render_scheduling_job_card(ui, i, &job, horizontal_padding)
                {
                    action = Some(act);
                }
                ui.add_space(8.0);
            }
        });

        match action {
            Some(JobListAction::Edit(i)) => {
                if let Some(job) = self.settings_view_state.scheduled_jobs.get(i) {
                    self.scheduling_editor = Some(ScheduleJobDraft::from_job(job));
                }
            }
            Some(JobListAction::Delete(i)) => {
                if i < self.settings_view_state.scheduled_jobs.len() {
                    let removed = self.settings_view_state.scheduled_jobs.remove(i);
                    info!("Removed scheduled job '{}'", removed.name);
                    self.save_settings();
                }
            }
            Some(JobListAction::ToggleEnabled(i)) => {
                if let Some(job) = self.settings_view_state.scheduled_jobs.get_mut(i) {
                    job.enabled = !job.enabled;
                    info!(
                        "Scheduled job '{}' {}",
                        job.name,
                        if job.enabled { "enabled" } else { "disabled" }
                    );
                    self.save_settings();
                }
            }
            Some(JobListAction::RunNow(i)) => {
                self.run_scheduled_job_now(i);
            }
            None => {}
        }
    }

    fn render_scheduling_job_card(
        &mut self,
        ui: &mut Ui,
        index: usize,
        job: &ScheduledJob,
        horizontal_padding: f32,
    ) -> Option<JobListAction> {
        let mut action = None;
        let busy =
            self.scheduler_active_run.is_some() || self.scheduler_pending_post_action.is_some();

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let card_frame = egui::Frame {
                fill: self.color_card_bg(),
                stroke: egui::Stroke::new(1.0, self.color_text_gray()),
                corner_radius: egui::CornerRadius::same(5),
                inner_margin: egui::Margin::same(8),
                ..Default::default()
            };
            let card_width = ui.available_width() - 2.0 * horizontal_padding;
            card_frame.show(ui, |ui| {
                ui.set_width(card_width);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(job.name.as_str())
                                .strong()
                                .color(self.color_text_normal()),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let delete_button =
                                ui.add(Button::new("X").fill(self.color_text_error()));
                            if delete_button.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if delete_button.clicked() {
                                action = Some(JobListAction::Delete(index));
                            }

                            let edit_button = ui.button(tr("Edit"));
                            if edit_button.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if edit_button.clicked() {
                                action = Some(JobListAction::Edit(index));
                            }

                            let run_button = ui.add_enabled(!busy, Button::new(tr("Run now")));
                            if run_button.hovered() && !busy {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if run_button.clicked() {
                                action = Some(JobListAction::RunNow(index));
                            }

                            let mut enabled = job.enabled;
                            if ui.checkbox(&mut enabled, tr("Enabled")).changed() {
                                action = Some(JobListAction::ToggleEnabled(index));
                            }
                        });
                    });

                    ui.label(
                        RichText::new(self.scheduling_schedule_text(job))
                            .color(self.color_text_dim()),
                    );
                    ui.label(
                        RichText::new(tr_fmt(
                            "Targets: {targets}",
                            &[("targets", self.scheduling_target_text(&job.target))],
                        ))
                        .color(self.color_text_dim()),
                    );
                    ui.label(
                        RichText::new(self.scheduling_stages_text(job))
                            .color(self.color_text_dim()),
                    );
                    if let Some(result) = job.last_result.as_ref() {
                        ui.label(
                            RichText::new(self.scheduling_last_result_text(result))
                                .italics()
                                .color(self.scheduling_outcome_color(result.outcome)),
                        );
                    }
                });
            });
            ui.add_space(horizontal_padding);
        });

        action
    }

    fn new_scheduling_draft(&self) -> ScheduleJobDraft {
        let mut draft = ScheduleJobDraft::new_add(Local::now());
        // Prefill a sensible target so a brand-new job is valid after picking a
        // time: first space if any exist, else the first repository.
        if let Some(space) = self.repository_spaces.first() {
            draft.target_kind = TargetKind::Space;
            draft.target_space_id = Some(space.id.clone());
        } else if let Some(repo) = self.repository_view_state.repositories.first() {
            draft.target_kind = TargetKind::Repository;
            draft.target_repo = Some(RepoInstanceKey {
                remote_url: repo.address.clone(),
                local_path: repo.path.clone(),
            });
        }
        draft
    }

    fn render_scheduling_editor(&mut self, ui: &mut Ui, horizontal_padding: f32) {
        let Some(mut draft) = self.scheduling_editor.take() else {
            return;
        };
        let title = if draft.editing_id.is_some() {
            tr("Edit scheduled job")
        } else {
            tr("New scheduled job")
        };

        let mut save = false;
        let mut cancel = false;

        ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading(RichText::new(title).color(self.color_text_normal()));
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label(tr("Name:"));
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .hint_text(tr("Scheduled job"))
                        .desired_width((ui.available_width() - horizontal_padding).max(160.0)),
                );
            });
            ui.add_space(6.0);

            // When section.
            ui.label(
                RichText::new(tr("When"))
                    .strong()
                    .color(self.color_text_normal()),
            );
            ui.horizontal(|ui| {
                ui.radio_value(&mut draft.kind, ScheduleKind::Once, tr("Once"));
                ui.radio_value(&mut draft.kind, ScheduleKind::Recurring, tr("Recurring"));
            });
            match draft.kind {
                ScheduleKind::Once => {
                    ui.horizontal(|ui| {
                        ui.label(tr("Date:"));
                        ui.add(egui::DragValue::new(&mut draft.year).range(2024..=2100));
                        ui.label("-");
                        ui.add(egui::DragValue::new(&mut draft.month).range(1..=12));
                        ui.label("-");
                        ui.add(egui::DragValue::new(&mut draft.day).range(1..=31));
                        ui.add_space(12.0);
                        ui.label(tr("Time:"));
                        ui.add(egui::DragValue::new(&mut draft.hour).range(0..=23));
                        ui.label(":");
                        ui.add(egui::DragValue::new(&mut draft.minute).range(0..=59));
                    });
                }
                ScheduleKind::Recurring => {
                    ui.horizontal(|ui| {
                        ui.label(tr("Time:"));
                        ui.add(egui::DragValue::new(&mut draft.hour).range(0..=23));
                        ui.label(":");
                        ui.add(egui::DragValue::new(&mut draft.minute).range(0..=59));
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr("Days:"));
                        for (i, label) in WEEKDAY_LABELS.iter().enumerate() {
                            let bit = 1u8 << i;
                            let mut on = draft.weekdays & bit != 0;
                            if ui.checkbox(&mut on, tr(label)).changed() {
                                if on {
                                    draft.weekdays |= bit;
                                } else {
                                    draft.weekdays &= !bit;
                                }
                            }
                        }
                    });
                }
            }
            ui.add_space(6.0);

            // Targets section.
            ui.label(
                RichText::new(tr("Targets"))
                    .strong()
                    .color(self.color_text_normal()),
            );
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut draft.target_kind,
                    TargetKind::Repository,
                    tr("Single repository"),
                );
                ui.radio_value(
                    &mut draft.target_kind,
                    TargetKind::Space,
                    tr("Repository space"),
                );
                ui.radio_value(
                    &mut draft.target_kind,
                    TargetKind::Custom,
                    tr("Custom selection"),
                );
            });
            match draft.target_kind {
                TargetKind::Repository => self.render_scheduling_repo_picker(ui, &mut draft),
                TargetKind::Space => self.render_scheduling_space_picker(ui, &mut draft),
                TargetKind::Custom => self.render_scheduling_custom_picker(ui, &mut draft),
            }
            ui.add_space(6.0);

            // Stages section.
            ui.label(
                RichText::new(tr("Actions"))
                    .strong()
                    .color(self.color_text_normal()),
            );
            ui.checkbox(&mut draft.recheck, tr("Recheck for updates"));
            ui.checkbox(&mut draft.download, tr("Download updates (auto-approve)"));

            ui.add_space(4.0);
            ui.label(tr("After finishing:"));
            ui.radio_value(&mut draft.post_action, PostAction::None, tr("Do nothing"));
            ui.radio_value(
                &mut draft.post_action,
                PostAction::CloseApp,
                tr("Close Foxy"),
            );
            if cfg!(target_os = "windows") {
                let shutdown_radio = ui.radio_value(
                    &mut draft.post_action,
                    PostAction::ShutdownPc,
                    tr("Shut down PC"),
                );
                shutdown_radio.on_hover_text(tr(
                    "A 60-second cancellable countdown is shown before shutting down.",
                ));
            } else if draft.post_action == PostAction::ShutdownPc {
                draft.post_action = PostAction::None;
            }
            ui.add_enabled(
                draft.post_action != PostAction::None,
                egui::Checkbox::new(
                    &mut draft.post_action_only_on_success,
                    tr("Only run the post-action if everything succeeded"),
                ),
            );

            ui.add_space(10.0);
            if let Some(error) = draft.error.as_ref() {
                ui.colored_label(self.color_text_error(), tr(error));
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                let save_button = ui.add(Button::new(tr("Save")).fill(self.color_primary_accent()));
                if save_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if save_button.clicked() {
                    save = true;
                }
                let cancel_button = ui.button(tr("Cancel"));
                if cancel_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if cancel_button.clicked() {
                    cancel = true;
                }
            });
        });

        if cancel {
            self.scheduling_editor = None;
            return;
        }

        if save {
            let id = draft
                .editing_id
                .clone()
                .unwrap_or_else(|| format!("job-{}", Local::now().timestamp_millis()));
            match draft.build(id, Local::now()) {
                Ok(job) => {
                    self.upsert_scheduled_job(job);
                    self.scheduling_editor = None;
                }
                Err(error) => {
                    draft.error = Some(error);
                    self.scheduling_editor = Some(draft);
                }
            }
        } else {
            self.scheduling_editor = Some(draft);
        }
    }

    fn upsert_scheduled_job(&mut self, mut job: ScheduledJob) {
        if let Some(slot) = self
            .settings_view_state
            .scheduled_jobs
            .iter_mut()
            .find(|existing| existing.id == job.id)
        {
            // Preserve the enabled toggle across an edit; reset run history since
            // the schedule may have changed.
            job.enabled = slot.enabled;
            info!("Updated scheduled job '{}'", job.name);
            *slot = job;
        } else {
            info!("Created scheduled job '{}'", job.name);
            self.settings_view_state.scheduled_jobs.push(job);
        }
        self.save_settings();
    }

    fn render_scheduling_repo_picker(&self, ui: &mut Ui, draft: &mut ScheduleJobDraft) {
        let selected_text = draft
            .target_repo
            .as_ref()
            .and_then(|key| self.scheduling_repo_label(key))
            .unwrap_or_else(|| tr("Choose a repository"));
        egui::ComboBox::from_id_salt("scheduling_repo_picker")
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for repo in &self.repository_view_state.repositories {
                    let key = RepoInstanceKey {
                        remote_url: repo.address.clone(),
                        local_path: repo.path.clone(),
                    };
                    let selected = draft.target_repo.as_ref() == Some(&key);
                    if ui.selectable_label(selected, repo.name.as_str()).clicked() {
                        draft.target_repo = Some(key);
                    }
                }
            });
    }

    fn render_scheduling_space_picker(&self, ui: &mut Ui, draft: &mut ScheduleJobDraft) {
        let selected_text = draft
            .target_space_id
            .as_ref()
            .and_then(|id| {
                self.repository_spaces
                    .iter()
                    .find(|space| &space.id == id)
                    .map(|space| Self::repository_space_display_name(space).to_string())
            })
            .unwrap_or_else(|| tr("Choose a repository space"));
        egui::ComboBox::from_id_salt("scheduling_space_picker")
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for space in &self.repository_spaces {
                    let selected = draft.target_space_id.as_deref() == Some(space.id.as_str());
                    let label = Self::repository_space_display_name(space).to_string();
                    if ui.selectable_label(selected, label).clicked() {
                        draft.target_space_id = Some(space.id.clone());
                    }
                }
            });
    }

    fn render_scheduling_custom_picker(&self, ui: &mut Ui, draft: &mut ScheduleJobDraft) {
        ui.label(
            RichText::new(tr_fmt(
                "{count} selected",
                &[("count", draft.target_custom.len().to_string())],
            ))
            .color(self.color_text_dim()),
        );
        ScrollArea::vertical()
            .max_height(160.0)
            .id_salt("scheduling_custom_picker")
            .show(ui, |ui| {
                for repo in &self.repository_view_state.repositories {
                    let key = RepoInstanceKey {
                        remote_url: repo.address.clone(),
                        local_path: repo.path.clone(),
                    };
                    let mut on = draft.target_custom.contains(&key);
                    if ui.checkbox(&mut on, repo.name.as_str()).changed() {
                        if on {
                            draft.target_custom.push(key);
                        } else {
                            draft.target_custom.retain(|existing| existing != &key);
                        }
                    }
                }
            });
    }

    fn scheduling_repo_label(&self, key: &RepoInstanceKey) -> Option<String> {
        self.repository_view_state
            .repositories
            .iter()
            .find(|repo| {
                Self::normalize_repo_url(&repo.address) == Self::normalize_repo_url(&key.remote_url)
                    && Self::repo_instance_path_key(&repo.path)
                        == Self::repo_instance_path_key(&key.local_path)
            })
            .map(|repo| repo.name.clone())
    }

    fn scheduling_schedule_text(&self, job: &ScheduledJob) -> String {
        match &job.schedule {
            JobSchedule::OnceAt { .. } => {
                if let Some(next) = job.schedule.next_fire_at(Local::now()) {
                    tr_fmt(
                        "Once at {when}",
                        &[("when", next.format("%Y-%m-%d %H:%M").to_string())],
                    )
                } else if matches!(
                    job.last_result.as_ref().map(|r| r.outcome),
                    Some(JobRunOutcome::Missed)
                ) {
                    tr("One-time job - missed")
                } else {
                    tr("One-time job - completed")
                }
            }
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } => {
                let time = format!("{:02}:{:02}", minutes_of_day / 60, minutes_of_day % 60);
                let days = self.scheduling_weekdays_text(*weekdays);
                tr_fmt("{days} at {time}", &[("days", days), ("time", time)])
            }
        }
    }

    fn scheduling_weekdays_text(&self, weekdays: u8) -> String {
        if weekdays == crate::ui::types::WEEKDAYS_ALL {
            return tr("every day");
        }
        let names: Vec<String> = (0..7)
            .filter(|i| weekdays & (1u8 << i) != 0)
            .map(|i| tr(WEEKDAY_LABELS[i]))
            .collect();
        if names.is_empty() {
            tr("no days")
        } else {
            names.join(", ")
        }
    }

    fn scheduling_target_text(&self, target: &JobTarget) -> String {
        match target {
            JobTarget::Repository(key) => self
                .scheduling_repo_label(key)
                .unwrap_or_else(|| tr("Unknown repository")),
            JobTarget::Space { space_id } => self
                .repository_spaces
                .iter()
                .find(|space| &space.id == space_id)
                .map(|space| Self::repository_space_display_name(space).to_string())
                .unwrap_or_else(|| tr("Unknown space")),
            JobTarget::Custom { repos } => tr_fmt(
                "{count} repositories",
                &[("count", repos.len().to_string())],
            ),
        }
    }

    fn scheduling_stages_text(&self, job: &ScheduledJob) -> String {
        let mut stages: Vec<String> = Vec::new();
        if job.recheck {
            stages.push(tr("Recheck"));
        }
        if job.download {
            stages.push(tr("Download"));
        }
        let post = match job.post_action {
            PostAction::None => None,
            PostAction::CloseApp => Some(tr("Close Foxy")),
            PostAction::ShutdownPc => Some(tr("Shut down PC")),
        };
        let mut text = if stages.is_empty() {
            tr("No operations")
        } else {
            stages.join(" + ")
        };
        if let Some(post) = post {
            text = tr_fmt("{stages} then {post}", &[("stages", text), ("post", post)]);
        }
        text
    }

    fn scheduling_last_result_text(&self, result: &crate::ui::types::JobRunResult) -> String {
        let when = unix_ms_to_local(result.finished_unix_ms)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let outcome = match result.outcome {
            JobRunOutcome::Success => tr("succeeded"),
            JobRunOutcome::PartialFailure => tr("partly failed"),
            JobRunOutcome::Failed => tr("failed"),
            JobRunOutcome::Skipped => tr("skipped"),
            JobRunOutcome::Missed => tr("missed"),
        };
        tr_fmt(
            "Last run {when}: {outcome}",
            &[("when", when), ("outcome", outcome)],
        )
    }

    fn scheduling_outcome_color(&self, outcome: JobRunOutcome) -> egui::Color32 {
        match outcome {
            JobRunOutcome::Success => self.color_success(),
            JobRunOutcome::PartialFailure | JobRunOutcome::Missed => self.color_warn(),
            JobRunOutcome::Failed => self.color_text_error(),
            JobRunOutcome::Skipped => self.color_text_dim(),
        }
    }
}
