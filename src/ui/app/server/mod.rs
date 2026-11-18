pub(crate) mod arma_admin;
mod backups;
mod maintenance;
mod publication;
mod rcon;
pub(crate) mod reforger_admin;
mod services;
mod sessions;
mod state;
mod steam_auth;
mod tasks;

pub(crate) use maintenance::{MaintenanceConfirmation, MaintenanceForm, MaintenanceNotice};
pub(crate) use rcon::{RconConfirmation, RconForm, RconNotice, RconPasswordEntry, RconSnapshot};
pub(crate) use state::{ServerDialog, ServerViewState};
pub(crate) use tasks::ServerTaskAction;

use crate::core::game::{registry, spaces};
use crate::core::server::{ContentStageControl, ContentStagePhase, DraftChange, ServerStoreError};
use crate::core::tasks::db_process_lock;
use crate::ui::app::Foxy;
use tasks::{ServerTask, ServerTaskKind, ServerTaskScope};

impl Foxy {
    pub(crate) fn server_draft_focus_id(&self, target: &str) -> Option<egui::Id> {
        let space = spaces::active_game_space().space_id;
        match target {
            "server-new-name" => Some(egui::Id::unique(("server.new-name", space))),
            "server-session-controls" => self.server_view.sessions.controls_focus,
            "server-service-controls" => self.server_view.services.controls_focus,
            "server-service-path" => Some(egui::Id::unique(("server.service-export-path", space))),
            "server-mission-import-path" => {
                Some(egui::Id::unique(("server.mission-import-path", space)))
            }
            "server-hostname" => self
                .server_view
                .editor
                .as_ref()
                .map(|editor| editor.field_scope().with("hostname")),
            "server-installation-directory" => self
                .server_view
                .editor
                .as_ref()
                .map(|editor| editor.field_scope().with("installation-directory")),
            "server-native-install-directory" => self
                .server_view
                .editor
                .as_ref()
                .map(|editor| editor.field_scope().with("native-install-directory")),
            "server-scenario" => self
                .server_view
                .editor
                .as_ref()
                .filter(|editor| {
                    matches!(
                        editor.draft.definition,
                        crate::core::server::ServerDefinition::Reforger(_)
                    )
                })
                .map(|editor| editor.field_scope().with("scenario")),
            "server-content-controls" => Some(egui::Id::unique(("server.content-controls", space))),
            "server-content-stage-controls" => {
                Some(egui::Id::unique(("server.content-stage-controls", space)))
            }
            "server-deployment-composition-controls" => Some(egui::Id::unique((
                "server.deployment-composition-controls",
                space,
            ))),
            "server-deployment-record-controls" => Some(egui::Id::unique((
                "server.deployment-record-controls",
                space,
            ))),
            "server-deployment-preparation-controls" => Some(egui::Id::unique((
                "server.deployment-preparation-controls",
                space,
            ))),
            "server-deployment-activation-controls" => Some(egui::Id::unique((
                "server.deployment-activation-controls",
                space,
            ))),
            "server-lifecycle-controls" => {
                Some(egui::Id::unique(("server.lifecycle-controls", space)))
            }
            "server-dialog-name" => Some(egui::Id::unique((
                "server.dialog-name",
                space,
                self.server_view.selected.as_ref().map(|id| id.as_str()),
                self.server_view.dialog,
            ))),
            _ => None,
        }
    }

    pub(crate) fn server_drafts_supported(&self) -> bool {
        registry()
            .get(&spaces::active_game_space().game_id)
            .is_some_and(|module| module.server_capabilities().draft_definitions)
    }

    pub(crate) fn start_server_draft_task(&mut self, change: Option<DraftChange>) {
        self.start_server_task(ServerTaskAction::Catalog(change));
    }

