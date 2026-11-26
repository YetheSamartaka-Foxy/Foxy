use crate::core::server::{
    BackupConsistencyMode, BackupRetentionPolicy, MAX_BACKUP_KEEP_LAST, ServerInstanceId,
};
use crate::ui::app::Foxy;
use crate::ui::app::server::{ServerDialog, ServerTaskAction};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, RichText, Ui};

impl Foxy {
    pub(crate) fn render_server_backups(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .server_backups
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let dirty = editor.dirty();
        let busy = self.server_view.busy();
        let response = ui.push_id("server.backup-controls", |ui| {
            ui.heading(tr("Server backups and retention"));
            ui.weak(tr("Create bounded verified backups of owned engine references, redacted configuration, content metadata, persistence provider coverage and save files. Plaintext secrets, RCon passwords, owner tokens and transient caches are excluded."));

            if dirty {
                ui.label(tr("Save or discard server draft edits before planning or creating backups."));
            }

            let status_btn = ui.add_enabled(!busy, egui::Button::new(tr("Refresh restore status")));
            Self::focus_outline(ui, &status_btn);
            if status_btn.clicked() {
                self.start_server_task(ServerTaskAction::LoadServerRestoreStatus(id.clone()));
            }
            if let Some(status) = self.server_view.backups.restore_status.as_ref()
                && status.instance_id == id
            {
                if let Some(pending) = &status.pending {
                    ui.colored_label(self.color_text_error(), tr_fmt(
                        "Restore recovery is pending for backup {backup} (restore {restore}).",
                        &[("backup", pending.backup_id.clone()), ("restore", pending.restore_id.clone())],
                    ));
                    ui.label(tr_fmt("Recovery fingerprint: {fingerprint}", &[("fingerprint", pending.fingerprint.clone())]));
                    let rollback = ui.add_enabled(!busy, egui::Button::new(tr("Review restore rollback")));
                    Self::focus_outline(ui, &rollback);
                    if rollback.clicked() {
                        self.server_view.open_dialog(ServerDialog::RollbackServerRestore);
                    }
                } else {
                    ui.label(tr("No pending restore recovery is recorded."));
                }
            }

            let prev_live = self.server_view.backups.live_mode;
            let live_check = ui.add_enabled(
                !busy,
                egui::Checkbox::new(
                    &mut self.server_view.backups.live_mode,
                    tr("Allow best-effort live backup while the server is running (default requires witnessed stopped consistency)"),
                ),
            );
            Self::focus_outline(ui, &live_check);
            if prev_live != self.server_view.backups.live_mode {
                self.server_view.backups.plan = None;
            }
            let mode = if self.server_view.backups.live_mode {
                BackupConsistencyMode::BestEffortLive
            } else {
                BackupConsistencyMode::StoppedVerified
            };

            let plan_matches = self
                .server_view
                .backups
                .plan
                .as_ref()
                .filter(|plan| plan.instance_id == id && plan.consistency.mode == mode)
                .map(|plan| (plan.fingerprint.clone(), plan.blockers.is_empty()));

            let mut plan_anchor = None;
            ui.horizontal_wrapped(|ui| {
                let plan_btn = ui.add_enabled(
                    !busy && !dirty,
                    egui::Button::new(tr("Review backup plan")),
                );
                Self::focus_outline(ui, &plan_btn);
                if plan_btn.clicked() {
                    self.server_view.backups.plan = None;
                    self.server_view.backups.outcome = None;
                    self.start_server_task(ServerTaskAction::PlanServerBackup {
                        id: id.clone(),
                        mode,
                    });
                }
                plan_anchor = Some(plan_btn);

                let can_create = !busy
                    && !dirty
                    && plan_matches.as_ref().is_some_and(|(_, ready)| *ready);
                let create_btn = ui.add_enabled(
                    can_create,
                    egui::Button::new(tr("Create verified backup")),
                );
                Self::focus_outline(ui, &create_btn);
                if create_btn.clicked()
                    && let Some((fingerprint, true)) = plan_matches
                {
                    self.start_server_task(ServerTaskAction::CreateServerBackup {
                        id: id.clone(),
                        mode,
                        expected_plan_fingerprint: fingerprint,
                    });
                }

                let refresh_btn = ui.add_enabled(!busy, egui::Button::new(tr("Refresh backups")));
                Self::focus_outline(ui, &refresh_btn);
                if refresh_btn.clicked() {
                    self.start_server_task(ServerTaskAction::LoadServerBackups(id.clone()));
                }
            });

            if let Some(plan) = self
                .server_view
                .backups
                .plan
                .as_ref()
                .filter(|plan| plan.instance_id == id)
            {
                ui.group(|ui| {
                    ui.label(
                        RichText::new(tr_fmt(
                            "Backup plan {fingerprint}",
                            &[("fingerprint", plan.fingerprint.clone())],
                        ))
                        .strong(),
                    );
                    ui.label(tr_fmt(
                        "Consistency: {mode}; stopped verified: {stopped}; owner running: {owner}; active consumers: {consumers}; files: {files}; uncompressed bytes: {bytes}",
                        &[
                            ("mode", plan.consistency.mode.as_str().to_owned()),
                            ("stopped", super::chrome::yes_no(plan.consistency.stopped_verified)),
                            ("owner", plan.consistency.owner_running.to_string()),
                            ("consumers", plan.consistency.active_consumers.to_string()),
                            ("files", plan.planned_files.to_string()),
                            ("bytes", plan.planned_uncompressed_bytes.to_string()),
                        ],
                    ));
                    ui.label(tr_fmt(
                        "Covered providers: {covered}; uncovered external providers: {uncovered}",
                        &[
                            ("covered", plan.provider_set.covered_providers.join(", ")),
                            ("uncovered", plan.provider_set.uncovered_providers.join(", ")),
                        ],
                    ));
                    ui.label(tr_fmt(
                        "Saves captured: {files} file(s), {bytes} byte(s); secret references: {refs}; excluded secret paths: {secrets}",
                        &[
                            ("files", plan.save_set.save_file_count.to_string()),
                            ("bytes", plan.save_set.save_total_bytes.to_string()),
                            ("refs", plan.configuration_set.secret_reference_count.to_string()),
                            (
                                "secrets",
                                plan.configuration_set.excluded_secret_paths.join(", "),
                            ),
                        ],
                    ));
                    for note in &plan.consistency.notes {
                        ui.label(note);
                    }
                    for blocker in &plan.blockers {
                        ui.colored_label(self.color_text_error(), blocker);
                    }
                });
            }

            if let Some(outcome) = self
                .server_view
                .backups
                .outcome
                .as_ref()
                .filter(|outcome| outcome.instance_id == id)
            {
                ui.label(tr_fmt(
                    "Created verified backup {id} ({mode}, stopped verified: {stopped}, archive {bytes} bytes, retention pruned: {pruned})",
                    &[
                        ("id", outcome.manifest.backup_id.clone()),
                        (
                            "mode",
                            outcome.manifest.consistency.mode.as_str().to_owned(),
                        ),
                        (
                            "stopped",
                            super::chrome::yes_no(outcome.manifest.consistency.stopped_verified),
                        ),
                        ("bytes", outcome.archive_bytes.to_string()),
                        ("pruned", outcome.pruned_backups.len().to_string()),
                    ],
                ));
            }

            if let Some(listing) = self
                .server_view
                .backups
                .listing
                .as_ref()
                .filter(|listing| listing.instance_id == id)
                .cloned()
            {
                self.render_backup_listing(ui, &id, &listing, busy, dirty);
            }

            if let Some(report) = self
                .server_view
                .backups
                .verification
                .as_ref()
                .filter(|report| report.instance_id == id)
            {
                ui.label(tr_fmt(
                    "Verification for {id}: valid={valid}, files={files}, bytes={bytes}, archive hash={hash}",
                    &[
                        ("id", report.backup_id.clone()),
                        ("valid", report.valid.to_string()),
                        ("files", report.verified_files.to_string()),
                        ("bytes", report.verified_uncompressed_bytes.to_string()),
                        ("hash", report.archive_blake3.clone()),
                    ],
                ));
            }

            if let Some(plan) = self
                .server_view
                .backups
                .prune_plan
                .as_ref()
                .filter(|plan| plan.instance_id == id)
            {
                ui.group(|ui| {
                    ui.label(tr_fmt(
                        "Prune plan {fingerprint}: {candidates} candidate(s) ({bytes} reclaimable bytes); {retained} retained; {corrupt} corrupt; {unrecognized} unrecognized",
                        &[
                            ("fingerprint", plan.fingerprint.clone()),
                            ("candidates", plan.candidates.len().to_string()),
                            ("bytes", plan.reclaimable_bytes.to_string()),
                            ("retained", plan.retained.len().to_string()),
                            ("corrupt", plan.corrupt_backups.len().to_string()),
                            ("unrecognized", plan.unrecognized_entries.len().to_string()),
                        ],
                    ));
                    for kept in &plan.retained {
                        ui.label(tr_fmt(
                            "Retain {id} ({mode}): {reasons}",
                            &[
                                ("id", kept.backup_id.clone()),
                                ("mode", kept.mode.as_str().to_owned()),
                                ("reasons", kept.reasons.join(", ")),
                            ],
                        ));
                    }
                    for cand in &plan.candidates {
                        ui.label(tr_fmt(
                            "Prune candidate {id}: {bytes} bytes ({reason})",
                            &[
                                ("id", cand.backup_id.clone()),
                                ("bytes", cand.archive_bytes.to_string()),
                                ("reason", cand.reason.clone()),
                            ],
                        ));
                    }
                    for blocker in &plan.blockers {
                        ui.colored_label(self.color_text_error(), blocker);
                    }
                });
                let can_prune =
                    !busy && !dirty && plan.blockers.is_empty() && !plan.candidates.is_empty();
                let confirm_btn = ui.add_enabled(
                    can_prune,
                    egui::Button::new(tr("Review prune confirmation")),
                );
                Self::focus_outline(ui, &confirm_btn);
                if confirm_btn.clicked() {
                    self.server_view.open_dialog(ServerDialog::PruneServerBackups);
                }
            }

            if let Some(outcome) = self
                .server_view
                .backups
                .prune_outcome
                .as_ref()
                .filter(|outcome| outcome.instance_id == id)
            {
                ui.label(tr_fmt(
                    "Pruned {count} backup(s), reclaimed {bytes} bytes; retained {retained} backup(s).",
                    &[
                        ("count", outcome.removed.len().to_string()),
                        ("bytes", outcome.reclaimed_bytes.to_string()),
                        ("retained", outcome.retained_backups.len().to_string()),
                    ],
                ));
            }

            plan_anchor.unwrap()
        });
        Some(response.inner)
    }

