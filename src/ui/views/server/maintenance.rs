use std::time::{Duration, Instant};

use chrono::{Local, TimeZone, Utc};
use eframe::egui::{self, Response, RichText, Ui};

use crate::core::schedule::{JobSchedule, ScheduleClock};
use crate::core::server::ServerInstanceId;
use crate::core::server::jobs::{
    JobStatus, MaintenanceAction, MaintenanceChange, MaintenanceJob, Occurrence, OccurrenceState,
    OccurrenceTrigger, PlayerPolicy,
};
use crate::ui::app::Foxy;
use crate::ui::app::server::{
    MaintenanceConfirmation, MaintenanceForm, MaintenanceNotice, ServerTaskAction,
};
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::types::{ScheduleKind, WEEKDAYS_ALL};

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const WEEKDAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn local_text(unix_ms: u64) -> String {
    crate::core::schedule::unix_ms_to_local(unix_ms)
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| tr("unknown time"))
}

fn clock_text(clock: ScheduleClock) -> String {
    tr(match clock {
        ScheduleClock::Local => "local time",
        ScheduleClock::Utc => "UTC",
    })
}

fn schedule_text(job: &MaintenanceJob) -> String {
    let definition = &job.definition;
    match definition.schedule {
        JobSchedule::OnceAt { unix_ms } => {
            let when = i64::try_from(unix_ms)
                .ok()
                .and_then(|millis| match definition.clock {
                    ScheduleClock::Utc => Utc
                        .timestamp_millis_opt(millis)
                        .single()
                        .map(|time| time.format("%Y-%m-%d %H:%M").to_string()),
                    ScheduleClock::Local => Local
                        .timestamp_millis_opt(millis)
                        .single()
                        .map(|time| time.format("%Y-%m-%d %H:%M").to_string()),
                })
                .unwrap_or_else(|| tr("unknown time"));
            tr_fmt(
                "Once at {when} ({clock})",
                &[("when", when), ("clock", clock_text(definition.clock))],
            )
        }
        JobSchedule::Recurring {
            minutes_of_day,
            weekdays,
        } => {
            let days = if weekdays == WEEKDAYS_ALL {
                tr("every day")
            } else {
                (0..7)
                    .filter(|day| weekdays & (1u8 << day) != 0)
                    .map(|day| tr(WEEKDAY_LABELS[day]))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            tr_fmt(
                "{days} at {time} ({clock})",
                &[
                    ("days", days),
                    (
                        "time",
                        format!("{:02}:{:02}", minutes_of_day / 60, minutes_of_day % 60),
                    ),
                    ("clock", clock_text(definition.clock)),
                ],
            )
        }
    }
}

fn action_text(action: MaintenanceAction) -> String {
    tr(match action {
        MaintenanceAction::Restart => "Restart the server",
        MaintenanceAction::RotateMission => "Rotate mission",
        MaintenanceAction::ReloadScenario => "Reload Reforger scenario",
        MaintenanceAction::Update => "Update the installation",
        MaintenanceAction::Backup => "Back up the server",
    })
}

fn state_text(state: OccurrenceState) -> String {
    tr(match state {
        OccurrenceState::Claimed => "Recorded",
        OccurrenceState::Deferred => "Deferred",
        OccurrenceState::Running => "Running",
        OccurrenceState::RetryWait => "Waiting to retry",
        OccurrenceState::Succeeded => "Succeeded",
        OccurrenceState::Failed => "Failed",
        OccurrenceState::Skipped => "Skipped",
        OccurrenceState::Blocked => "Blocked",
        OccurrenceState::Cancelled => "Cancelled",
        OccurrenceState::Unknown => "Unknown",
    })
}

fn occurrence_detail(occurrence: &Occurrence) -> Option<String> {
    let reason = occurrence.reason.map(|reason| tr(reason.describe()));
    let message = occurrence
        .message
        .as_ref()
        .filter(|message| {
            occurrence
                .reason
                .is_none_or(|reason| reason.describe() != message.as_str())
        })
        .cloned();
    match (reason, message) {
        (Some(reason), Some(message)) => Some(format!("{reason} {message}")),
        (reason, message) => reason.or(message),
    }
}

fn trigger_text(occurrence: &Occurrence) -> String {
    match &occurrence.trigger {
        OccurrenceTrigger::Scheduled { scheduled_unix_ms } => tr_fmt(
            "Scheduled for {when}",
            &[("when", local_text(*scheduled_unix_ms))],
        ),
        OccurrenceTrigger::Manual {
            requested_unix_ms, ..
        } => tr_fmt(
            "Requested {when}",
            &[("when", local_text(*requested_unix_ms))],
        ),
    }
}

impl Foxy {
    /// Returns the section's focus anchor for restoring focus after a maintenance task.
    pub(in crate::ui::views) fn render_server_maintenance(&mut self, ui: &mut Ui) -> Option<Response> {
        let capabilities = crate::core::game::registry().active().server_capabilities();
        if !capabilities.lifecycle {
            return None;
        }
        let id = self.server_view.selected.clone()?;
        let busy = self.server_view.controls_busy();
        let refresh_due = self
            .server_view
            .maintenance
            .last_refresh
            .is_none_or(|last| last.elapsed() >= REFRESH_INTERVAL);
        if refresh_due && !self.server_view.busy() {
            self.server_view.maintenance.last_refresh = Some(Instant::now());
            self.start_server_task(ServerTaskAction::LoadMaintenance(id.clone()));
        }
        ui.ctx().request_repaint_after(REFRESH_INTERVAL);

        ui.separator();
        ui.heading(tr("Scheduled maintenance"));
        ui.label(tr(
            "Maintenance jobs run only while a server owner for this game space is running. A scheduled restart never starts a stopped server, and an occurrence whose owner ended is reported as unknown and never repeated automatically.",
        ));
        if let Some(error) = &self.server_view.maintenance.load_error {
            ui.colored_label(
                self.color_text_error(),
                tr_fmt(
                    "Maintenance jobs could not be read: {reason}",
                    &[("reason", tr(error))],
                ),
            );
        }
        let Some(snapshot) = self.server_view.maintenance.for_instance(&id).cloned() else {
            if self.server_view.maintenance.load_error.is_none() {
                ui.horizontal(|ui| {
                    ui.add(crate::ui::app::PacedSpinner::new());
                    ui.label(tr("Loading maintenance jobs..."));
                });
            }
            return None;
        };
        self.clear_stale_maintenance_confirmation(&snapshot.overview);
        let overview = &snapshot.overview;
        ui.label(tr(if overview.owner_running {
            "A server owner is running and executes due jobs."
        } else {
            "No server owner is running, so no job runs until one starts."
        }));
        if overview.unreadable_occurrences > 0 {
            ui.colored_label(
                self.color_text_error(),
                tr_fmt(
                    "{count} unreadable occurrence records were left untouched.",
                    &[("count", overview.unreadable_occurrences.to_string())],
                ),
            );
        }
        if overview.history_truncated {
            ui.label(tr("Only part of the occurrence records were read."));
        }
        let mut action = None;
        let anchor = ui
            .horizontal_wrapped(|ui| {
                let add = ui.add_enabled(
                    !busy
                        && self.server_view.maintenance.form.is_none()
                        && overview.jobs.len() < crate::core::server::jobs::MAX_JOBS_PER_INSTANCE,
                    egui::Button::new(tr("Add maintenance job")),
                );
                Self::focus_outline(ui, &add);
                if add.clicked() {
                    let mut form = MaintenanceForm::new_add(Local::now());
                    form.base_revision = overview.catalog_revision;
                    self.server_view.maintenance.form = Some(form);
                    self.server_view.maintenance.focus_form = true;
                    self.server_view.maintenance.confirmation = None;
                    self.server_view.maintenance.notice = None;
                    self.server_view.maintenance.action_error = None;
                }
                let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh maintenance")));
                Self::focus_outline(ui, &refresh);
                if refresh.clicked() {
                    self.server_view.maintenance.last_refresh = Some(Instant::now());
                    self.server_view.maintenance.notice = None;
                    self.server_view.maintenance.action_error = None;
                    action = Some(ServerTaskAction::LoadMaintenance(id.clone()));
                }
                add
            })
            .inner;
        if let Some(notice) = self.server_view.maintenance.notice {
            ui.label(tr(match notice {
                MaintenanceNotice::Saved => {
                    "Maintenance job saved. A running owner picks up the change within a few seconds."
                }
                MaintenanceNotice::Removed => {
                    "Maintenance job removed. Its occurrence history is kept."
                }
                MaintenanceNotice::Recorded => "Occurrence recorded. The owner runs it shortly.",
                MaintenanceNotice::AlreadyOpen => {
                    "This job already has an open occurrence, so no second one was recorded."
                }
                MaintenanceNotice::Cancelled => "Cancellation recorded.",
                MaintenanceNotice::CancelRequested => {
                    "Cancellation requested. A restart in progress stops at its next safe point."
                }
                MaintenanceNotice::AlreadyFinished => {
                    "The occurrence had already finished, so nothing was cancelled."
                }
            }));
        }
        if let Some(error) = &self.server_view.maintenance.action_error {
            ui.colored_label(self.color_text_error(), tr(error));
        }
        if let Some(form_action) = self.render_maintenance_form(ui, &id, busy) {
            action = Some(form_action);
        }
        if overview.jobs.is_empty() {
            ui.label(tr("No maintenance jobs for this instance."));
        }
        for status in &overview.jobs {
            if let Some(job_action) =
                self.render_maintenance_job(ui, &id, status, overview, busy, &capabilities)
            {
                action = Some(job_action);
            }
        }
        if !snapshot.recent.is_empty() {
            egui::CollapsingHeader::new(tr_fmt(
                "Recent occurrences ({count})",
                &[("count", snapshot.recent.len().to_string())],
            ))
            .id_salt(("server-maintenance-history", id.as_str()))
            .show(ui, |ui| {
                for occurrence in &snapshot.recent {
                    let name = overview
                        .jobs
                        .iter()
                        .find(|status| status.job.id == occurrence.job_id)
                        .map(|status| status.job.definition.name.clone())
                        .unwrap_or_else(|| tr("Removed job"));
                    ui.group(|ui| {
                        ui.label(
                            RichText::new(tr_fmt(
                                "{job}: {state}",
                                &[("job", name), ("state", state_text(occurrence.state))],
                            ))
                            .strong(),
                        );
                        ui.label(tr_fmt(
                            "{trigger}; attempts {attempts}; policy revision {revision}",
                            &[
                                ("trigger", trigger_text(occurrence)),
                                ("attempts", occurrence.attempts.to_string()),
                                ("revision", occurrence.policy_revision.to_string()),
                            ],
                        ));
                        if let Some(detail) = occurrence_detail(occurrence) {
                            ui.label(detail);
                        }
                        if let Some(backup_id) = &occurrence.backup_id {
                            ui.label(tr_fmt(
                                "Verified backup: {id} ({mode})",
                                &[
                                    ("id", backup_id.clone()),
                                    (
                                        "mode",
                                        occurrence
                                            .backup_consistency
                                            .unwrap_or_default()
                                            .as_str()
                                            .to_owned(),
                                    ),
                                ],
                            ));
                        }
                        ui.label(
                            RichText::new(occurrence.occurrence_id.as_str())
                                .color(self.color_text_dim()),
                        );
                    });
                }
            });
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        Some(anchor)
    }

    fn render_maintenance_job(
        &mut self,
        ui: &mut Ui,
        id: &ServerInstanceId,
        status: &JobStatus,
        overview: &crate::core::server::jobs::MaintenanceOverview,
        busy: bool,
        capabilities: &crate::core::server::ServerCapabilities,
    ) -> Option<ServerTaskAction> {
        let job = &status.job;
        let definition = &job.definition;
        let policy = &definition.policy;
        let mut action = None;
        ui.push_id(("server-maintenance-job", job.id.as_str()), |ui| {
            ui.group(|ui| {
                ui.label(
                    RichText::new(tr_fmt(
                        "{name}: {action}",
                        &[
                            ("name", definition.name.clone()),
                            ("action", action_text(definition.action)),
                        ],
                    ))
                    .strong(),
                );
                ui.label(schedule_text(job));
                ui.label(tr_fmt(
                    if definition.enabled {
                        "Enabled; policy revision {revision}"
                    } else {
                        "Disabled; policy revision {revision}"
                    },
                    &[("revision", job.revision.to_string())],
                ));
                if capabilities.arma_administration
                    && let Some(arma) = &policy.arma
                {
                    if let Some(announcement) = &arma.announcement {
                        ui.label(tr_fmt(
                            "Arma maintenance announcement: {message} (wait {seconds} seconds)",
                            &[
                                ("message", announcement.clone()),
                                (
                                    "seconds",
                                    arma.announcement_delay_seconds.to_string(),
                                ),
                            ],
                        ));
                    }
                    if let Some(template) = &arma.mission_template {
                        ui.label(tr_fmt("Mission rotation template: {template}", &[(
                            "template",
                            template.clone(),
                        )]));
                    }
                    if arma.require_backup {
                        ui.label(tr(match policy.backup_mode {
                            crate::core::server::BackupConsistencyMode::StoppedVerified => {
                                "Requires a witnessed stopped-consistent backup before running."
                            }
                            crate::core::server::BackupConsistencyMode::BestEffortLive => {
                                "Requires a best-effort live backup before running."
                            }
                        }));
                    }
                }
                ui.label(tr_fmt(
                    "Players: {players}; missed-run grace {grace} min; deferral limit {defer} min; up to {attempts} attempts {retry} s apart; stop timeout {timeout} s",
                    &[
                        (
                            "players",
                            tr(match policy.player_policy {
                                PlayerPolicy::Ignore => "restart even with players connected",
                                PlayerPolicy::RequireEmpty => "only when the server is empty",
                            }),
                        ),
                        ("grace", policy.grace_minutes.to_string()),
                        ("defer", policy.max_defer_minutes.to_string()),
                        ("attempts", policy.max_attempts.to_string()),
                        ("retry", policy.retry_delay_seconds.to_string()),
                        ("timeout", policy.stop_timeout_seconds.to_string()),
                    ],
                ));
                if let Some(next) = status.next_run_unix_ms {
                    ui.label(tr_fmt("Next run: {when}", &[("when", local_text(next))]));
                }
                if let Some(last) = &status.last {
                    ui.label(tr_fmt(
                        "Last result: {state} ({trigger})",
                        &[
                            ("state", state_text(last.state)),
                            ("trigger", trigger_text(last)),
                        ],
                    ));
                    if last.state.terminal()
                        && let Some(detail) = occurrence_detail(last)
                    {
                        ui.label(detail);
                    }
                }
                if let Some(blocker) = &status.blocker {
                    ui.colored_label(
                        self.color_text_error(),
                        tr_fmt("Not running as scheduled: {reason}", &[("reason", tr(blocker))]),
                    );
                }
                let open = status.open.clone();
                if let Some(open) = &open {
                    ui.label(tr_fmt(
                        "In progress: {state}, attempt {attempts}",
                        &[
                            ("state", state_text(open.state)),
                            ("attempts", open.attempts.to_string()),
                        ],
                    ));
                }
                let can_run = definition.action.unavailable_reason(capabilities).is_none()
                    && overview.owner_running;
                let confirming = self.server_view.maintenance.confirmation.clone();
                ui.horizontal_wrapped(|ui| {
                    let run = ui.add_enabled(
                        !busy && can_run && open.is_none() && confirming.is_none(),
                        egui::Button::new(tr("Run now")),
                    );
                    let run = if !overview.owner_running {
                        run.on_disabled_hover_text(tr("Run now needs a running server owner."))
                    } else {
                        run
                    };
                    Self::focus_outline(ui, &run);
                    if run.clicked() {
                        self.open_maintenance_confirmation(MaintenanceConfirmation::Run {
                            job_id: job.id.clone(),
                            revision: job.revision,
                        });
                    }
                    let edit = ui.add_enabled(
                        !busy && self.server_view.maintenance.form.is_none(),
                        egui::Button::new(tr("Edit job")),
                    );
                    Self::focus_outline(ui, &edit);
                    if edit.clicked() {
                        let mut form = MaintenanceForm::from_job(job, Local::now());
                        form.base_revision = overview.catalog_revision;
                        self.server_view.maintenance.form = Some(form);
                        self.server_view.maintenance.focus_form = true;
                        self.server_view.maintenance.confirmation = None;
                        self.server_view.maintenance.notice = None;
                        self.server_view.maintenance.action_error = None;
                    }
                    let remove = ui.add_enabled(
                        !busy && confirming.is_none(),
                        egui::Button::new(tr("Remove job")),
                    );
                    Self::focus_outline(ui, &remove);
                    if remove.clicked() {
                        self.open_maintenance_confirmation(MaintenanceConfirmation::Remove {
                            job_id: job.id.clone(),
                            catalog_revision: overview.catalog_revision,
                        });
                    }
                    if let Some(open) = &open {
                        let cancel = ui.add_enabled(
                            !busy && overview.owner_running && confirming.is_none(),
                            egui::Button::new(tr("Cancel occurrence")),
                        );
                        Self::focus_outline(ui, &cancel);
                        if cancel.clicked() {
                            self.open_maintenance_confirmation(MaintenanceConfirmation::Cancel {
                                occurrence_id: open.occurrence_id.clone(),
                            });
                        }
                    }
                });
                let mine = match &confirming {
                    Some(MaintenanceConfirmation::Run { job_id, .. })
                    | Some(MaintenanceConfirmation::Remove { job_id, .. }) => job_id == &job.id,
                    Some(MaintenanceConfirmation::Cancel { occurrence_id }) => open
                        .as_ref()
                        .is_some_and(|open| &open.occurrence_id == occurrence_id),
                    None => false,
                };
                if mine && let Some(confirmation) = confirming {
                    action = self.render_maintenance_confirmation(ui, id, confirmation, busy);
                }
            });
        });
        action
    }

    fn open_maintenance_confirmation(&mut self, confirmation: MaintenanceConfirmation) {
        self.server_view.maintenance.confirmation = Some(confirmation);
        self.server_view.maintenance.focus_confirmation = true;
        self.server_view.maintenance.notice = None;
        self.server_view.maintenance.action_error = None;
    }

    /// A confirmation whose job or occurrence disappeared is no longer drawn, so it must not keep
    /// disabling the other controls.
    fn clear_stale_maintenance_confirmation(
        &mut self,
        overview: &crate::core::server::jobs::MaintenanceOverview,
    ) {
        let present = match &self.server_view.maintenance.confirmation {
            Some(
                MaintenanceConfirmation::Run { job_id, .. }
                | MaintenanceConfirmation::Remove { job_id, .. },
            ) => overview.jobs.iter().any(|status| &status.job.id == job_id),
            Some(MaintenanceConfirmation::Cancel { occurrence_id }) => {
                overview.jobs.iter().any(|status| {
                    status
                        .open
                        .as_ref()
                        .is_some_and(|open| &open.occurrence_id == occurrence_id)
                })
            }
            None => true,
        };
        if !present {
            self.server_view.maintenance.confirmation = None;
            self.server_view.maintenance.focus_confirmation = false;
        }
    }

    fn render_maintenance_confirmation(
        &mut self,
        ui: &mut Ui,
        id: &ServerInstanceId,
        confirmation: MaintenanceConfirmation,
        busy: bool,
    ) -> Option<ServerTaskAction> {
        let (question, confirm, keep) = match &confirmation {
            MaintenanceConfirmation::Run { .. } => (
                "Run this job now? A restart disconnects connected players unless the job only runs when the server is empty.",
                "Run job now",
                "Cancel",
            ),
            MaintenanceConfirmation::Remove { .. } => (
                "Remove this job? A restart that already stopped the server still starts it again, other open occurrences are cancelled, and recorded history is kept.",
                "Remove maintenance job",
                "Cancel",
            ),
            MaintenanceConfirmation::Cancel { .. } => (
                "Cancel this occurrence? A restart that already stopped the server can leave it stopped.",
                "Cancel the occurrence",
                "Keep the occurrence",
            ),
        };
        ui.label(tr(question));
        let mut action = None;
        ui.horizontal(|ui| {
            let apply = ui.add_enabled(!busy, egui::Button::new(tr(confirm)));
            Self::focus_outline(ui, &apply);
            let safe = ui.button(tr(keep));
            if self.server_view.maintenance.focus_confirmation {
                safe.request_focus();
                safe.scroll_to_me(Some(egui::Align::Center));
                self.server_view.maintenance.focus_confirmation = false;
            }
            Self::focus_outline(ui, &safe);
            if safe.clicked()
                || (!busy
                    && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
            {
                self.server_view.maintenance.confirmation = None;
                self.server_view.pending_focus = Some("server-maintenance-controls");
            }
            if apply.clicked() {
                action = Some(match confirmation {
                    MaintenanceConfirmation::Run { job_id, revision } => {
                        ServerTaskAction::RunMaintenanceJob {
                            id: id.clone(),
                            job_id,
                            expected_policy_revision: revision,
                        }
                    }
                    MaintenanceConfirmation::Remove {
                        job_id,
                        catalog_revision,
                    } => ServerTaskAction::ChangeMaintenance {
                        id: id.clone(),
                        expected_revision: catalog_revision,
                        change: MaintenanceChange::Remove { id: job_id },
                    },
                    MaintenanceConfirmation::Cancel { occurrence_id } => {
                        ServerTaskAction::CancelMaintenanceOccurrence {
                            id: id.clone(),
                            occurrence_id,
                        }
                    }
                });
            }
        });
        action
    }

    fn render_maintenance_form(
        &mut self,
        ui: &mut Ui,
        id: &ServerInstanceId,
        busy: bool,
    ) -> Option<ServerTaskAction> {
        let mut form = self.server_view.maintenance.form.take()?;
        let capabilities = crate::core::game::registry().active().server_capabilities();
        let mut save = false;
        let mut cancel = false;
        ui.group(|ui| {
            ui.label(
                RichText::new(tr(if form.editing.is_some() {
                    "Edit maintenance job"
                } else {
                    "New maintenance job"
                }))
                .strong(),
            );
            ui.horizontal(|ui| {
                let caption = ui.label(tr("Name:"));
                let name = ui
                    .add(
                        egui::TextEdit::singleline(&mut form.name)
                            .id(egui::Id::unique(("server-maintenance-name", id.as_str())))
                            .char_limit(48)
                            .desired_width(240.0),
                    )
                    .labelled_by(caption.id);
                if self.server_view.maintenance.focus_form {
                    name.request_focus();
                    name.scroll_to_me(Some(egui::Align::Center));
                    self.server_view.maintenance.focus_form = false;
                }
            });
            ui.label(tr("Action:"));
            ui.horizontal_wrapped(|ui| {
                let mut choices = vec![
                    MaintenanceAction::Restart,
                    MaintenanceAction::Update,
                    MaintenanceAction::Backup,
                ];
                if capabilities.arma_administration {
                    choices.push(MaintenanceAction::RotateMission);
                }
                if capabilities.reforger_administration {
                    choices.push(MaintenanceAction::ReloadScenario);
                }
                for choice in choices {
                    let reason = choice.unavailable_reason(&capabilities);
                    let response = ui.add_enabled(
                        reason.is_none(),
                        egui::RadioButton::new(form.action == choice, action_text(choice)),
                    );
                    if let Some(reason) = reason {
                        response.on_disabled_hover_text(tr(reason));
                    } else if response.clicked() {
                        form.action = choice;
                    }
                }
            });
            if let Some(reason) = form.action.unavailable_reason(&capabilities) {
                ui.label(tr(reason));
            }
            if form.action == MaintenanceAction::RotateMission {
                ui.label(tr(
                    "Mission rotation requires Arma RCon and a mission template already deployed to the server.",
                ));
            }
            if form.action == MaintenanceAction::ReloadScenario {
                ui.label(tr("This sends the Reforger scenario reload command. Use the lifecycle Restart control to restart the server process."));
            }
            ui.label(RichText::new(tr("When")).strong());
            ui.horizontal(|ui| {
                ui.radio_value(&mut form.kind, ScheduleKind::Once, tr("Once"));
                ui.radio_value(&mut form.kind, ScheduleKind::Recurring, tr("Recurring"));
            });
            match form.kind {
                ScheduleKind::Once => {
                    ui.horizontal(|ui| {
                        ui.label(tr("Date:"));
                        ui.add(egui::DragValue::new(&mut form.year).range(2024..=2100))
                            .accessible_name(tr("Year"));
                        ui.label("-");
                        ui.add(egui::DragValue::new(&mut form.month).range(1..=12))
                            .accessible_name(tr("Month"));
                        ui.label("-");
                        ui.add(egui::DragValue::new(&mut form.day).range(1..=31))
                            .accessible_name(tr("Day"));
                        ui.add_space(12.0);
                        ui.label(tr("Time:"));
                        ui.add(egui::DragValue::new(&mut form.hour).range(0..=23))
                            .accessible_name(tr("Hour"));
                        ui.label(":");
                        ui.add(egui::DragValue::new(&mut form.minute).range(0..=59))
                            .accessible_name(tr("Minute"));
                    });
                }
                ScheduleKind::Recurring => {
                    ui.horizontal(|ui| {
                        ui.label(tr("Time:"));
                        ui.add(egui::DragValue::new(&mut form.hour).range(0..=23))
                            .accessible_name(tr("Hour"));
                        ui.label(":");
                        ui.add(egui::DragValue::new(&mut form.minute).range(0..=59))
                            .accessible_name(tr("Minute"));
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr("Days:"));
                        for (index, label) in WEEKDAY_LABELS.iter().enumerate() {
                            let bit = 1u8 << index;
                            let mut on = form.weekdays & bit != 0;
                            if ui.checkbox(&mut on, tr(label)).changed() {
                                form.weekdays = if on {
                                    form.weekdays | bit
                                } else {
                                    form.weekdays & !bit
                                };
                            }
                        }
                    });
                }
            }
            ui.checkbox(&mut form.utc, tr("Use UTC instead of local time"));
            ui.checkbox(&mut form.enabled, tr("Enabled"));
            ui.checkbox(
                &mut form.require_empty,
                tr("Only restart when no players are connected"),
            );
            egui::Grid::new(("server-maintenance-policy", id.as_str()))
                .num_columns(2)
                .show(ui, |ui| {
                    let grace_label = ui.label(tr("Missed-run grace (minutes)"));
                    ui.add(egui::DragValue::new(&mut form.grace_minutes).range(0..=1440))
                        .labelled_by(grace_label.id);
                    ui.end_row();
                    let defer_label = ui.label(tr("Deferral limit (minutes)"));
                    ui.add(egui::DragValue::new(&mut form.max_defer_minutes).range(0..=720))
                        .labelled_by(defer_label.id);
                    ui.end_row();
                    let attempts_label = ui.label(tr("Attempts"));
                    ui.add(egui::DragValue::new(&mut form.max_attempts).range(1..=5))
                        .labelled_by(attempts_label.id);
                    ui.end_row();
                    let retry_label = ui.label(tr("Retry delay (seconds)"));
                    ui.add(egui::DragValue::new(&mut form.retry_delay_seconds).range(10..=3600))
                        .labelled_by(retry_label.id);
                    ui.end_row();
                    let stop_label = ui.label(tr("Stop timeout (seconds)"));
                    ui.add(egui::DragValue::new(&mut form.stop_timeout_seconds).range(5..=120))
                        .labelled_by(stop_label.id);
                    ui.end_row();
                });
            if capabilities.arma_administration {
                ui.separator();
                ui.label(RichText::new(tr("Arma maintenance options")).strong());
                if matches!(form.action, MaintenanceAction::Restart | MaintenanceAction::RotateMission) {
                    ui.checkbox(
                        &mut form.arma_hooks,
                        tr("Announce before Arma maintenance"),
                    );
                    if form.arma_hooks {
                        ui.label(tr(
                            "When a message is entered and RCon is configured, send it before restarting the server or rotating the mission.",
                        ));
                        let announce_label =
                            ui.label(tr("Announcement (optional, up to 256 bytes)"));
                        ui.add(
                            egui::TextEdit::multiline(&mut form.announcement)
                                .desired_rows(2)
                                .char_limit(256),
                        )
                        .labelled_by(announce_label.id);
                        ui.horizontal(|ui| {
                            let delay_label = ui.label(tr("Announcement delay (seconds)"));
                            ui.add(
                                egui::DragValue::new(&mut form.announcement_delay_seconds)
                                    .range(0..=300),
                            )
                            .labelled_by(delay_label.id);
                        });
                    }
                }
                if form.action == MaintenanceAction::RotateMission {
                    let template_label = ui.label(tr("Mission template"));
                    ui.add(
                        egui::TextEdit::singleline(&mut form.mission_template)
                            .char_limit(256)
                            .desired_width(320.0),
                    )
                    .labelled_by(template_label.id);
                }
                ui.checkbox(
                    &mut form.require_backup,
                    tr("Require a verified backup before running"),
                );
                if form.require_backup {
                    if form.action == MaintenanceAction::RotateMission {
                        ui.label(tr(
                            "Mission rotation runs on a live server and requires enabling best-effort live backup mode below.",
                        ));
                    } else {
                        ui.label(tr(
                            "A stopped-consistent backup will be created after stopping the server before starting it again.",
                        ));
                    }
                }
            }
            if form.action == MaintenanceAction::Backup
                || (capabilities.arma_administration && form.require_backup)
            {
                ui.checkbox(
                    &mut form.live_backup,
                    tr("Allow best-effort live backup when the server is running"),
                );
            }
            if let Some(error) = &form.error {
                ui.colored_label(self.color_text_error(), tr(error));
            }
            ui.horizontal(|ui| {
                let save_button = ui.add_enabled(!busy, egui::Button::new(tr("Save job")));
                Self::focus_outline(ui, &save_button);
                save = save_button.clicked();
                let cancel_button = ui.button(tr("Cancel"));
                Self::focus_outline(ui, &cancel_button);
                cancel = cancel_button.clicked()
                    || (!busy
                        && ui.input_mut(|i| {
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                        }));
            });
        });
        if cancel {
            self.server_view.pending_focus = Some("server-maintenance-controls");
            return None;
        }
        let mut action = None;
        if save {
            match form.build(crate::core::server::jobs::unix_ms()) {
                Ok(definition) => {
                    form.error = None;
                    action = Some(ServerTaskAction::ChangeMaintenance {
                        id: id.clone(),
                        expected_revision: form.base_revision,
                        change: match &form.editing {
                            Some(job_id) => MaintenanceChange::Set {
                                id: job_id.clone(),
                                definition,
                            },
                            None => MaintenanceChange::Add(definition),
                        },
                    });
                }
                Err(error) => form.error = Some(error),
            }
        }
        self.server_view.maintenance.form = Some(form);
        action
    }
}