    pub(crate) fn start_server_task(&mut self, action: ServerTaskAction) {
        let passive_poll = matches!(
            &action,
            ServerTaskAction::PollInstanceMonitoring(_)
                | ServerTaskAction::LoadMaintenance(_)
                | ServerTaskAction::LoadRcon(_)
        );
        if self
            .server_view
            .task
            .as_ref()
            .is_some_and(|task| task.passive)
        {
            if !passive_poll {
                self.server_view.queue_passive_action(action);
            }
            return;
        }
        let action = match action {
            ServerTaskAction::Inspect(directory) => match std::path::absolute(directory) {
                Ok(directory) => ServerTaskAction::Inspect(directory),
                Err(_) => {
                    self.server_view.error = Some(ServerStoreError::UnsafePath);
                    return;
                }
            },
            action => action,
        };
        if self.server_view.busy()
            || (self.effective_app_mode() == crate::core::app_mode::AppMode::Server
                && !self.mode_ownership.ready())
            || self.db_lock_conflict.is_some()
            || self.pending_game_space_switch.is_some()
            || self.close_requested_at.is_some()
            || !self.server_drafts_supported()
        {
            return;
        }
        if self.server_view.dirty()
            && !matches!(
                &action,
                ServerTaskAction::Catalog(Some(DraftChange::Replace { .. }))
                    | ServerTaskAction::LoadMissionLibrary
                    | ServerTaskAction::PlanMissionImport(_)
                    | ServerTaskAction::PlanMissionRefresh(_)
                    | ServerTaskAction::PlanMissionAcquisition { .. }
                    | ServerTaskAction::ApplyMissionImport(_)
                    | ServerTaskAction::PlanDeploymentRecordSelection { .. }
                    | ServerTaskAction::SelectDeploymentRecord(_)
                    | ServerTaskAction::PlanDeploymentPreparation { .. }
                    | ServerTaskAction::BuildDeploymentPreparation(_)
                    | ServerTaskAction::ListDeploymentPreparations(_)
                    | ServerTaskAction::VerifyDeploymentPreparation { .. }
                    | ServerTaskAction::ReviewReforgerImport(_)
                    | ServerTaskAction::ImportScenarioLog { .. }
                    | ServerTaskAction::LoadScenarioDiscovery(_)
                    | ServerTaskAction::PollInstanceMonitoring(_)
                    | ServerTaskAction::LoadMaintenance(_)
                    | ServerTaskAction::ChangeMaintenance { .. }
                    | ServerTaskAction::RunMaintenanceJob { .. }
                    | ServerTaskAction::CancelMaintenanceOccurrence { .. }
                    | ServerTaskAction::LoadRcon(_)
                    | ServerTaskAction::TestRcon { .. }
                    | ServerTaskAction::SendRconCommand { .. }
                    | ServerTaskAction::SendArmaAdminCommand { .. }
                    | ServerTaskAction::SendReforgerAdminCommand { .. }
            )
        {
            self.server_view.error = Some(ServerStoreError::Invalid(
                "Save or discard draft changes before reloading or changing instances.".into(),
            ));
            return;
        }
        let Some(path) = self
            .server_context_path()
            .or_else(db_process_lock::held_database_path)
        else {
            self.server_view.error = Some(ServerStoreError::Ownership);
            return;
        };
        let Some(root) = path
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
        else {
            self.server_view.error = Some(ServerStoreError::Ownership);
            return;
        };
        let active = spaces::active_game_space();
        let scope = ServerTaskScope {
            root: root.to_path_buf(),
            space_id: active.space_id,
            game_id: active.game_id,
            request_id: crate::core::api::next_operation_id("server-draft"),
            owner: self.mode_ownership.owner.as_ref().map(|owner| {
                (
                    owner.attachment.scope().owner_id.clone(),
                    owner.attachment.scope().generation,
                )
            }),
        };
        if scope.database_path() != path {
            self.server_view.error = Some(ServerStoreError::Ownership);
            return;
        }
        if passive_poll {
            let maintenance_poll = matches!(&action, ServerTaskAction::LoadMaintenance(_));
            let rcon_poll = matches!(&action, ServerTaskAction::LoadRcon(_));
            let worker_scope = scope.clone();
            let selected = self.server_view.selected.clone();
            let repaint = self.repaint_ctx.clone();
            let worker = std::thread::Builder::new()
                .name("server-draft".into())
                .spawn(move || {
                    let result = tasks::execute(
                        worker_scope,
                        action,
                        selected,
                        ContentStageControl::default(),
                    );
                    Foxy::request_background_repaint(repaint.as_ref());
                    result
                });
            match worker {
                Ok(worker) => {
                    self.server_view.task = Some(ServerTask {
                        scope,
                        passive: true,
                        restore_focus: None,
                        worker,
                        stage_control: None,
                        kind: None,
                        maintenance_poll,
                        rcon_poll,
                    });
                }
                Err(_) => self.server_view.error = Some(ServerStoreError::Io),
            }
            return;
        }
        let inspecting = matches!(&action, ServerTaskAction::Inspect(_));
        if matches!(
            &action,
            ServerTaskAction::ApplyNativeActivation(_)
                | ServerTaskAction::ResolveNativeActivation(_)
        ) {
            self.server_view.activation_outcome = None;
            self.server_view.activation_status = None;
        }
        if matches!(
            &action,
            ServerTaskAction::StartHeadlessClients { .. }
                | ServerTaskAction::StopHeadlessClients { .. }
                | ServerTaskAction::RestartHeadlessClients { .. }
        ) {
            self.server_view.lifecycle_outcome = None;
        }
        if matches!(
            &action,
            ServerTaskAction::StartInstance(_)
                | ServerTaskAction::StopInstance { .. }
                | ServerTaskAction::RestartInstance { .. }
        ) {
            self.server_view.lifecycle_outcome = None;
            self.server_view.lifecycle_status = None;
            self.server_view.lifecycle_monitoring = None;
            self.server_view.monitoring_last_refresh = None;
            self.server_view.monitoring_search.clear();
            self.server_view.monitoring_paused = false;
            self.server_view.monitoring_auto_scroll = true;
        }
        if matches!(
            &action,
            ServerTaskAction::ApplyNativeActivation(_)
                | ServerTaskAction::ResolveNativeActivation(_)
                | ServerTaskAction::Catalog(_)
        ) {
            self.server_view.lifecycle_plan = None;
            self.server_view.lifecycle_status = None;
            self.server_view.lifecycle_outcome = None;
            self.server_view.lifecycle_allow_force = false;
        }
        if matches!(&action, ServerTaskAction::Preflight(_)) {
            self.server_view.preflight = None;
            self.server_view.status = None;
        }
        let kind = match &action {
            ServerTaskAction::SendReforgerAdminCommand {
                command: crate::core::server::rcon::reforger::ReforgerAdminCommand::ReloadScenario,
                ..
            } => Some(ServerTaskKind::Lifecycle),
            ServerTaskAction::LoadReforgerAddonLock(_)
            | ServerTaskAction::PlanReforgerAddonAcquisition { .. }
            | ServerTaskAction::AcquireReforgerAddons(_) => Some(ServerTaskKind::Acquisition),
            ServerTaskAction::PlanContentAcquisition { .. }
            | ServerTaskAction::AcquireContent(_) => Some(ServerTaskKind::Acquisition),
            ServerTaskAction::ReviewServerKeys { .. }
            | ServerTaskAction::ApplyServerKeys { .. } => Some(ServerTaskKind::Keys),
            ServerTaskAction::LoadMissionLibrary
            | ServerTaskAction::PlanMissionImport(_)
            | ServerTaskAction::PlanMissionRefresh(_)
            | ServerTaskAction::PlanMissionAcquisition { .. }
            | ServerTaskAction::ApplyMissionImport(_) => Some(ServerTaskKind::Mission),
            ServerTaskAction::PlanInstanceStart(_)
            | ServerTaskAction::InspectInstanceRuntime(_)
            | ServerTaskAction::PollInstanceMonitoring(_)
            | ServerTaskAction::OpenInstanceRuntimeLogs(_)
            | ServerTaskAction::ExportMonitoringSnapshot { .. }
            | ServerTaskAction::StartInstance(_)
            | ServerTaskAction::StopInstance { .. }
            | ServerTaskAction::RestartInstance { .. }
            | ServerTaskAction::StartHeadlessClients { .. }
            | ServerTaskAction::StopHeadlessClients { .. }
            | ServerTaskAction::RestartHeadlessClients { .. } => Some(ServerTaskKind::Lifecycle),
            ServerTaskAction::PlanNativeActivation { .. }
            | ServerTaskAction::ApplyNativeActivation(_)
            | ServerTaskAction::InspectNativeActivation(_)
            | ServerTaskAction::PlanNativeRecovery(_)
            | ServerTaskAction::ResolveNativeActivation(_) => Some(ServerTaskKind::Activation),
            ServerTaskAction::PlanContentStage { .. }
            | ServerTaskAction::ApplyContentStage(_)
            | ServerTaskAction::VerifyContentStage { .. } => Some(ServerTaskKind::ContentStage),
            ServerTaskAction::PlanDeploymentComposition(_) => Some(ServerTaskKind::Composition),
            ServerTaskAction::PrepareDeploymentRecord { .. }
            | ServerTaskAction::CommitDeploymentRecord(_)
            | ServerTaskAction::VerifyDeploymentRecord { .. }
            | ServerTaskAction::PlanDeploymentRecordSelection { .. }
            | ServerTaskAction::SelectDeploymentRecord(_) => Some(ServerTaskKind::DeploymentRecord),
            ServerTaskAction::PlanPlayerPublication(_)
            | ServerTaskAction::ApplyPlayerPublication(_)
            | ServerTaskAction::VerifyPlayerPublication(_) => {
                Some(ServerTaskKind::PlayerPublication)
            }
            ServerTaskAction::PlanDeploymentPreparation { .. }
            | ServerTaskAction::BuildDeploymentPreparation(_)
            | ServerTaskAction::ListDeploymentPreparations(_)
            | ServerTaskAction::VerifyDeploymentPreparation { .. } => {
                Some(ServerTaskKind::Preparation)
            }
            ServerTaskAction::PlanReconciliation(_) | ServerTaskAction::ApplyReconciliation(_) => {
                Some(ServerTaskKind::Reconciliation)
            }
            ServerTaskAction::PlanServerBackup { .. }
            | ServerTaskAction::CreateServerBackup { .. }
            | ServerTaskAction::VerifyServerBackup { .. }
            | ServerTaskAction::PlanServerBackupPrune { .. }
            | ServerTaskAction::ApplyServerBackupPrune { .. }
            | ServerTaskAction::PlanServerRestore { .. }
            | ServerTaskAction::LoadServerRestoreStatus(_)
            | ServerTaskAction::ApplyServerRestore { .. }
            | ServerTaskAction::RollbackServerRestore { .. } => Some(ServerTaskKind::Backup),
            ServerTaskAction::LoadServerSessions(_)
            | ServerTaskAction::PlanServerSession { .. }
            | ServerTaskAction::ApplyServerSession { .. } => Some(ServerTaskKind::Session),
            ServerTaskAction::PlanConsumerRecovery(_)
            | ServerTaskAction::ApplyConsumerRecovery { .. } => {
                Some(ServerTaskKind::ConsumerRecovery)
            }
            ServerTaskAction::PlanNativeInstall(_)
            | ServerTaskAction::CheckNativeInstallation { .. }
            | ServerTaskAction::ValidateNativeInstallation(_)
            | ServerTaskAction::PlanNativeMaintenance { .. }
            | ServerTaskAction::ApplyNativeInstall(_) => Some(ServerTaskKind::NativeInstall),
            ServerTaskAction::PlanWorkshop(_)
            | ServerTaskAction::ApplyWorkshop(_)
            | ServerTaskAction::VerifyWorkshop(_) => Some(ServerTaskKind::Workshop),
            _ => None,
        };
        let stage_control = kind.map(|_| ContentStageControl::default());
        let selected = self.server_view.selected.clone();
        if matches!(
            &action,
            ServerTaskAction::ApplyServerRestore { .. }
                | ServerTaskAction::RollbackServerRestore { .. }
                | ServerTaskAction::LoadServerRestoreStatus(_)
        ) {
            self.server_view.backups.restore_plan = None;
            self.server_view.backups.restore_status = None;
            self.server_view.backups.restore_outcome = None;
        }
        if !matches!(&action, ServerTaskAction::AcquireContent(_)) {
            self.server_view.invalidate_content_acquisition_plan();
        }
        if matches!(
            &action,
            ServerTaskAction::PlanContentAcquisition { .. } | ServerTaskAction::AcquireContent(_)
        ) {
            self.server_view.acquisition_outcome = None;
        }
        if matches!(
            &action,
            ServerTaskAction::LoadContentSources
                | ServerTaskAction::ReloadContentBindings(_)
                | ServerTaskAction::PlanContent { .. }
                | ServerTaskAction::SaveContentBindings(_)
                | ServerTaskAction::Catalog(_)
        ) {
            self.server_view.invalidate_content_stage();
        }
        if !matches!(&action, ServerTaskAction::BuildDeploymentPreparation(_)) {
            self.server_view.preparation_plan = None;
        }
        if !matches!(&action, ServerTaskAction::ApplyReconciliation(_)) {
            self.server_view.reconciliation_plan = None;
        }
        if !matches!(
            &action,
            ServerTaskAction::ApplyConsumerRecovery { .. }
                | ServerTaskAction::PollInstanceMonitoring(_)
        ) {
            self.server_view.consumer_recovery_plan = None;
            self.server_view.consumer_recovery_confirmation = false;
        }
        if matches!(
            &action,
            ServerTaskAction::PlanReconciliation(_) | ServerTaskAction::ApplyReconciliation(_)
        ) {
            self.server_view.reconciliation_outcome = None;
        }
        if matches!(
            &action,
            ServerTaskAction::BuildDeploymentPreparation(_)
                | ServerTaskAction::VerifyDeploymentPreparation { .. }
                | ServerTaskAction::PlanDeploymentPreparation { .. }
                | ServerTaskAction::ListDeploymentPreparations(_)
        ) {
            self.server_view.preparation_report = None;
            self.server_view.status = None;
        }
        match &action {
            ServerTaskAction::PlanContentStage { .. }
            | ServerTaskAction::ApplyContentStage(_)
            | ServerTaskAction::VerifyContentStage { .. } => {
                self.server_view.content_stage_report = None;
            }
            ServerTaskAction::ListContentStages(_) => self.server_view.invalidate_content_stage(),
            _ => {}
        }
        self.server_view.invalidate_deployment_composition();
        if !matches!(&action, ServerTaskAction::CommitDeploymentRecord(_)) {
            self.server_view.invalidate_deployment_record_plan();
        }
        if matches!(
            &action,
            ServerTaskAction::PrepareDeploymentRecord { .. }
                | ServerTaskAction::CommitDeploymentRecord(_)
                | ServerTaskAction::ListDeploymentRecords(_)
                | ServerTaskAction::VerifyDeploymentRecord { .. }
                | ServerTaskAction::PlanDeploymentRecordSelection { .. }
                | ServerTaskAction::Catalog(_)
        ) {
            self.server_view
                .invalidate_deployment_record_selection_plan();
        }
        let worker_scope = scope.clone();
        let worker_stage_control = stage_control.clone();
        let repaint = self.repaint_ctx.clone();
        let restore_focus = if matches!(&action, ServerTaskAction::PollInstanceMonitoring(_)) {
            None
        } else {
            Some(match &action {
                ServerTaskAction::PlanServiceExport
                | ServerTaskAction::WriteServiceExport { .. } => "server-service-controls",
                ServerTaskAction::ReviewServerKeys { .. }
                | ServerTaskAction::ApplyServerKeys { .. } => "server-key-controls",
                ServerTaskAction::LoadMissionLibrary
                | ServerTaskAction::PlanMissionImport(_)
                | ServerTaskAction::PlanMissionRefresh(_)
                | ServerTaskAction::PlanMissionAcquisition { .. }
                | ServerTaskAction::ApplyMissionImport(_) => "server-mission-import-path",
                ServerTaskAction::PlanInstanceStart(_)
                | ServerTaskAction::InspectInstanceRuntime(_)
                | ServerTaskAction::ExportMonitoringSnapshot { .. }
                | ServerTaskAction::OpenInstanceRuntimeLogs(_)
                | ServerTaskAction::StartInstance(_)
                | ServerTaskAction::StopInstance { .. }
                | ServerTaskAction::RestartInstance { .. }
                | ServerTaskAction::StartHeadlessClients { .. }
                | ServerTaskAction::StopHeadlessClients { .. }
                | ServerTaskAction::RestartHeadlessClients { .. } => "server-lifecycle-controls",
                ServerTaskAction::PlanNativeActivation { .. }
                | ServerTaskAction::ApplyNativeActivation(_)
                | ServerTaskAction::InspectNativeActivation(_)
                | ServerTaskAction::PlanNativeRecovery(_)
                | ServerTaskAction::ResolveNativeActivation(_) => {
                    "server-deployment-activation-controls"
                }
                ServerTaskAction::Inspect(_)
                | ServerTaskAction::Catalog(Some(
                    DraftChange::Adopt { .. } | DraftChange::Bind { .. },
                )) => "server-installation-directory",
                ServerTaskAction::Catalog(_) => "server-hostname",
                ServerTaskAction::LoadContentSources
                | ServerTaskAction::ReloadContentBindings(_)
                | ServerTaskAction::PlanContent { .. }
                | ServerTaskAction::SaveContentBindings(_) => "server-content-controls",
                ServerTaskAction::PlanContentStage { .. }
                | ServerTaskAction::ApplyContentStage(_)
                | ServerTaskAction::VerifyContentStage { .. }
                | ServerTaskAction::ListContentStages(_) => "server-content-stage-controls",
                ServerTaskAction::PlanContentAcquisition { .. }
                | ServerTaskAction::AcquireContent(_) => "server-content-acquisition-controls",
                ServerTaskAction::LoadReforgerAddonLock(_)
                | ServerTaskAction::PlanReforgerAddonAcquisition { .. }
                | ServerTaskAction::AcquireReforgerAddons(_) => "server-reforger-addon-controls",
                ServerTaskAction::PlanDeploymentComposition(_) => {
                    "server-deployment-composition-controls"
                }
                ServerTaskAction::PrepareDeploymentRecord { .. }
                | ServerTaskAction::CommitDeploymentRecord(_)
                | ServerTaskAction::ListDeploymentRecords(_)
                | ServerTaskAction::VerifyDeploymentRecord { .. }
                | ServerTaskAction::PlanDeploymentRecordSelection { .. }
                | ServerTaskAction::SelectDeploymentRecord(_) => {
                    "server-deployment-record-controls"
                }
                ServerTaskAction::PlanPlayerPublication(_)
                | ServerTaskAction::ApplyPlayerPublication(_)
                | ServerTaskAction::VerifyPlayerPublication(_) => {
                    "server-player-publication-controls"
                }
                ServerTaskAction::PlanDeploymentPreparation { .. }
                | ServerTaskAction::BuildDeploymentPreparation(_)
                | ServerTaskAction::ListDeploymentPreparations(_)
                | ServerTaskAction::VerifyDeploymentPreparation { .. } => {
                    "server-deployment-preparation-controls"
                }
                ServerTaskAction::LoadContentConsumers
                | ServerTaskAction::LoadDeploymentPins(_)
                | ServerTaskAction::SetDeploymentPin { .. }
                | ServerTaskAction::PlanReconciliation(_)
                | ServerTaskAction::ApplyReconciliation(_) => "server-reconciliation-controls",
                ServerTaskAction::PlanServerBackup { .. }
                | ServerTaskAction::CreateServerBackup { .. }
                | ServerTaskAction::LoadServerBackups(_)
                | ServerTaskAction::VerifyServerBackup { .. }
                | ServerTaskAction::SetServerBackupPin { .. }
                | ServerTaskAction::SetServerBackupRetention { .. }
                | ServerTaskAction::PlanServerBackupPrune { .. }
                | ServerTaskAction::ApplyServerBackupPrune { .. }
                | ServerTaskAction::PlanServerRestore { .. }
                | ServerTaskAction::LoadServerRestoreStatus(_)
                | ServerTaskAction::ApplyServerRestore { .. }
                | ServerTaskAction::RollbackServerRestore { .. } => "server-backup-controls",
                ServerTaskAction::LoadServerSessions(_)
                | ServerTaskAction::PlanServerSession { .. }
                | ServerTaskAction::ApplyServerSession { .. } => "server-session-controls",
                ServerTaskAction::PlanNativeInstall(_)
                | ServerTaskAction::CheckNativeInstallation { .. }
                | ServerTaskAction::ValidateNativeInstallation(_)
                | ServerTaskAction::PlanNativeMaintenance { .. }
                | ServerTaskAction::ApplyNativeInstall(_) => "server-native-install-controls",
                ServerTaskAction::LoadWorkshop
                | ServerTaskAction::PlanWorkshop(_)
                | ServerTaskAction::ApplyWorkshop(_)
                | ServerTaskAction::VerifyWorkshop(_) => "server-workshop-controls",
                ServerTaskAction::ChangeMaintenance { .. }
                | ServerTaskAction::RunMaintenanceJob { .. }
                | ServerTaskAction::CancelMaintenanceOccurrence { .. } => {
                    "server-maintenance-controls"
                }
                ServerTaskAction::ConfigureRcon { .. }
                | ServerTaskAction::SetRconPassword { .. }
                | ServerTaskAction::TestRcon { .. }
                | ServerTaskAction::SendRconCommand { .. } => "server-rcon-controls",
                ServerTaskAction::SendArmaAdminCommand { .. } => "server-rcon-controls",
                ServerTaskAction::SendReforgerAdminCommand { .. } => "server-rcon-controls",
                _ => "server-configuration-controls",
            })
        };
        if matches!(
            &action,
            ServerTaskAction::LoadContentSources | ServerTaskAction::PlanContent { .. }
        ) {
            self.server_view.content_plan = None;
            self.server_view.content_bindings_plan = None;
        }
        if matches!(&action, ServerTaskAction::LoadContentSources) {
            self.server_view.content_sources = None;
            self.server_view.content_bindings.clear();
            self.server_view.saved_content_bindings = None;
            self.server_view.content_bindings_plan = None;
        }
        if matches!(&action, ServerTaskAction::ReloadContentBindings(_)) {
            self.server_view.content_sources = None;
            self.server_view.saved_content_bindings = None;
            self.server_view.content_bindings_plan = None;
            self.server_view.content_plan = None;
        }
        let worker = std::thread::Builder::new()
            .name("server-draft".into())
            .spawn(move || {
                let result = tasks::execute(
                    worker_scope,
                    action,
                    selected,
                    worker_stage_control.unwrap_or_default(),
                );
                Foxy::request_background_repaint(repaint.as_ref());
                result
            });
        match worker {
            Ok(worker) => {
                self.server_view.pending_focus =
                    stage_control.as_ref().and(kind.map(|kind| match kind {
                        ServerTaskKind::Acquisition => "server-content-acquisition-cancel",
                        ServerTaskKind::Lifecycle => "server-lifecycle-cancel",
                        ServerTaskKind::Mission => "server-mission-cancel",
                        ServerTaskKind::Keys => "server-key-cancel",
                        ServerTaskKind::ContentStage => "server-content-stage-cancel",
                        ServerTaskKind::Composition => "server-deployment-composition-cancel",
                        ServerTaskKind::DeploymentRecord => "server-deployment-record-cancel",
                        ServerTaskKind::PlayerPublication => "server-player-publication-cancel",
                        ServerTaskKind::Preparation => "server-deployment-preparation-cancel",
                        ServerTaskKind::Reconciliation => "server-reconciliation-cancel",
                        ServerTaskKind::Backup => "server-backup-cancel",
                        ServerTaskKind::Session => "server-session-cancel",
                        ServerTaskKind::ConsumerRecovery => "server-consumer-recovery-cancel",
                        ServerTaskKind::NativeInstall => "server-native-install-cancel",
                        ServerTaskKind::Workshop => "server-workshop-cancel",
                        ServerTaskKind::Activation => "server-deployment-activation-cancel",
                    }));
                if inspecting {
                    self.server_view.inspection = None;
                    self.server_view.inspection_build = None;
                }
                self.server_view.preflight = None;
                self.server_view.configuration_preview = None;
                self.server_view.configuration_plan = None;
                self.server_view.configuration_verification = None;
                self.server_view.task = Some(ServerTask {
                    scope,
                    passive: false,
                    worker,
                    restore_focus,
                    stage_control,
                    kind,
                    maintenance_poll: false,
                    rcon_poll: false,
                });
                self.server_view.status = None;
                self.server_view.error = None;
            }
            Err(_) => self.server_view.error = Some(ServerStoreError::Io),
        }
    }

