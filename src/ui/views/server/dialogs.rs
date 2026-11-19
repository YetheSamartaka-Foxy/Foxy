use crate::core::server::DraftChange;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerDialog;
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.server_view.dialog else {
            return;
        };
        let title = match dialog {
            ServerDialog::AcquireRepositoryContent => "Acquire the reviewed repository content?",
            ServerDialog::RollbackConfiguration => "Select this previous configuration revision?",
            ServerDialog::SelectDeploymentRecord => "Select this offline deployment record?",
            ServerDialog::ReclaimDeploymentStorage => "Reclaim the reviewed deployment artifacts?",
            ServerDialog::PruneServerBackups => "Prune excess server backups?",
            ServerDialog::RestoreServerBackup => "Restore the reviewed server backup?",
            ServerDialog::RollbackServerRestore => "Roll back the interrupted server restore?",
            ServerDialog::NativeInstall
                if self
                    .server_view
                    .native_install_plan
                    .as_ref()
                    .and_then(|plan| plan.maintenance.as_ref())
                    .is_some_and(|review| {
                        matches!(
                            &review.action,
                            crate::core::server::NativeMaintenanceAction::Update
                        )
                    }) =>
            {
                "Apply the reviewed server update?"
            }
            ServerDialog::NativeInstall
                if self
                    .server_view
                    .native_install_plan
                    .as_ref()
                    .and_then(|plan| plan.maintenance.as_ref())
                    .is_some_and(|review| {
                        matches!(
                            &review.action,
                            crate::core::server::NativeMaintenanceAction::Repair
                        )
                    }) =>
            {
                "Apply the reviewed server repair?"
            }
            ServerDialog::NativeInstall => "Install the reviewed dedicated server build?",
            ServerDialog::AcquireWorkshop => "Acquire the reviewed Workshop items?",
            ServerDialog::Rename => "Rename draft",
            ServerDialog::Clone => "Clone draft",
            ServerDialog::Remove => "Remove draft",
            ServerDialog::DiscardAndClose => "Discard draft changes and close?",
        };
        let mut confirm = false;
        let mut cancel = false;
        let name_id = self.server_draft_focus_id("server-dialog-name").unwrap();
        egui::Modal::new(egui::Id::unique("server-draft-confirmation")).show(ctx, |ui| {
            if let Some(error) = &self.server_view.error {
                ui.colored_label(self.color_text_error(), self.t(&error.to_string()));
            }
            let busy = self.server_view.busy();
            ui.heading(self.t(title));
            if matches!(dialog, ServerDialog::Rename | ServerDialog::Clone) {
                let caption = ui.label(self.t("Instance display name"));
                ui.add(egui::TextEdit::singleline(&mut self.server_view.dialog_name).id(name_id))
                    .labelled_by(caption.id);
            }
            ui.label(self.t(match dialog {
                ServerDialog::AcquireRepositoryContent=>"The current remote publication will be downloaded and verified into an inactive retained stage. Previous complete stages, source folders and client enablement are kept.",
                ServerDialog::RollbackConfiguration => "Only the saved configuration selection changes. The current draft, game installation, runtime and saves are kept.",
                ServerDialog::SelectDeploymentRecord => "This changes only the offline selected record. It does not activate a deployment, alter the server, or start a process.",
                ServerDialog::ReclaimDeploymentStorage => "Only the listed unreferenced stages, preparations, runtime copies and interrupted leftovers are removed. Content sources, deployment records, configuration archives, runtime state and other instances are kept.",
                ServerDialog::PruneServerBackups => "Only unpinned excess backups matching the reviewed prune plan are removed. Pinned backups, the newest verified stopped-consistent backup, corrupt backups and unknown entries are kept.",
                ServerDialog::RestoreServerBackup => "Foxy rechecks the exact saved engine, configuration, content and provider compatibility before restoring. Only backed-up save and profile files are replaced; unknown files remain untouched and previous bytes are retained for rollback.",
                ServerDialog::RollbackServerRestore => "Foxy uses the retained restore journal to restore the prior save and profile bytes. Review status first and keep this server stopped until recovery is complete.",
                ServerDialog::NativeInstall if self.server_view.native_install_plan.as_ref().is_some_and(|plan| plan.maintenance.is_some()) => "Foxy downloads the reviewed build into a fresh folder, verifies every file, then binds the new installation to this instance only. The old installation root, unknown files and other instances are retained. Foxy does not restart the server automatically.",
                ServerDialog::NativeInstall => "Foxy downloads the reviewed depots into a new staging folder, verifies every file, then publishes the folder and binds it to this instance. Existing folders and installations are never changed.",
                ServerDialog::AcquireWorkshop => "Each item downloads into a new verified revision. Only fully verified items are pinned and update their content profile; earlier revisions are kept and failed items change nothing.",
                ServerDialog::Rename => "Renaming preserves the engine hostname and server configuration.",
                ServerDialog::Clone => "Cloning copies draft settings under a new ID. Choose distinct ports before deployment.",
                ServerDialog::Remove => "Remove this draft record only? Game files and deployments are kept.",
                ServerDialog::DiscardAndClose => "The entered draft changes have not been saved.",
            }));
            if dialog == ServerDialog::RollbackConfiguration && let Some(plan) = &self.server_view.configuration_rollback {
                ui.label(&plan.revision_id);
                ui.label(self.t_fmt("Expected configuration generation: {generation}", &[("generation", plan.expected_generation.to_string())]));
            }
            if dialog == ServerDialog::SelectDeploymentRecord
                && let Some(plan) = &self.server_view.deployment_record_selection_plan
            {
                ui.label(self.t_fmt(
                    "Previous offline record: {record}",
                    &[("record", plan.previous_record_id.as_deref().unwrap_or("None").to_owned())],
                ));
                ui.label(self.t_fmt(
                    "Proposed offline record: {record}",
                    &[("record", plan.target.record_id.clone())],
                ));
                ui.label(self.t_fmt(
                    "Expected selection generation: {generation}",
                    &[("generation", plan.expected_generation.to_string())],
                ));
                ui.label(self.t_fmt(
                    "Selection fingerprint: {fingerprint}",
                    &[("fingerprint", plan.selection_fingerprint.clone())],
                ));
                ui.label(self.t_fmt(
                    "Selection review fingerprint: {fingerprint}",
                    &[("fingerprint", plan.fingerprint.clone())],
                ));
                ui.label(self.t_fmt(
                    "Configuration revision: {revision}; content stage: {stage}",
                    &[("revision", plan.target.manifest.configuration_revision.clone()), ("stage", plan.target.manifest.content_stage_id.clone())],
                ));
                ui.label(self.t_fmt(
                    "Configuration archive fingerprint: {fingerprint}",
                    &[("fingerprint", plan.target.manifest.configuration_manifest_fingerprint.clone())],
                ));
                ui.label(self.t_fmt(
                    "Content archive fingerprint: {fingerprint}",
                    &[("fingerprint", plan.target.manifest.content_manifest_fingerprint.clone())],
                ));
                ui.label(self.t("Confirming saves an offline selection only. Full record verification completed during review."));
            }
            if dialog == ServerDialog::ReclaimDeploymentStorage
                && let Some(plan) = &self.server_view.reconciliation_plan
            {
                ui.label(self.t_fmt(
                    "Artifacts to remove: {count}; bytes: {bytes}",
                    &[("count", plan.candidates.len().to_string()), ("bytes", plan.reclaimable_bytes.to_string())],
                ));
                ui.label(self.t_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                ui.label(self.t("Foxy re-reviews storage before removing anything and refuses while this instance has a running or unresolved engine consumer."));
            }
            if dialog == ServerDialog::PruneServerBackups
                && let Some(plan) = &self.server_view.backups.prune_plan
            {
                ui.label(self.t_fmt(
                    "Backups to prune: {count}; bytes: {bytes}; retained: {retained}",
                    &[
                        ("count", plan.candidates.len().to_string()),
                        ("bytes", plan.reclaimable_bytes.to_string()),
                        ("retained", plan.retained.len().to_string()),
                    ],
                ));
                ui.label(self.t_fmt(
                    "Review fingerprint: {fingerprint}",
                    &[("fingerprint", plan.fingerprint.clone())],
                ));
            }
            if dialog == ServerDialog::RestoreServerBackup
                && let Some(plan) = &self.server_view.backups.restore_plan
            {
                ui.label(self.t_fmt(
                    "Backup {backup}; restore files: {files}; restore bytes: {bytes}; rollback bytes: {rollback}",
                    &[
                        ("backup", plan.backup_id.clone()),
                        ("files", plan.restore_files.to_string()),
                        ("bytes", plan.restore_bytes.to_string()),
                        ("rollback", plan.rollback_bytes.to_string()),
                    ],
                ));
                ui.label(self.t_fmt(
                    "Review fingerprint: {fingerprint}",
                    &[("fingerprint", plan.fingerprint.clone())],
                ));
            }
            if dialog == ServerDialog::RollbackServerRestore
                && let Some(pending) = self
                    .server_view
                    .backups
                    .restore_status
                    .as_ref()
                    .and_then(|status| status.pending.as_ref())
            {
                ui.label(self.t_fmt(
                    "Backup {backup}; restore {restore}",
                    &[
                        ("backup", pending.backup_id.clone()),
                        ("restore", pending.restore_id.clone()),
                    ],
                ));
                ui.label(self.t_fmt(
                    "Recovery fingerprint: {fingerprint}",
                    &[("fingerprint", pending.fingerprint.clone())],
                ));
            }
            if dialog == ServerDialog::NativeInstall
                && let Some(plan) = &self.server_view.native_install_plan
            {
                ui.label(self.t_fmt(
                    "Steam app {app}, branch {branch}, build {build}",
                    &[("app", plan.app_id.to_string()), ("branch", plan.branch.clone()), ("build", plan.review.selection.build_id.map_or_else(|| self.t("Unknown"), |build| build.to_string()))],
                ));
                ui.label(self.t_fmt("New installation folder: {path}", &[("path", plan.directory.display().to_string())]));
                ui.label(self.t_fmt("Download size: {bytes} bytes", &[("bytes", plan.required_bytes.to_string())]));
                if let Some(review) = &plan.maintenance {
                    ui.label(self.t_fmt(
                        "Previous installation: {path}",
                        &[("path", review.previous.directory.display().to_string())],
                    ));
                    ui.label(self.t_fmt(
                        "Previous build: {previous}; reviewed build: {reviewed}",
                        &[
                            ("previous", review.previous_build_id.map_or_else(|| self.t("Unknown"), |build| build.to_string())),
                            ("reviewed", plan.review.selection.build_id.map_or_else(|| self.t("Unknown"), |build| build.to_string())),
                        ],
                    ));
                    ui.label(self.t_fmt(
                        "Other instances in this game space sharing the old installation: {count}",
                        &[("count", review.retained_peer_instances.len().to_string())],
                    ));
                    ui.label(self.t("Foxy checks consumers and the same review fingerprint again before applying. Live or unresolved consumers block the operation."));
                }
                ui.label(self.t_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                ui.label(self.t("Foxy reviews Steam again first and stops if a different build was published."));
            }
            if dialog == ServerDialog::AcquireWorkshop
                && let Some(plan) = &self.server_view.workshop_plan
            {
                let downloads = plan.items.iter().filter(|item| matches!(item.action, crate::core::server::WorkshopItemAction::Acquire | crate::core::server::WorkshopItemAction::Update | crate::core::server::WorkshopItemAction::Repair)).count();
                ui.label(self.t_fmt(
                    "Items to download: {count}; download size: {bytes} bytes",
                    &[("count", downloads.to_string()), ("bytes", plan.required_bytes.to_string())],
                ));
                ui.label(self.t_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                ui.label(self.t("Foxy checks Steam again and fails an item whose revision changed since this review."));
            }
            if dialog==ServerDialog::AcquireRepositoryContent && let Some(plan)=&self.server_view.acquisition_plan {
                ui.label(self.t_fmt("Acquisition plan fingerprint: {fingerprint}",&[("fingerprint",plan.fingerprint.clone())]));
                ui.label(self.t_fmt("Repository profiles to acquire: {count}",&[("count",plan.profiles.len().to_string())]));
            }
            ui.horizontal_wrapped(|ui| {
                let cancel_button = ui.add_enabled(!busy, egui::Button::new(self.t("Cancel")));
                if cancel_button.enabled() && ui.is_visible() && std::mem::take(&mut self.server_view.focus_dialog_cancel) {
                    cancel_button.request_focus();
                }
                Self::focus_outline(ui, &cancel_button);
                if cancel_button.clicked() {
                    cancel = true;
                }
                let confirm_button = ui.add_enabled(!busy, egui::Button::new(self.t(match dialog {
                    ServerDialog::DiscardAndClose => "Discard and close",
                    ServerDialog::SelectDeploymentRecord => "Select offline record",
                    ServerDialog::ReclaimDeploymentStorage => "Reclaim reviewed artifacts",
                    ServerDialog::PruneServerBackups => "Prune excess backups",
                    ServerDialog::RestoreServerBackup => "Restore reviewed backup",
                    ServerDialog::RollbackServerRestore => "Roll back restore",
                    ServerDialog::NativeInstall if self.server_view.native_install_plan.as_ref().and_then(|plan| plan.maintenance.as_ref()).is_some_and(|review| matches!(&review.action, crate::core::server::NativeMaintenanceAction::Update)) => "Apply reviewed update",
                    ServerDialog::NativeInstall if self.server_view.native_install_plan.as_ref().and_then(|plan| plan.maintenance.as_ref()).is_some_and(|review| matches!(&review.action, crate::core::server::NativeMaintenanceAction::Repair)) => "Apply reviewed repair",
                    ServerDialog::NativeInstall => "Install reviewed build",
                    ServerDialog::AcquireWorkshop => "Acquire reviewed items",
                    ServerDialog::AcquireRepositoryContent=>"Acquire repository content",
                    _ => "Confirm",
                })));
                Self::focus_outline(ui, &confirm_button);
                if confirm_button.clicked() { confirm = true; }
            });
            if !busy && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) { cancel = true; }
        });
        if cancel {
            if dialog == ServerDialog::SelectDeploymentRecord {
                self.server_view
                    .invalidate_deployment_record_selection_plan();
            }
            if dialog == ServerDialog::ReclaimDeploymentStorage {
                self.server_view.reconciliation_plan = None;
            }
            if matches!(
                dialog,
                ServerDialog::PruneServerBackups | ServerDialog::RestoreServerBackup | ServerDialog::RollbackServerRestore
            ) {
                self.server_view.pending_focus = Some("server-backup-controls");
            }
            if dialog == ServerDialog::NativeInstall {
                self.server_view.native_install_plan = None;
                self.server_view.pending_focus = Some("server-native-install-controls");
            }
            if dialog == ServerDialog::AcquireWorkshop {
                self.server_view.workshop_plan = None;
            }
            if dialog == ServerDialog::AcquireRepositoryContent {
                self.server_view.acquisition_plan = None;
            }
            self.server_view.dialog = None;
            return;
        }
        if confirm {
            if dialog == ServerDialog::AcquireRepositoryContent {
                if let Some(plan) = self.server_view.acquisition_plan.take() {
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::AcquireContent(plan),
                    );
                }
                return;
            }
            if dialog == ServerDialog::RollbackConfiguration {
                if let Some(plan) = self.server_view.configuration_rollback.clone() {
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::RollbackConfiguration(plan),
                    );
                }
                return;
            }
            if dialog == ServerDialog::SelectDeploymentRecord {
                if let Some(plan) = self.server_view.deployment_record_selection_plan.take() {
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::SelectDeploymentRecord(plan),
                    );
                }
                return;
            }
            if dialog == ServerDialog::AcquireWorkshop {
                if let Some(plan) = self.server_view.workshop_plan.take() {
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::ApplyWorkshop(plan),
                    );
                }
                return;
            }
            if dialog == ServerDialog::NativeInstall {
                if let Some(plan) = self.server_view.native_install_plan.take() {
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::ApplyNativeInstall(plan),
                    );
                }
                return;
            }
            if dialog == ServerDialog::ReclaimDeploymentStorage {
                if let Some(plan) = self.server_view.reconciliation_plan.take() {
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::ApplyReconciliation(plan),
                    );
                }
                return;
            }
            if dialog == ServerDialog::PruneServerBackups {
                if let Some(plan) = self.server_view.backups.prune_plan.as_ref() {
                    let id = plan.instance_id.clone();
                    let expected_plan_fingerprint = plan.fingerprint.clone();
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::ApplyServerBackupPrune {
                            id,
                            expected_plan_fingerprint,
                            keep_last_override: None,
                        },
                    );
                }
                return;
            }
            if dialog == ServerDialog::RestoreServerBackup {
                if let Some(plan) = self.server_view.backups.restore_plan.as_ref() {
                    let id = plan.instance_id.clone();
                    let backup_id = plan.backup_id.clone();
                    let expected_plan_fingerprint = plan.fingerprint.clone();
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::ApplyServerRestore {
                            id,
                            backup_id,
                            expected_plan_fingerprint,
                        },
                    );
                }
                return;
            }
            if dialog == ServerDialog::RollbackServerRestore {
                if let Some((id, expected_journal_fingerprint)) = self
                    .server_view
                    .backups
                    .restore_status
                    .as_ref()
                    .and_then(|status| {
                        status.pending.as_ref().map(|pending| {
                            (status.instance_id.clone(), pending.fingerprint.clone())
                        })
                    })
                {
                    self.server_view.dialog = None;
                    self.start_server_task(
                        crate::ui::app::server::ServerTaskAction::RollbackServerRestore {
                            id,
                            expected_journal_fingerprint,
                        },
                    );
                }
                return;
            }
            if dialog == ServerDialog::DiscardAndClose {
                self.server_view.discard();
                self.server_view.dialog = None;
                self.request_app_close(ctx, "server draft discard");
                return;
            }
            if let Some(editor) = &self.server_view.editor {
                let id = editor.baseline.id.clone();
                let expected_revision = editor.baseline.revision;
                let name = self.server_view.dialog_name.clone();
                let change = match dialog {
                    ServerDialog::Rename => DraftChange::Rename {
                        id,
                        name,
                        expected_revision,
                    },
                    ServerDialog::Clone => DraftChange::Clone {
                        id,
                        name,
                        expected_revision,
                    },
                    ServerDialog::Remove => DraftChange::Remove {
                        id,
                        expected_revision,
                    },
                    ServerDialog::DiscardAndClose
                    | ServerDialog::RollbackConfiguration
                    | ServerDialog::SelectDeploymentRecord
                    | ServerDialog::ReclaimDeploymentStorage
                    | ServerDialog::PruneServerBackups
                    | ServerDialog::RestoreServerBackup
                    | ServerDialog::RollbackServerRestore
                    | ServerDialog::NativeInstall
                    | ServerDialog::AcquireWorkshop
                    | ServerDialog::AcquireRepositoryContent => {
                        unreachable!()
                    }
                };
                self.start_server_draft_task(Some(change));
            }
        }
    }

    pub(crate) fn server_close_is_blocked(&mut self, ctx: &egui::Context) -> bool {
        if self.close_requested_at.is_none() {
            return false;
        }
        if self.server_view.busy() {
            self.close_requested_at = Some(std::time::Instant::now());
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            egui::Modal::new(egui::Id::unique("server-close-drain")).show(ctx, |ui: &mut Ui| {
                ui.label(self.t("Finishing the server draft operation before closing..."));
            });
            crate::ui::app::request_frame_after(ctx, std::time::Duration::from_millis(25));
            return true;
        }
        if self.server_view.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_requested_at = None;
            self.server_view.open_dialog(ServerDialog::DiscardAndClose);
            self.needs_repaint = true;
            return true;
        }
        false
    }
}