    fn render_backup_listing(
        &mut self,
        ui: &mut Ui,
        id: &ServerInstanceId,
        listing: &crate::core::server::ServerBackupListing,
        busy: bool,
        dirty: bool,
    ) {
        ui.label(tr_fmt(
            "Stored backups: {count} ({bytes} archive bytes); keep last {keep}; pinned {pinned}; corrupt {corrupt}; unrecognized {unrecognized}",
            &[
                ("count", listing.backups.len().to_string()),
                ("bytes", listing.total_archive_bytes.to_string()),
                ("keep", listing.retention.policy.keep_last.to_string()),
                ("pinned", listing.retention.pinned.len().to_string()),
                ("corrupt", listing.corrupt_backups.len().to_string()),
                ("unrecognized", listing.unrecognized_entries.len().to_string()),
            ],
        ));
        if self.server_view.backups.keep_last_input.is_empty() && listing.retention.generation == 0
        {
            self.server_view.backups.sync_retention_inputs(listing);
        }
        egui::ScrollArea::vertical()
            .id_salt(("server-backup-list", id.as_str()))
            .max_height(180.0)
            .show(ui, |ui| {
                for summary in &listing.backups {
                    let selected = self.server_view.backups.selection.as_deref()
                        == Some(summary.backup_id.as_str());
                    let label = tr_fmt(
                        "{id} [{mode}; stopped_verified={stopped}; pinned={pinned}; {files} files; {bytes} archive bytes]",
                        &[
                            ("id", summary.backup_id.clone()),
                            ("mode", summary.mode.as_str().to_owned()),
                            ("stopped", super::chrome::yes_no(summary.stopped_verified)),
                            ("pinned", summary.pinned.to_string()),
                            ("files", summary.total_files.to_string()),
                            ("bytes", summary.archive_bytes.to_string()),
                        ],
                    );
                    let row = ui.selectable_label(selected, label);
                    Self::focus_outline(ui, &row);
                    if row.clicked() {
                        self.server_view.backups.selection = Some(summary.backup_id.clone());
                        self.server_view.backups.restore_plan = None;
                        self.server_view.backups.restore_outcome = None;
                    }
                }
            });

        if let Some(selected_id) = self.server_view.backups.selection.clone()
            && let Some(entry) = listing
                .backups
                .iter()
                .find(|item| item.backup_id == selected_id)
        {
            ui.horizontal_wrapped(|ui| {
                let verify_btn =
                    ui.add_enabled(!busy, egui::Button::new(tr("Verify selected backup")));
                Self::focus_outline(ui, &verify_btn);
                if verify_btn.clicked() {
                    self.start_server_task(ServerTaskAction::VerifyServerBackup {
                        id: id.clone(),
                        backup_id: entry.backup_id.clone(),
                    });
                }

                let pin_label = if entry.pinned {
                    "Unpin selected backup"
                } else {
                    "Pin selected backup"
                };
                let pin_btn = ui.add_enabled(!busy && !dirty, egui::Button::new(tr(pin_label)));
                Self::focus_outline(ui, &pin_btn);
                if pin_btn.clicked() {
                    self.start_server_task(ServerTaskAction::SetServerBackupPin {
                        id: id.clone(),
                        backup_id: entry.backup_id.clone(),
                        pinned: !entry.pinned,
                        expected_generation: listing.retention.generation,
                    });
                }

                let restore_ready =
                    self.server_view
                        .backups
                        .restore_plan
                        .as_ref()
                        .is_some_and(|plan| {
                            plan.instance_id == *id
                                && plan.backup_id == entry.backup_id
                                && plan.blockers.is_empty()
                        });
                let review =
                    ui.add_enabled(!busy && !dirty, egui::Button::new(tr("Review restore")));
                Self::focus_outline(ui, &review);
                if review.clicked() {
                    self.server_view.backups.restore_plan = None;
                    self.start_server_task(ServerTaskAction::PlanServerRestore {
                        id: id.clone(),
                        backup_id: entry.backup_id.clone(),
                    });
                }
                if restore_ready {
                    let apply = ui.add_enabled(
                        !busy && !dirty,
                        egui::Button::new(tr("Restore selected backup")),
                    );
                    Self::focus_outline(ui, &apply);
                    if apply.clicked() {
                        self.server_view.open_dialog(ServerDialog::RestoreServerBackup);
                    }
                }
            });
        }

        if let Some(plan) = self
            .server_view
            .backups
            .restore_plan
            .as_ref()
            .filter(|plan| plan.instance_id == *id)
        {
            ui.group(|ui| {
                ui.label(tr_fmt("Restore review for backup {id}", &[("id", plan.backup_id.clone())]));
                ui.label(tr_fmt("Archive BLAKE3: {hash}; files: {files}; restore bytes: {bytes}", &[
                    ("hash", plan.archive_blake3.clone()),
                    ("files", plan.restore_files.to_string()),
                    ("bytes", plan.restore_bytes.to_string()),
                ]));
                ui.label(tr_fmt("Rollback journal bytes: {rollback}; required free bytes: {required}; available: {available}", &[
                    ("rollback", plan.rollback_bytes.to_string()),
                    ("required", plan.required_free_bytes.to_string()),
                    ("available", plan.available_free_bytes.to_string()),
                ]));
                ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                for warning in &plan.warnings { ui.label(warning); }
                for blocker in &plan.blockers { ui.colored_label(self.color_text_error(), blocker); }
                ui.label(tr("Restore replaces only backed-up save and profile files. Unknown files are left untouched; replaced bytes are retained for rollback."));
            });
        }
        if let Some(outcome) = self
            .server_view
            .backups
            .restore_outcome
            .as_ref()
            .filter(|outcome| outcome.instance_id == *id)
        {
            ui.label(tr_fmt(
                if outcome.rolled_back {
                    "Restore {restore} rolled back: {files} files, {bytes} bytes."
                } else {
                    "Restore {restore} completed: {files} files, {bytes} bytes."
                },
                &[
                    ("restore", outcome.restore_id.clone()),
                    ("files", outcome.restored_files.to_string()),
                    ("bytes", outcome.restored_bytes.to_string()),
                ],
            ));
        }

        ui.horizontal_wrapped(|ui| {
            let keep_caption = ui.label(tr("Keep last backups:"));
            ui.add(
                egui::TextEdit::singleline(&mut self.server_view.backups.keep_last_input)
                    .desired_width(60.0),
            )
            .labelled_by(keep_caption.id);
            let max_caption = ui.label(tr("Max total bytes (optional):"));
            ui.add(
                egui::TextEdit::singleline(&mut self.server_view.backups.max_bytes_input)
                    .desired_width(120.0),
            )
            .labelled_by(max_caption.id);
            let save_ret = ui.add_enabled(
                !busy && !dirty,
                egui::Button::new(tr("Save backup retention policy")),
            );
            Self::focus_outline(ui, &save_ret);
            if save_ret.clicked() {
                match parse_retention_inputs(
                    &self.server_view.backups.keep_last_input,
                    &self.server_view.backups.max_bytes_input,
                ) {
                    Ok(policy) => {
                        self.server_view.backups.validation_error = None;
                        self.start_server_task(ServerTaskAction::SetServerBackupRetention {
                            id: id.clone(),
                            policy,
                            expected_generation: listing.retention.generation,
                        });
                    }
                    Err(message) => {
                        self.server_view.backups.validation_error = Some(message);
                    }
                }
            }
            let prune_plan_btn = ui.add_enabled(
                !busy && !dirty,
                egui::Button::new(tr("Review backup pruning")),
            );
            Self::focus_outline(ui, &prune_plan_btn);
            if prune_plan_btn.clicked() {
                self.server_view.backups.prune_plan = None;
                self.start_server_task(ServerTaskAction::PlanServerBackupPrune {
                    id: id.clone(),
                    keep_last_override: None,
                });
            }
        });
        if let Some(error) = &self.server_view.backups.validation_error {
            ui.colored_label(self.color_text_error(), error);
        }
    }
}

fn parse_retention_inputs(
    keep_last_raw: &str,
    max_bytes_raw: &str,
) -> Result<BackupRetentionPolicy, String> {
    let keep_last = keep_last_raw.trim().parse::<u16>().map_err(|_| {
        tr_fmt(
            "Enter a keep-last count from 1 to {max}.",
            &[("max", MAX_BACKUP_KEEP_LAST.to_string())],
        )
    })?;
    let max_total_bytes =
        if max_bytes_raw.trim().is_empty() {
            None
        } else {
            Some(max_bytes_raw.trim().parse::<u64>().map_err(|_| {
                tr("Enter an integer byte budget or leave blank for unlimited.")
            })?)
        };
    let policy = BackupRetentionPolicy {
        keep_last,
        max_total_bytes,
    };
    policy.validate()?;
    Ok(policy)
}