    pub(crate) fn poll_server_draft_task(&mut self) {
        self.poll_server_steam_auth();
        if !self
            .server_view
            .task
            .as_ref()
            .is_some_and(|task| task.worker.is_finished())
        {
            return;
        }
        let task = self.server_view.task.take().unwrap();
        let active = spaces::active_game_space();
        let path = self
            .server_context_path()
            .or_else(db_process_lock::held_database_path);
        if !task.scope.matches(
            &active.space_id,
            &active.game_id,
            path.as_deref(),
            &task.scope.request_id,
        ) || task.scope.owner
            != self.mode_ownership.owner.as_ref().map(|owner| {
                (
                    owner.attachment.scope().owner_id.clone(),
                    owner.attachment.scope().generation,
                )
            })
        {
            let _ = task.worker.join();
            self.server_view.queued_action = None;
            return;
        }
        let mut result = task.worker.join().unwrap_or(Err(ServerStoreError::Io));
        if task.maintenance_poll
            && let Err(error) = &result
        {
            self.server_view.maintenance.load_error = Some(error.to_string());
        }
        if task.restore_focus == Some("server-maintenance-controls") {
            self.server_view.maintenance.action_error =
                result.as_ref().err().map(ToString::to_string);
        }
        if task.rcon_poll
            && let Err(error) = &result
        {
            self.server_view.rcon.load_error = Some(error.to_string());
        }
        if task.restore_focus == Some("server-rcon-controls") {
            self.server_view.rcon.action_error = result.as_ref().err().map(ToString::to_string);
            if result.is_err() {
                self.server_view.rcon.confirmation = None;
                self.server_view.arma_admin.confirmation = None;
                self.server_view.reforger_admin.confirmation = None;
                self.server_view.reforger_admin.action_error =
                    result.as_ref().err().map(ToString::to_string);
                self.server_view.arma_admin.action_error =
                    result.as_ref().err().map(ToString::to_string);
            }
        }
        let passive_status = task.passive.then_some(self.server_view.status).flatten();
        let passive_error = task.passive.then(|| self.server_view.error.clone());
        if task.kind == Some(ServerTaskKind::Lifecycle)
            && matches!(result, Err(ServerStoreError::Cancelled))
        {
            result = Err(ServerStoreError::Invalid(
                "Foreground operation cancelled. Refresh runtime status before retrying.".into(),
            ));
        }
        if matches!(result, Err(ServerStoreError::Ownership)) && task.scope.owner.is_some() {
            self.mode_ownership.error = Some("The foreground owner changed or disconnected. Retry ownership connection before continuing.".into());
            self.mode_ownership.snapshot = None;
        }
        let changed_definition = matches!(
            &result,
            Ok(tasks::ServerTaskResult {
                output: tasks::ServerTaskOutput::Catalog { saved: true, .. },
                ..
            }) | Ok(tasks::ServerTaskResult {
                output: tasks::ServerTaskOutput::NativeInstallOutcome(..),
                ..
            })
        );
        if self.server_view.finish(&task.scope, result) && changed_definition {
            self.clear_server_edit_buffers();
        }
        if task.passive {
            self.server_view.status = passive_status;
            self.server_view.error = passive_error.flatten();
        }
        if !task.passive {
            self.server_view.pending_focus = task.restore_focus;
        }
        self.needs_repaint = true;
        if task.passive {
            if self.close_requested_at.is_some() || self.pending_game_space_switch.is_some() {
                self.server_view.queued_action = None;
            } else if let Some(action) = self.server_view.queued_action.take() {
                self.start_server_task(*action);
            }
        }
    }

    pub(crate) fn render_server_content_stage_status(&mut self, ui: &mut egui::Ui) {
        let Some((control, kind)) = self
            .server_view
            .task
            .as_ref()
            .and_then(|task| task.stage_control.as_ref().zip(task.kind))
        else {
            return;
        };
        let control = control.clone();
        let progress = control.snapshot();
        let mut cancel_response = None;
        ui.horizontal_wrapped(|ui| {
            let phase_label = match (kind, progress.cancel_requested, progress.phase) {
                (ServerTaskKind::Acquisition, true, _) => {
                    "Cancelling content acquisition; waiting for cleanup..."
                }
                (ServerTaskKind::Acquisition, false, ContentStagePhase::Complete) => {
                    "Content acquisition complete."
                }
                (ServerTaskKind::Acquisition, false, ContentStagePhase::Failed) => {
                    "Content acquisition failed."
                }
                (ServerTaskKind::Acquisition, false, ContentStagePhase::Cancelled) => {
                    "Content acquisition cancelled."
                }
                (ServerTaskKind::Acquisition, false, _) => "Acquiring content...",
                (ServerTaskKind::Mission, true, _) => "Cancelling mission operation...",
                (ServerTaskKind::Keys, true, _) => "Cancelling key review...",
                (ServerTaskKind::Keys, false, _) => "Reviewing or saving public-key trust...",
                (ServerTaskKind::Mission, false, _) => "Reviewing or saving mission content...",
                (ServerTaskKind::Lifecycle, true, _) => {
                    "Cancelling foreground operation; waiting for the retained owner outcome..."
                }
                (ServerTaskKind::Lifecycle, false, _) => {
                    "Reviewing or changing the foreground server session..."
                }
                (ServerTaskKind::Activation, true, _) => {
                    "Cancelling native deployment; waiting for the owner outcome..."
                }
                (ServerTaskKind::Activation, false, _) => {
                    "Reviewing or applying native deployment..."
                }
                (ServerTaskKind::Composition, true, _) => {
                    "Cancelling deployment composition review..."
                }
                (ServerTaskKind::Composition, false, _) => "Reviewing deployment composition...",
                (ServerTaskKind::DeploymentRecord, true, _) => {
                    "Cancelling deployment record operation..."
                }
                (ServerTaskKind::DeploymentRecord, false, ContentStagePhase::Preparing) => {
                    "Preparing deployment record..."
                }
                (ServerTaskKind::DeploymentRecord, false, ContentStagePhase::Verifying) => {
                    "Verifying deployment archives..."
                }
                (ServerTaskKind::DeploymentRecord, false, ContentStagePhase::Copying) => {
                    "Saving deployment record..."
                }
                (ServerTaskKind::DeploymentRecord, false, ContentStagePhase::Checking) => {
                    "Checking deployment record..."
                }
                (ServerTaskKind::DeploymentRecord, false, ContentStagePhase::Complete) => {
                    "Deployment record operation complete."
                }
                (ServerTaskKind::DeploymentRecord, false, ContentStagePhase::Cancelled) => {
                    "Deployment record operation cancelled."
                }
                (ServerTaskKind::DeploymentRecord, false, ContentStagePhase::Failed) => {
                    "Deployment record operation failed."
                }
                (ServerTaskKind::Preparation, true, _) => "Cancelling deployment preparation...",
                (ServerTaskKind::Preparation, false, ContentStagePhase::Preparing) => {
                    "Planning deployment preparation..."
                }
                (ServerTaskKind::Preparation, false, ContentStagePhase::Verifying) => {
                    "Verifying prepared deployment..."
                }
                (ServerTaskKind::Preparation, false, ContentStagePhase::Copying) => {
                    "Copying deployment preparation..."
                }
                (ServerTaskKind::Preparation, false, ContentStagePhase::Checking) => {
                    "Checking deployment preparation..."
                }
                (ServerTaskKind::Preparation, false, ContentStagePhase::Complete) => {
                    "Deployment preparation complete."
                }
                (ServerTaskKind::Preparation, false, ContentStagePhase::Cancelled) => {
                    "Deployment preparation cancelled."
                }
                (ServerTaskKind::Preparation, false, ContentStagePhase::Failed) => {
                    "Deployment preparation failed."
                }
                (ServerTaskKind::Reconciliation, true, _) => {
                    "Cancelling deployment storage review..."
                }
                (ServerTaskKind::Reconciliation, false, _) => {
                    "Reviewing or reclaiming deployment storage..."
                }
                (ServerTaskKind::ConsumerRecovery, true, _) => "Cancelling consumer recovery...",
                (ServerTaskKind::ConsumerRecovery, false, _) => {
                    "Reviewing or retiring verified consumer records..."
                }
                (ServerTaskKind::NativeInstall, true, _) => "Cancelling native installation...",
                (ServerTaskKind::NativeInstall, false, ContentStagePhase::Preparing) => {
                    "Reviewing the Steam build..."
                }
                (ServerTaskKind::NativeInstall, false, ContentStagePhase::Copying) => {
                    "Downloading depot content..."
                }
                (ServerTaskKind::NativeInstall, false, ContentStagePhase::Verifying) => {
                    "Verifying installed files..."
                }
                (ServerTaskKind::NativeInstall, false, ContentStagePhase::Checking) => {
                    "Recording the managed installation..."
                }
                (ServerTaskKind::NativeInstall, false, ContentStagePhase::Complete) => {
                    "Native installation step complete."
                }
                (ServerTaskKind::NativeInstall, false, ContentStagePhase::Cancelled) => {
                    "Native installation cancelled."
                }
                (ServerTaskKind::NativeInstall, false, ContentStagePhase::Failed) => {
                    "Native installation failed."
                }
                (ServerTaskKind::Workshop, true, _) => "Cancelling Workshop operation...",
                (ServerTaskKind::Workshop, false, ContentStagePhase::Preparing) => {
                    "Reviewing Workshop items on Steam..."
                }
                (ServerTaskKind::Workshop, false, ContentStagePhase::Copying) => {
                    "Downloading Workshop content..."
                }
                (ServerTaskKind::Workshop, false, ContentStagePhase::Verifying) => {
                    "Verifying Workshop files..."
                }
                (ServerTaskKind::Workshop, false, ContentStagePhase::Checking) => {
                    "Pinning Workshop revisions..."
                }
                (ServerTaskKind::Workshop, false, ContentStagePhase::Complete) => {
                    "Workshop operation step complete."
                }
                (ServerTaskKind::Workshop, false, ContentStagePhase::Cancelled) => {
                    "Workshop operation cancelled."
                }
                (ServerTaskKind::Workshop, false, ContentStagePhase::Failed) => {
                    "Workshop operation failed."
                }
                (_, true, _) => "Cancelling local content stage...",
                (_, false, ContentStagePhase::Preparing) => "Preparing local content stage...",
                (_, false, ContentStagePhase::Verifying) => "Verifying local content...",
                (_, false, ContentStagePhase::Copying) => "Copying local content...",
                (_, false, ContentStagePhase::Checking) => "Checking staged content...",
                (_, false, ContentStagePhase::Complete) => "Local content stage complete.",
                (_, false, ContentStagePhase::Cancelled) => "Cancelling local content stage...",
                (_, false, ContentStagePhase::Failed) => "Local content staging failed.",
            };
            ui.label(self.t(phase_label));
            if kind == ServerTaskKind::Acquisition
                && let Some(profile) = &progress.source
            {
                ui.label(self.t_fmt("Source profile: {profile}", &[("profile", profile.clone())]));
            }
            if kind == ServerTaskKind::Mission && progress.bytes_done > 0 {
                ui.label(self.t_fmt(
                    "Mission bytes transferred: {bytes}",
                    &[("bytes", progress.bytes_done.to_string())],
                ));
            } else if matches!(
                kind,
                ServerTaskKind::NativeInstall | ServerTaskKind::Workshop
            ) && progress.total_files == 0
            {
                ui.label(self.t_fmt(
                    "Bytes: {bytes} of {total_bytes}",
                    &[
                        ("bytes", progress.bytes_done.to_string()),
                        ("total_bytes", progress.total_bytes.to_string()),
                    ],
                ));
            } else if !matches!(
                kind,
                ServerTaskKind::Lifecycle
                    | ServerTaskKind::Mission
                    | ServerTaskKind::Keys
                    | ServerTaskKind::Reconciliation
                    | ServerTaskKind::Backup
                    | ServerTaskKind::Session
            ) {
                ui.label(self.t_fmt(
                    "Files: {done} of {total}; bytes: {bytes} of {total_bytes}",
                    &[
                        ("done", progress.files_done.to_string()),
                        ("total", progress.total_files.to_string()),
                        ("bytes", progress.bytes_done.to_string()),
                        ("total_bytes", progress.total_bytes.to_string()),
                    ],
                ));
            }
            if !progress.cancel_requested {
                let cancel_label = match kind {
                    ServerTaskKind::Acquisition => "Cancel content acquisition",
                    ServerTaskKind::Lifecycle => "Cancel foreground operation",
                    ServerTaskKind::Mission => "Cancel mission operation",
                    ServerTaskKind::Keys => "Cancel key operation",
                    ServerTaskKind::ContentStage => "Cancel local content stage",
                    ServerTaskKind::Composition => "Cancel deployment review",
                    ServerTaskKind::DeploymentRecord => "Cancel deployment record operation",
                    ServerTaskKind::PlayerPublication => "Cancel player publication operation",
                    ServerTaskKind::Preparation => "Cancel deployment preparation",
                    ServerTaskKind::Reconciliation => "Cancel deployment storage review",
                    ServerTaskKind::Backup => "Cancel backup operation",
                    ServerTaskKind::Session => "Cancel session operation",
                    ServerTaskKind::ConsumerRecovery => "Cancel consumer recovery",
                    ServerTaskKind::NativeInstall => "Cancel native installation",
                    ServerTaskKind::Workshop => "Cancel Workshop operation",
                    ServerTaskKind::Activation => "Cancel native deployment",
                };
                let response = ui.button(self.t(cancel_label));
                if response.clicked() {
                    control.cancel();
                }
                cancel_response = Some(response);
            }
        });
        let cancel_focus = match kind {
            ServerTaskKind::Acquisition => "server-content-acquisition-cancel",
            ServerTaskKind::Lifecycle => "server-lifecycle-cancel",
            ServerTaskKind::Mission => "server-mission-cancel",
            ServerTaskKind::Keys => "server-key-cancel",
            ServerTaskKind::ContentStage => "server-content-stage-cancel",
            ServerTaskKind::Composition => "server-deployment-composition-cancel",
            ServerTaskKind::DeploymentRecord => "server-deployment-record-cancel",
            ServerTaskKind::PlayerPublication => "server-player-publication-cancel",
            ServerTaskKind::Preparation => "server-deployment-preparation-cancel",
            ServerTaskKind::Reconciliation => "server-reconciliation-cancel",
            ServerTaskKind::Backup => "server-backup-cancel",
            ServerTaskKind::Session => "server-session-cancel",
            ServerTaskKind::ConsumerRecovery => "server-consumer-recovery-cancel",
            ServerTaskKind::NativeInstall => "server-native-install-cancel",
            ServerTaskKind::Workshop => "server-workshop-cancel",
            ServerTaskKind::Activation => "server-deployment-activation-cancel",
        };
        if self.server_view.pending_focus == Some(cancel_focus) {
            if let Some(response) = cancel_response {
                if ui.clip_rect().intersects(response.rect) {
                    response.request_focus();
                    self.server_view.pending_focus = None;
                } else {
                    response.scroll_to_me(Some(egui::Align::Center));
                    ui.ctx().request_repaint();
                }
            } else {
                self.server_view.pending_focus = None;
            }
        }
    }
}
