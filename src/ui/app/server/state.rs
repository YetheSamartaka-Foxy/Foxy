use crate::core::server::{
    ConfigurationCommitPlan, ConfigurationPreview, ConfigurationRevisions,
    ConfigurationRollbackPlan, ConfigurationVerification, ContentAcquisitionOutcome,
    ContentAcquisitionPlan, ContentAcquisitionStatus, ContentBindingsPlan, ContentBindingsSnapshot,
    ContentConsumerReport, ContentDeploymentPlan, ContentSourceSnapshot, ContentStageList,
    ContentStagePlan, ContentStageReport, DeploymentActivationPlan, DeploymentActivationStatus,
    DeploymentCompositionPlan, DeploymentOperationOutcome, DeploymentPins, DeploymentPreflight,
    DeploymentPreparationList, DeploymentPreparationPlan, DeploymentPreparationReport,
    DeploymentRecordCommit, DeploymentRecordPlan, DeploymentRecordSelectionPlan,
    DeploymentRecordState, DeploymentRecordVerification, DeploymentResolutionReview,
    InstallationBuildObservation, InstallationId, NativeInstallOutcome, NativeInstallPlan,
    NativeInstallStatus, NativeInstallationCheck, NativeInstallationValidation,
    ReconciliationOutcome, ReconciliationPlan, ServerAddonBinding, ServerCatalog,
    ServerInstallation, ServerInstance, ServerInstanceId, ServerStoreError, WorkshopBatchStatus,
    WorkshopOutcome, WorkshopPlan, WorkshopSnapshot, WorkshopVerification,
};
use std::time::Instant;

use super::tasks::ServerTaskAction;
use super::tasks::{ServerTask, ServerTaskOutput, ServerTaskResult, ServerTaskScope};

#[derive(Clone)]
pub(crate) struct ServerEditor {
    pub baseline: ServerInstance,
    pub draft: ServerInstance,
    pub mission_keys: Vec<u64>,
    pub addon_keys: Vec<u64>,
    pub next_row_key: u64,
    pub invalid_input: bool,
}

impl ServerEditor {
    pub fn new(instance: ServerInstance) -> Self {
        let (missions, addons) = match &instance.definition {
            crate::core::server::ServerDefinition::Arma3(settings) => {
                (settings.mission_cycle.len(), 0)
            }
            crate::core::server::ServerDefinition::Reforger(settings) => (0, settings.addons.len()),
        };
        Self {
            baseline: instance.clone(),
            draft: instance,
            mission_keys: (0..missions as u64).collect(),
            addon_keys: (0..addons as u64).collect(),
            next_row_key: (missions + addons) as u64,
            invalid_input: false,
        }
    }

    pub fn dirty(&self) -> bool {
        self.invalid_input || self.baseline != self.draft
    }

    pub fn field_scope(&self) -> egui::Id {
        egui::Id::unique((
            "server-fields",
            &self.draft.game_space_id,
            self.draft.id.as_str(),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ServerDialog {
    AcquireRepositoryContent,
    RollbackConfiguration,
    SelectDeploymentRecord,
    ReclaimDeploymentStorage,
    PruneServerBackups,
    RestoreServerBackup,
    RollbackServerRestore,
    NativeInstall,
    AcquireWorkshop,
    Rename,
    Clone,
    Remove,
    DiscardAndClose,
}

#[derive(Default)]
pub(crate) struct ServerViewState {
    pub page: super::ServerPage,
    pub profile_editor: super::ServerProfileEditor,
    pub instance_filter: String,
    pub pending_sidebar_selection: Option<ServerInstanceId>,
    pub focus_add_name: bool,
    pub arma_admin: super::arma_admin::ArmaAdminViewState,
    pub reforger_admin: super::reforger_admin::ReforgerAdminViewState,
    pub steam_auth: super::steam_auth::ServerSteamState,
    pub key_review: Option<Box<crate::core::server::ServerKeyReview>>,
    pub key_selection: std::collections::BTreeSet<String>,
    pub key_operator_files: Vec<std::path::PathBuf>,
    pub key_controls_focus: Option<egui::Id>,
    pub mission_library: Option<Box<crate::core::server::MissionLibrarySnapshot>>,
    pub mission_import_path: String,
    pub mission_import_plan: Option<Box<crate::core::server::MissionImportPlan>>,
    pub mission_acquire_url: String,
    pub mission_acquire_template: String,
    pub mission_acquire_hash: String,
    pub configuration_import_plan: Option<Box<crate::core::server::ConfigurationImportPlan>>,
    pub instance_pack_import_plan: Option<Box<crate::core::server::InstancePackImportPlan>>,
    pub import_server_path: String,
    pub import_basic_path: String,
    pub import_profile_path: String,
    pub import_managed_server: bool,
    pub import_managed_basic: bool,
    pub import_managed_profile: bool,
    pub import_paths_reviewed: bool,
    pub import_pack_path: String,
    pub import_pack_name: String,
    pub import_pack_paths_reviewed: bool,
    pub export_pack_sanitized: bool,
    pub reforger_import_path: String,
    pub reforger_import_review: Option<Box<crate::core::server::ReforgerImportReview>>,
    pub reforger_export_path: String,
    pub scenario_log_path: String,
    pub scenario_discovery: Option<Box<crate::core::server::ScenarioDiscovery>>,
    pub lifecycle_plan: Option<Box<crate::core::server::InstanceStartPlan>>,
    pub lifecycle_status: Option<crate::core::server::InstanceRuntimeStatus>,
    pub lifecycle_monitoring: Option<crate::core::server::InstanceMonitoring>,
    pub lifecycle_outcome: Option<crate::core::server::RuntimeOperationOutcome>,
    pub consumer_recovery_plan: Option<crate::core::server::ConsumerRecoveryPlan>,
    pub consumer_recovery_outcome: Option<crate::core::server::ConsumerRecoveryOutcome>,
    pub consumer_recovery_confirmation: bool,
    pub maintenance: super::maintenance::MaintenanceViewState,
    pub rcon: super::rcon::RconViewState,
    pub monitoring_search: String,
    pub monitoring_paused: bool,
    pub monitoring_auto_scroll: bool,
    pub monitoring_last_refresh: Option<Instant>,
    pub monitoring_last_received: Option<Instant>,
    pub lifecycle_allow_force: bool,
    pub activation_plan: Option<Box<DeploymentActivationPlan>>,
    pub activation_status: Option<DeploymentActivationStatus>,
    pub activation_resolution: Option<Box<DeploymentResolutionReview>>,
    pub activation_outcome: Option<DeploymentOperationOutcome>,
    pub pending_focus: Option<&'static str>,
    pub configuration_plan: Option<Box<ConfigurationCommitPlan>>,
    pub configuration_revisions: Option<ConfigurationRevisions>,
    pub configuration_choice: Option<String>,
    pub configuration_verification: Option<ConfigurationVerification>,
    pub configuration_rollback: Option<Box<ConfigurationRollbackPlan>>,
    pub configuration_preview: Option<ConfigurationPreview>,
    pub installation_directory: String,
    pub installation_choice: Option<InstallationId>,
    pub inspection: Option<ServerInstallation>,
    pub inspection_build: Option<Box<InstallationBuildObservation>>,
    pub preflight: Option<Box<DeploymentPreflight>>,
    pub migration_pending: bool,
    pub catalog: Option<ServerCatalog>,
    pub editor: Option<ServerEditor>,
    pub selected: Option<ServerInstanceId>,
    pub error: Option<ServerStoreError>,
    pub status: Option<&'static str>,
    pub new_name: String,
    pub dialog: Option<ServerDialog>,
    pub dialog_name: String,
    pub focus_dialog_cancel: bool,
    pub task: Option<ServerTask>,
    pub queued_action: Option<Box<ServerTaskAction>>,
    pub loaded: bool,
    pub show_client_progress: bool,
    pub content_sources: Option<ContentSourceSnapshot>,
    pub addon_candidates: Option<(std::path::PathBuf, Vec<String>)>,
    pub content_bindings: Vec<ServerAddonBinding>,
    pub content_plan: Option<Box<ContentDeploymentPlan>>,
    pub saved_content_bindings: Option<ContentBindingsSnapshot>,
    pub content_bindings_load_requested: bool,
    pub content_bindings_plan: Option<Box<ContentBindingsPlan>>,
    pub content_stage_plan: Option<Box<ContentStagePlan>>,
    pub content_stage_report: Option<ContentStageReport>,
    pub content_stage_list: Option<ContentStageList>,
    pub content_stage_selection: Option<String>,
    pub acquisition_plan: Option<Box<ContentAcquisitionPlan>>,
    pub acquisition_outcome: Option<Box<ContentAcquisitionOutcome>>,
    pub acquisition_seed: String,
    pub reforger_addon_scenario_provider: String,
    pub reforger_addon_plan: Option<Box<crate::core::server::ReforgerAddonAcquisitionPlan>>,
    pub reforger_addon_lock: Option<crate::core::server::ReforgerAddonLock>,
    pub reforger_addon_lock_loaded: bool,
    pub reforger_addon_confirmation: bool,
    pub deployment_composition: Option<Box<DeploymentCompositionPlan>>,
    pub deployment_record_plan: Option<Box<DeploymentRecordPlan>>,
    pub deployment_record_selection_plan: Option<Box<DeploymentRecordSelectionPlan>>,
    pub deployment_record_state: Option<DeploymentRecordState>,
    pub deployment_record_inspection_selection: Option<String>,
    pub deployment_record_verification: Option<DeploymentRecordVerification>,
    pub deployment_record_commit: Option<DeploymentRecordCommit>,
    pub preparation_plan: Option<Box<DeploymentPreparationPlan>>,
    pub preparation_list: Option<DeploymentPreparationList>,
    pub preparation_selection: Option<String>,
    pub preparation_report: Option<DeploymentPreparationReport>,
    pub content_consumers: Option<Box<ContentConsumerReport>>,
    pub deployment_pins: Option<(DeploymentPins, Vec<String>)>,
    pub reconciliation_plan: Option<Box<ReconciliationPlan>>,
    pub reconciliation_outcome: Option<ReconciliationOutcome>,
    pub backups: super::backups::ServerBackupViewState,
    pub sessions: super::sessions::ServerSessionsViewState,
    pub services: super::services::ServerServiceExportViewState,
    pub publication: super::publication::PlayerPublicationViewState,
    pub native_install_directory: String,
    pub native_install_branch: String,
    pub native_install_channel: crate::core::server::ContentChannel,
    pub native_install_anonymous: bool,
    pub native_install_plan: Option<Box<NativeInstallPlan>>,
    pub native_install_outcome: Option<Box<NativeInstallOutcome>>,
    pub native_install_check: Option<Box<NativeInstallationCheck>>,
    pub native_install_validation: Option<Box<NativeInstallationValidation>>,
    pub workshop_items_input: String,
    pub workshop_anonymous: bool,
    pub workshop_repair: bool,
    pub workshop_snapshot: Option<WorkshopSnapshot>,
    pub workshop_plan: Option<Box<WorkshopPlan>>,
    pub workshop_outcome: Option<Box<WorkshopOutcome>>,
    pub workshop_verification: Option<WorkshopVerification>,
}

impl ServerViewState {
    fn update_content_binding_source_review(&mut self, fingerprint: &Option<String>) {
        const STALE_SOURCES: &str = "Content source registrations changed since these bindings were reviewed. Review their identities and paths again.";
        let Some(saved) = &mut self.saved_content_bindings else {
            return;
        };
        let Some(document) = &saved.document else {
            return;
        };
        if document.content_sources_fingerprint.as_ref() != fingerprint.as_ref() {
            saved.review_current = false;
            if !saved.issues.contains(&STALE_SOURCES) {
                saved.issues.push(STALE_SOURCES);
            }
        } else {
            saved.issues.retain(|issue| *issue != STALE_SOURCES);
            saved.review_current = saved.issues.is_empty();
        }
    }

    pub fn open_dialog(&mut self, dialog: ServerDialog) {
        self.dialog = Some(dialog);
        self.focus_dialog_cancel = true;
    }

    fn update_monitoring(&mut self, mut monitoring: crate::core::server::InstanceMonitoring) {
        self.monitoring_last_received = Some(Instant::now());
        let changed_run = self.lifecycle_monitoring.as_ref().is_some_and(|previous| {
            previous.status.owner_id != monitoring.status.owner_id
                || previous.status.owner_generation != monitoring.status.owner_generation
                || previous.status.run_generation != monitoring.status.run_generation
        });
        if changed_run {
            self.monitoring_search.clear();
            self.monitoring_paused = false;
            self.monitoring_auto_scroll = true;
        } else if self.monitoring_paused
            && let (Some(previous), Some(current)) = (
                self.lifecycle_monitoring
                    .as_ref()
                    .and_then(|report| report.observation.as_ref()),
                monitoring.observation.as_mut(),
            )
        {
            current.logs = previous.logs.clone();
        }
        self.lifecycle_status = Some(monitoring.status.clone());
        self.lifecycle_monitoring = Some(monitoring);
    }

    pub fn preparation_plan_is_current(&self, plan: &DeploymentPreparationPlan) -> bool {
        let record = &plan.manifest.record;
        self.selected.as_ref() == Some(&record.instance_id)
            && self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.baseline.id == record.instance_id)
            && self.catalog.as_ref().is_some_and(|catalog| {
                catalog.game_space_id == record.game_space_id
                    && catalog.game_id == record.game_id
                    && catalog
                        .instances
                        .iter()
                        .any(|instance| instance.id == record.instance_id)
            })
            && self.deployment_record_state.as_ref().is_some_and(|state| {
                state.instance_id == record.instance_id
                    && state.game_space_id == record.game_space_id
                    && state.generation == plan.expected_generation
                    && state.selected.as_deref() == Some(plan.manifest.record_id.as_str())
                    && serde_json::to_vec(state).is_ok_and(|bytes| {
                        blake3::hash(&bytes).to_hex().as_str() == plan.inventory_fingerprint
                    })
            })
    }

    pub fn deployment_record_selection_plan_is_current(
        &self,
        plan: &DeploymentRecordSelectionPlan,
    ) -> bool {
        let catalog_matches = self.catalog.as_ref().is_some_and(|catalog| {
            catalog.game_space_id == plan.target.manifest.game_space_id
                && catalog.game_id == plan.target.manifest.game_id
                && catalog
                    .instances
                    .iter()
                    .any(|instance| instance.id == plan.instance_id)
        });
        let inventory_matches = self.deployment_record_state.as_ref().is_some_and(|state| {
            deployment_record_selection_matches(
                state,
                &plan.instance_id,
                &plan.target.manifest.game_space_id,
                plan.expected_generation,
                plan.previous_record_id.as_deref(),
                &plan.target.record_id,
                &plan.inventory_fingerprint,
            )
        });
        catalog_matches
            && inventory_matches
            && self.selected.as_ref() == Some(&plan.instance_id)
            && self.deployment_record_inspection_selection.as_deref()
                == Some(&plan.target.record_id)
            && plan.target.manifest.instance_id == plan.instance_id
    }

    pub fn dirty(&self) -> bool {
        self.editor.as_ref().is_some_and(ServerEditor::dirty)
    }

    pub fn busy(&self) -> bool {
        self.task.is_some() || self.steam_auth.job.is_some()
    }

    pub fn controls_busy(&self) -> bool {
        self.steam_auth.job.is_some()
            || self.queued_action.is_some()
            || self.task.as_ref().is_some_and(|task| !task.passive)
    }

    pub fn queue_passive_action(&mut self, action: ServerTaskAction) -> bool {
        if self.queued_action.is_some() {
            return false;
        }
        self.queued_action = Some(Box::new(action));
        true
    }

    pub fn select(&mut self, id: &ServerInstanceId) -> bool {
        if self.dirty() || self.busy() {
            return false;
        }
        let Some(instance) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.instances.iter().find(|instance| &instance.id == id))
        else {
            return false;
        };
        self.editor = Some(ServerEditor::new(instance.clone()));
        self.key_review = None;
        self.key_selection.clear();
        self.key_operator_files.clear();
        self.key_controls_focus = None;
        self.mission_import_plan = None;
        self.configuration_import_plan = None;
        self.instance_pack_import_plan = None;
        self.import_paths_reviewed = false;
        self.import_pack_paths_reviewed = false;
        self.lifecycle_plan = None;
        self.lifecycle_status = None;
        self.lifecycle_monitoring = None;
        self.lifecycle_outcome = None;
        self.monitoring_search.clear();
        self.monitoring_paused = false;
        self.monitoring_auto_scroll = true;
        self.monitoring_last_refresh = None;
        self.lifecycle_allow_force = false;
        self.pending_focus = None;
        self.installation_choice = instance.installation_id.clone();
        self.inspection = None;
        self.inspection_build = None;
        self.preflight = None;
        self.configuration_preview = None;
        self.configuration_plan = None;
        self.configuration_revisions = None;
        self.configuration_choice = None;
        self.configuration_verification = None;
        self.configuration_rollback = None;
        self.content_sources = None;
        self.addon_candidates = None;
        self.content_bindings.clear();
        self.content_plan = None;
        self.saved_content_bindings = None;
        self.content_bindings_load_requested = false;
        self.content_bindings_plan = None;
        self.acquisition_plan = None;
        self.acquisition_outcome = None;
        self.acquisition_seed.clear();
        self.invalidate_reforger_addon_acquisition();
        self.reforger_addon_scenario_provider.clear();
        self.reforger_addon_lock = None;
        self.reforger_addon_lock_loaded = false;
        self.invalidate_content_stage();
        self.deployment_record_plan = None;
        self.invalidate_deployment_record_selection_plan();
        self.deployment_record_state = None;
        self.deployment_record_inspection_selection = None;
        self.deployment_record_verification = None;
        self.deployment_record_commit = None;
        self.invalidate_deployment_preparation();
        self.invalidate_reconciliation();
        self.backups.reset_for_instance();
        self.sessions.reset_for_instance();
        self.publication.reset();
        self.consumer_recovery_plan = None;
        self.consumer_recovery_outcome = None;
        self.consumer_recovery_confirmation = false;
        self.maintenance = Default::default();
        self.rcon = Default::default();
        self.arma_admin = Default::default();
        self.reforger_admin = Default::default();
        self.native_install_plan = None;
        self.native_install_outcome = None;
        self.native_install_check = None;
        self.native_install_validation = None;
        if self.dialog == Some(ServerDialog::NativeInstall) {
            self.dialog = None;
        }
        self.selected = Some(id.clone());
        self.error = None;
        true
    }

    pub fn discard(&mut self) {
        if !self.busy()
            && let Some(editor) = self.editor.take()
        {
            self.editor = Some(ServerEditor::new(editor.baseline));
            self.error = None;
        }
    }

    pub fn invalidate_content_stage(&mut self) {
        self.content_stage_plan = None;
        self.content_stage_report = None;
        self.content_stage_list = None;
        self.content_stage_selection = None;
        self.invalidate_content_acquisition_plan();
        self.deployment_composition = None;
    }

    pub fn invalidate_content_acquisition_plan(&mut self) {
        self.acquisition_plan = None;
        if self.dialog == Some(ServerDialog::AcquireRepositoryContent) {
            self.dialog = None;
        }
    }

    pub fn invalidate_reforger_addon_acquisition(&mut self) {
        self.reforger_addon_plan = None;
        self.reforger_addon_confirmation = false;
    }

    pub fn invalidate_deployment_composition(&mut self) {
        self.deployment_composition = None;
    }

    pub fn invalidate_deployment_record_plan(&mut self) {
        self.deployment_record_plan = None;
    }

    pub fn invalidate_deployment_preparation(&mut self) {
        self.activation_plan = None;
        self.activation_status = None;
        self.activation_resolution = None;
        self.activation_outcome = None;
        self.preparation_plan = None;
        self.preparation_list = None;
        self.preparation_selection = None;
        self.preparation_report = None;
    }

    pub fn invalidate_reconciliation(&mut self) {
        self.deployment_pins = None;
        self.reconciliation_plan = None;
        self.reconciliation_outcome = None;
        if self.dialog == Some(ServerDialog::ReclaimDeploymentStorage) {
            self.dialog = None;
        }
    }

    pub fn invalidate_deployment_record_selection_plan(&mut self) {
        self.deployment_record_selection_plan = None;
        if self.dialog == Some(ServerDialog::SelectDeploymentRecord) {
            self.dialog = None;
        }
    }

    pub fn finish(
        &mut self,
        scope: &ServerTaskScope,
        result: Result<ServerTaskResult, ServerStoreError>,
    ) -> bool {
        if result.as_ref().is_ok_and(|result| &result.scope != scope) {
            return false;
        }
        if let Ok(result) = &result {
            let runtime = match &result.output {
                ServerTaskOutput::InstanceMonitoring(monitoring) => Some(&monitoring.status),
                ServerTaskOutput::InstanceRuntimeOutcome(outcome) => Some(&outcome.status),
                _ => None,
            };
            if runtime.is_some_and(|status| {
                scope.owner.as_ref().is_none_or(|(owner, generation)| {
                    owner != &status.owner_id || *generation != status.owner_generation
                })
            }) {
                return false;
            }
            let target = match &result.output {
                ServerTaskOutput::ConfigurationImportPlan(plan) => Some(&plan.report.instance_id),
                ServerTaskOutput::InstanceStartPlan(plan) => Some(&plan.instance_id),
                ServerTaskOutput::SessionInventory(inventory)
                | ServerTaskOutput::SessionApplied(inventory) => Some(&inventory.instance_id),
                ServerTaskOutput::SessionPlan(plan) => Some(&plan.instance_id),
                ServerTaskOutput::ConsumerRecoveryPlan(plan) => Some(&plan.instance_id),
                ServerTaskOutput::ConsumerRecoveryOutcome(outcome) => Some(&outcome.instance_id),
                ServerTaskOutput::Maintenance(snapshot, _) => Some(&snapshot.instance_id),
                ServerTaskOutput::InstanceMonitoring(monitoring) => {
                    Some(&monitoring.status.instance_id)
                }
                ServerTaskOutput::InstanceRuntimeOutcome(outcome) => {
                    Some(&outcome.status.instance_id)
                }
                ServerTaskOutput::MonitoringSnapshotExported(instance_id) => Some(instance_id),
                ServerTaskOutput::InstanceRuntimeLogsOpened(instance_id) => Some(instance_id),
                ServerTaskOutput::NativeActivationPlan(plan) => Some(&plan.instance_id),
                ServerTaskOutput::NativeActivationStatus(status) => Some(&status.instance_id),
                ServerTaskOutput::NativeResolutionReview(review) => Some(&review.instance_id),
                ServerTaskOutput::NativeActivationOutcome(outcome) => Some(&outcome.instance_id),
                ServerTaskOutput::ContentAcquisitionPlan(plan) => Some(&plan.instance_id),
                ServerTaskOutput::ContentAcquisitionOutcome(outcome) => Some(&outcome.instance_id),
                ServerTaskOutput::ReforgerAddonLock { instance_id, .. }
                | ServerTaskOutput::ReforgerAddonsAcquired { instance_id, .. } => Some(instance_id),
                ServerTaskOutput::ReforgerAddonAcquisitionPlan(plan) => Some(&plan.instance_id),
                ServerTaskOutput::NativeInstallPlan(plan) => Some(&plan.instance_id),
                ServerTaskOutput::NativeInstallationCheck(check) => Some(&check.instance_id),
                ServerTaskOutput::NativeInstallationValidation(validation) => {
                    Some(&validation.instance_id)
                }
                _ => None,
            };
            if target.is_some() && target != self.selected.as_ref() {
                return false;
            }
            if let ServerTaskOutput::ReforgerAddonAcquisitionPlan(plan) = &result.output
                && !self.editor.as_ref().is_some_and(|editor| {
                    !editor.dirty()
                        && editor.baseline.id == plan.instance_id
                        && editor.baseline.revision == plan.expected_instance_revision
                })
            {
                return false;
            }
        }
        if let Ok(ServerTaskResult {
            output: ServerTaskOutput::DeploymentComposition(report),
            ..
        }) = &result
        {
            let current_instance = self.editor.as_ref().filter(|editor| !editor.dirty());
            let current_catalog_revision = self.catalog.as_ref().map(|catalog| catalog.revision);
            let current_sources = self
                .content_sources
                .as_ref()
                .and_then(|sources| sources.fingerprint.as_deref());
            let current_bindings = self
                .saved_content_bindings
                .as_ref()
                .and_then(|bindings| bindings.fingerprint.as_deref());
            let current_configuration_generation = self
                .configuration_revisions
                .as_ref()
                .filter(|revisions| revisions.instance_id == report.instance_id)
                .map(|revisions| revisions.generation);
            if !current_instance.is_some_and(|editor| {
                editor.baseline.id == report.instance_id
                    && editor.baseline.revision == report.instance_revision
            }) || Some(report.catalog_revision) != current_catalog_revision
                || self.content_stage_selection != report.content.requested_stage_id
                || self
                    .configuration_revisions
                    .as_ref()
                    .filter(|revisions| revisions.instance_id == report.instance_id)
                    .is_some_and(|revisions| revisions.active != report.configuration.revision_id)
                || current_configuration_generation
                    .is_some_and(|generation| generation != report.configuration.generation)
                || (self.content_sources.is_some()
                    && current_sources != report.content.current_sources_fingerprint.as_deref())
                || (self.saved_content_bindings.is_some()
                    && current_bindings != report.content.current_bindings.fingerprint.as_deref())
            {
                return false;
            }
        }
        if let Ok(ServerTaskResult {
            output: ServerTaskOutput::DeploymentRecordPlan(plan),
            ..
        }) = &result
        {
            let current_instance = self.editor.as_ref().filter(|editor| !editor.dirty());
            if !current_instance.is_some_and(|editor| {
                editor.baseline.id == plan.manifest.instance_id
                    && editor.baseline.revision == plan.manifest.instance_revision
            }) || self
                .catalog
                .as_ref()
                .is_none_or(|catalog| catalog.revision != plan.manifest.catalog_revision)
                || self.content_stage_selection.as_deref()
                    != Some(plan.manifest.content_stage_id.as_str())
                || self
                    .configuration_revisions
                    .as_ref()
                    .filter(|revisions| revisions.instance_id == plan.manifest.instance_id)
                    .is_some_and(|revisions| {
                        revisions.active.as_deref()
                            != Some(plan.manifest.configuration_revision.as_str())
                            || revisions.generation != plan.review.configuration.generation
                    })
                || self
                    .deployment_record_state
                    .as_ref()
                    .filter(|state| state.instance_id == plan.manifest.instance_id)
                    .is_some_and(|state| state.generation != plan.expected_generation)
                || self.content_sources.as_ref().is_some_and(|sources| {
                    sources.fingerprint != plan.review.content.current_sources_fingerprint
                })
                || self
                    .saved_content_bindings
                    .as_ref()
                    .is_some_and(|bindings| {
                        bindings.fingerprint != plan.review.content.current_bindings.fingerprint
                    })
            {
                return false;
            }
        }
        if let Ok(ServerTaskResult {
            output: ServerTaskOutput::DeploymentRecordSelectionPlan(plan),
            ..
        }) = &result
            && !self.deployment_record_selection_plan_is_current(plan)
        {
            return false;
        }
        if let Ok(ServerTaskResult {
            output: ServerTaskOutput::DeploymentPreparationPlan(plan),
            ..
        }) = &result
            && !self.preparation_plan_is_current(plan)
        {
            return false;
        }
        self.loaded = true;
        match result {
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerKeys(review),
                ..
            }) => {
                self.key_selection = review
                    .candidates
                    .iter()
                    .filter(|candidate| candidate.trusted || candidate.implicit_tested_base)
                    .map(|candidate| candidate.id.clone())
                    .collect();
                self.key_review = Some(review);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::MissionLibrary(snapshot),
                ..
            }) => {
                self.mission_library = Some(snapshot);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::MissionImportPlan(plan),
                ..
            }) => {
                self.mission_import_plan = Some(plan);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::MissionImported(snapshot),
                ..
            }) => {
                self.mission_library = Some(snapshot);
                self.mission_import_plan = None;
                self.error = None;
                self.status = Some(
                    "Mission library saved. Add a mission to the draft cycle before saving and staging.",
                );
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ConfigurationImportPlan(plan),
                ..
            }) => {
                self.configuration_import_plan = Some(plan);
                self.import_paths_reviewed = false;
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::InstancePackImportPlan(plan),
                ..
            }) => {
                self.instance_pack_import_plan = Some(plan);
                self.import_pack_paths_reviewed = false;
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::InstancePackExported,
                ..
            }) => {
                self.status = Some(
                    "Redacted instance pack exported. Installation, credentials and saves were excluded.",
                );
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServiceExportPlan(plan),
                ..
            }) => {
                self.services.ensure_space(&plan.game_space_id);
                self.services.plan = Some(*plan);
                self.services.exported = false;
                self.status =
                    Some("Service export review is ready; no definition was saved or registered.");
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServiceExportWritten,
                ..
            }) => {
                self.services.exported = true;
                self.status =
                    Some("Definition saved. Registration is separate; no server was started.");
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::InstanceStartPlan(plan),
                ..
            }) => {
                self.lifecycle_plan = Some(plan);
                self.lifecycle_outcome = None;
                self.error = None;
                self.status = Some(
                    "Committed runtime reviewed. Start requires the retained foreground owner.",
                );
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::InstanceMonitoring(monitoring),
                ..
            }) => {
                self.update_monitoring(monitoring);
                self.error = None;
                self.status =
                    Some("Foreground monitoring refreshed from the retained owner observation.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::MonitoringSnapshotExported(_),
                ..
            }) => {
                self.error = None;
                self.status = Some("Sanitized server monitoring snapshot exported.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::InstanceRuntimeLogsOpened(_),
                ..
            }) => {
                self.error = None;
                self.status = Some(
                    "Opened the selected server's raw runtime log folder. These logs are unsanitized; copy and export actions use sanitized data.",
                );
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::InstanceRuntimeOutcome(outcome),
                ..
            }) => {
                self.lifecycle_status = Some(outcome.status.clone());
                self.lifecycle_monitoring = None;
                self.monitoring_last_refresh = None;
                self.monitoring_search.clear();
                self.monitoring_paused = false;
                self.monitoring_auto_scroll = true;
                self.error = outcome
                    .message
                    .clone()
                    .filter(|_| !outcome.completed)
                    .map(ServerStoreError::Invalid);
                self.lifecycle_outcome = Some(outcome);
                self.lifecycle_allow_force = false;
                self.status =
                    Some("Foreground operation completed. Review the retained runtime outcome.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentSources(sources),
                ..
            }) => {
                self.invalidate_content_acquisition_plan();
                self.invalidate_content_stage();
                self.update_content_binding_source_review(&sources.fingerprint);
                self.content_sources = Some(sources);
                self.addon_candidates = None;
                self.content_plan = None;
                self.content_bindings_plan = None;
                self.error = None;
                self.status = Some("Server content sources loaded. Select profiles to preview.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentSourcesSaved(sources),
                ..
            }) => {
                self.profile_editor = super::ServerProfileEditor::default();
                self.invalidate_content_acquisition_plan();
                self.invalidate_content_stage();
                self.update_content_binding_source_review(&sources.fingerprint);
                self.content_sources = Some(sources);
                self.content_plan = None;
                self.content_bindings_plan = None;
                self.addon_candidates = None;
                self.content_consumers = None;
                self.error = None;
                self.status =
                    Some("Content profile registry saved. Existing server bindings were retained.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::AddonCandidates { folder, names },
                ..
            }) => {
                self.addon_candidates = Some((folder, names));
                self.error = None;
                self.status = Some("Local addon folders scanned.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentPlan(plan),
                ..
            }) => {
                self.content_plan = Some(plan);
                self.error = None;
                self.status = Some("Content deployment preview generated. No files were written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentBindings { sources, snapshot },
                ..
            }) => {
                self.invalidate_content_acquisition_plan();
                self.content_sources = Some(sources);
                self.content_bindings = snapshot.document.as_ref().map_or_else(Vec::new, |doc| {
                    doc.bindings
                        .iter()
                        .map(|entry| entry.binding.clone())
                        .collect()
                });
                self.saved_content_bindings = Some(snapshot);
                self.content_plan = None;
                self.content_bindings_plan = None;
                self.error = None;
                self.status =
                    Some("Saved content bindings loaded. Review and preview before saving.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentBindingsPlan(plan),
                ..
            }) => {
                self.content_plan = Some(Box::new(plan.preview.clone()));
                self.content_bindings_plan = Some(plan);
                self.error = None;
                self.status = Some("Content bindings preview generated. Review before saving.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentBindingsSaved(snapshot),
                ..
            }) => {
                self.invalidate_content_acquisition_plan();
                self.saved_content_bindings = Some(snapshot);
                self.content_bindings_plan = None;
                self.error = None;
                self.status = Some("Reviewed content bindings saved.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentStagePlan(plan),
                ..
            }) => {
                self.content_stage_report = None;
                self.content_stage_plan = Some(plan);
                self.error = None;
                self.status = Some("Verified local content plan is ready to stage.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentStageReport(report),
                ..
            }) => {
                self.content_stage_report = Some(report);
                self.content_stage_plan = None;
                self.error = None;
                self.status = Some("Local content stage verified.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentAcquisitionPlan(plan),
                ..
            }) => {
                self.acquisition_outcome = None;
                self.acquisition_plan = Some(plan);
                self.error = None;
                self.status =
                    Some("Repository content acquisition plan is ready for confirmation.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentAcquisitionOutcome(outcome),
                ..
            }) => {
                let completed = outcome.status == ContentAcquisitionStatus::Complete;
                if let Some(stage) = &outcome.stage {
                    self.content_stage_report = Some(stage.clone());
                    self.content_stage_selection = Some(stage.stage_id.clone());
                    self.content_stage_list = None;
                    self.content_stage_plan = None;
                    self.deployment_composition = None;
                    self.deployment_record_plan = None;
                    self.invalidate_deployment_record_selection_plan();
                    self.deployment_record_state = None;
                    self.deployment_record_inspection_selection = None;
                    self.deployment_record_verification = None;
                    self.deployment_record_commit = None;
                    self.preparation_plan = None;
                    self.preparation_list = None;
                    self.preparation_selection = None;
                    self.preparation_report = None;
                    self.invalidate_reconciliation();
                }
                self.acquisition_plan = None;
                self.acquisition_outcome = Some(outcome);
                self.error = None;
                self.dialog = None;
                self.status = Some(if completed {
                    "Repository content acquisition completed. The retained stage is inactive."
                } else if self
                    .acquisition_outcome
                    .as_ref()
                    .is_some_and(|outcome| outcome.status == ContentAcquisitionStatus::Cancelled)
                {
                    "Repository content acquisition was cancelled. Review the per-profile results."
                } else {
                    "Repository content acquisition failed. Review the per-profile results."
                });
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ReforgerAddonLock { lock, .. },
                ..
            }) => {
                self.reforger_addon_lock = lock;
                self.reforger_addon_lock_loaded = true;
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ReforgerAddonAcquisitionPlan(plan),
                ..
            }) => {
                self.reforger_addon_plan = Some(plan);
                self.reforger_addon_confirmation = false;
                self.error = None;
                self.status = Some("Reforger addon acquisition plan is ready for confirmation.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ReforgerAddonsAcquired { lock, .. },
                ..
            }) => {
                self.reforger_addon_lock = Some(lock);
                self.reforger_addon_lock_loaded = true;
                self.invalidate_reforger_addon_acquisition();
                self.invalidate_content_stage();
                self.error = None;
                self.status = Some("Reforger addon revisions were acquired and retained.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentStages(list),
                ..
            }) => {
                self.content_stage_list = Some(list);
                self.content_stage_selection = None;
                self.content_stage_plan = None;
                self.content_stage_report = None;
                self.error = None;
                self.status =
                    Some("Saved content stages loaded. Verify a stage before relying on it.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentComposition(report),
                ..
            }) => {
                self.deployment_composition = Some(report);
                self.error = None;
                self.status =
                    Some("Deployment composition review completed; no files were written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentRecordPlan(plan),
                ..
            }) => {
                self.deployment_record_plan = Some(plan);
                self.deployment_record_verification = None;
                self.deployment_record_commit = None;
                self.error = None;
                self.status = Some("Deployment record is ready for review.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentRecordSelectionPlan(plan),
                ..
            }) => {
                self.deployment_record_selection_plan = Some(plan);
                self.error = None;
                self.dialog = Some(ServerDialog::SelectDeploymentRecord);
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentRecordCommit(commit),
                ..
            }) => {
                if self
                    .deployment_record_state
                    .as_ref()
                    .is_some_and(|state| state.selected != commit.state.selected)
                {
                    self.publication.plan = None;
                    self.publication.outcome = None;
                }
                self.deployment_record_state = Some(commit.state.clone());
                self.deployment_record_inspection_selection = Some(commit.record.record_id.clone());
                self.deployment_record_verification = Some(commit.record.clone());
                self.deployment_record_commit = Some(commit);
                self.deployment_record_plan = None;
                self.deployment_record_selection_plan = None;
                self.error = None;
                self.status = Some("Deployment record saved as offline selection.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentRecordState(state),
                ..
            }) => {
                if self
                    .deployment_record_state
                    .as_ref()
                    .is_some_and(|previous| previous.selected != state.selected)
                {
                    self.publication.plan = None;
                    self.publication.outcome = None;
                }
                self.deployment_record_inspection_selection = None;
                self.deployment_record_state = Some(state);
                self.deployment_record_plan = None;
                self.invalidate_deployment_record_selection_plan();
                self.deployment_record_verification = None;
                self.deployment_record_commit = None;
                self.error = None;
                self.status = Some("Deployment record inventory loaded; records are not verified.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentRecordVerification(report),
                ..
            }) => {
                self.deployment_record_verification = Some(report);
                self.deployment_record_plan = None;
                self.error = None;
                self.status = Some("Deployment record verification completed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::PlayerPublicationPlan(plan),
                ..
            }) => {
                self.publication.plan = Some(*plan);
                self.publication.outcome = None;
                self.publication.arma_permission = false;
                self.error = None;
                self.status = Some("Player publication plan is ready for review.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::PlayerPublicationOutcome(outcome),
                ..
            }) => {
                self.publication.outcome = Some(outcome);
                self.error = None;
                self.status = Some("Player publication operation completed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentPreparationPlan(plan),
                ..
            }) => {
                self.preparation_plan = Some(plan);
                self.preparation_report = None;
                self.error = None;
                self.status = Some("Reviewed deployment preparation plan is ready.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentPreparationList(list),
                ..
            }) => {
                self.preparation_list = Some(list);
                self.preparation_selection = None;
                self.preparation_report = None;
                self.error = None;
                self.status =
                    Some("Prepared generations loaded; inventory is not integrity evidence.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentPreparationReport(report),
                ..
            }) => {
                self.preparation_report = Some(report);
                self.preparation_plan = None;
                self.error = None;
                self.status = Some("Prepared generation verified or built offline.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ContentConsumers(report),
                ..
            }) => {
                self.content_consumers = Some(report);
                self.error = None;
                self.status = Some("Shared content consumers loaded; nothing was changed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::DeploymentPins(pins, records),
                ..
            }) => {
                self.deployment_pins = Some((pins, records));
                self.error = None;
                self.status = Some("Deployment pins loaded. Pins never delete artifacts.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ReconciliationPlan(plan),
                ..
            }) => {
                self.reconciliation_plan = Some(plan);
                self.error = None;
                self.status = Some("Deployment storage review is ready; nothing was removed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::Maintenance(snapshot, notice),
                ..
            }) => {
                if let Some(notice) = notice {
                    if notice == super::maintenance::MaintenanceNotice::Saved {
                        self.maintenance.form = None;
                    }
                    self.maintenance.notice = Some(notice);
                    self.maintenance.confirmation = None;
                    self.error = None;
                }
                self.maintenance.load_error = None;
                self.maintenance.snapshot = Some(*snapshot);
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::Rcon(snapshot, notice, record),
                ..
            }) => {
                if let Some(notice) = notice {
                    match notice {
                        super::rcon::RconNotice::SettingsSaved
                        | super::rcon::RconNotice::Disabled => self.rcon.form = None,
                        super::rcon::RconNotice::PasswordStored => self.rcon.password = None,
                        super::rcon::RconNotice::TestReplied { .. } => {}
                    }
                    self.rcon.notice = Some(notice);
                    self.rcon.confirmation = None;
                    self.error = None;
                }
                if let Some(record) = record {
                    if self.arma_admin.confirmation.as_ref().is_some_and(|review| {
                        review.instance_id.as_str() == record.instance_id
                            && review.run_generation == record.run_generation
                            && review.wire_text == record.text
                    }) {
                        self.arma_admin.confirmation = None;
                    }
                    self.rcon.last_command = Some(*record);
                    self.rcon.confirmation = None;
                    self.rcon.command_text.clear();
                    self.rcon.notice = None;
                    self.error = None;
                }
                self.rcon.load_error = None;
                self.rcon.snapshot = Some(*snapshot);
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ConsumerRecoveryPlan(plan),
                ..
            }) => {
                self.consumer_recovery_plan = Some(*plan);
                self.consumer_recovery_confirmation = false;
                self.error = None;
                self.status = Some("Consumer recovery review is ready; no records were retired.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ConsumerRecoveryOutcome(outcome),
                ..
            }) => {
                self.consumer_recovery_outcome = Some(outcome);
                self.consumer_recovery_plan = None;
                self.consumer_recovery_confirmation = false;
                self.lifecycle_plan = None;
                self.error = None;
                self.status = Some(
                    "Consumer recovery finished. Review remaining records before starting a server.",
                );
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ReconciliationOutcome(outcome),
                ..
            }) => {
                self.reconciliation_outcome = Some(outcome);
                self.reconciliation_plan = None;
                self.invalidate_deployment_preparation();
                self.invalidate_content_stage();
                self.error = None;
                self.status = Some("Reviewed deployment artifacts were reclaimed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::Workshop(snapshot),
                ..
            }) => {
                self.workshop_snapshot = Some(snapshot);
                self.error = None;
                self.status = Some("Tracked Workshop items loaded.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::WorkshopPlan(plan),
                ..
            }) => {
                self.workshop_plan = Some(plan);
                self.workshop_outcome = None;
                self.error = None;
                self.status = Some("Workshop review is ready; nothing was downloaded or written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::WorkshopOutcome(outcome, snapshot),
                ..
            }) => {
                let status = outcome.status;
                self.workshop_outcome = Some(outcome);
                self.workshop_snapshot = Some(snapshot);
                self.workshop_plan = None;
                self.workshop_verification = None;
                self.content_sources = None;
                self.content_bindings_plan = None;
                self.content_plan = None;
                self.invalidate_content_stage();
                self.error = None;
                self.status = Some(match status {
                    WorkshopBatchStatus::Complete => {
                        "Every reviewed Workshop item was verified and pinned."
                    }
                    WorkshopBatchStatus::Partial => {
                        "Some Workshop items were pinned; others failed. Review the item results."
                    }
                    WorkshopBatchStatus::Failed => {
                        "No Workshop item was pinned. Review the item results."
                    }
                    WorkshopBatchStatus::Cancelled => {
                        "Workshop acquisition cancelled; finished items stay pinned."
                    }
                });
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::WorkshopVerification(report),
                ..
            }) => {
                self.workshop_verification = Some(report);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeInstallPlan(plan),
                ..
            }) => {
                self.native_install_plan = Some(plan);
                self.native_install_outcome = None;
                self.error = None;
                self.status =
                    Some("Native installation review is ready; nothing was downloaded or written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeInstallationCheck(check),
                ..
            }) => {
                self.native_install_check = Some(check);
                self.native_install_validation = None;
                self.error = None;
                self.status = Some("Managed installation update check completed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeInstallationValidation(validation),
                ..
            }) => {
                self.native_install_validation = Some(validation);
                self.error = None;
                self.status = Some("Managed installation file validation completed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeInstallOutcome(outcome, catalog),
                ..
            }) => {
                let instance = outcome.instance_id.clone();
                let status = outcome.status;
                self.catalog = Some(catalog);
                self.editor = None;
                self.selected = None;
                self.select(&instance);
                self.native_install_outcome = Some(outcome);
                self.error = None;
                self.status = Some(match status {
                    NativeInstallStatus::Installed => {
                        "Managed installation verified, published and bound."
                    }
                    NativeInstallStatus::PublishedUnbound => {
                        "Verified installation published but not bound."
                    }
                    NativeInstallStatus::Cancelled => {
                        "Native installation cancelled; the destination was not created."
                    }
                    NativeInstallStatus::Failed => {
                        "Native installation failed; the destination was not created."
                    }
                });
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ConfigurationPlan(plan),
                ..
            }) => {
                self.configuration_preview = Some(plan.preview.clone());
                self.configuration_plan = Some(plan);
                self.error = None;
                self.status = Some("Configuration commit prepared. Review it before saving.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ConfigurationRevisions(state, saved),
                ..
            }) => {
                self.configuration_choice = state.active.clone();
                self.configuration_revisions = Some(state);
                self.configuration_plan = None;
                self.configuration_verification = None;
                self.configuration_rollback = None;
                self.error = None;
                self.dialog = None;
                self.status = Some(if saved {
                    "Configuration revision selection saved; no server was started."
                } else {
                    "Configuration history loaded."
                });
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ConfigurationVerification(report),
                ..
            }) => {
                self.configuration_verification = Some(report);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ConfigurationRollbackPlan(plan),
                ..
            }) => {
                self.configuration_rollback = Some(plan);
                self.dialog = Some(ServerDialog::RollbackConfiguration);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::Configuration(preview),
                ..
            }) => {
                self.configuration_preview = Some(preview);
                self.error = None;
                self.status = Some("Configuration preview generated; no files were written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::Inspection(installation, build),
                ..
            }) => {
                self.inspection = Some(installation);
                self.inspection_build = Some(build);
                self.error = None;
                self.status = Some("Installation inspected; game files were not changed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::Preflight(preflight),
                ..
            }) => {
                self.preflight = Some(preflight);
                self.error = None;
                self.status = Some("Deployment preflight completed; no files were written.");
            }
            Ok(ServerTaskResult {
                output:
                    ServerTaskOutput::Catalog {
                        catalog,
                        selected,
                        saved,
                        migration_pending,
                    },
                ..
            }) => {
                self.catalog = Some(catalog);
                self.migration_pending = migration_pending;
                self.inspection = None;
                self.inspection_build = None;
                self.preflight = None;
                self.selected = None;
                self.editor = None;
                self.content_sources = None;
                self.addon_candidates = None;
                self.content_bindings.clear();
                self.content_plan = None;
                self.saved_content_bindings = None;
                self.content_bindings_load_requested = false;
                self.content_bindings_plan = None;
                self.invalidate_reforger_addon_acquisition();
                self.reforger_addon_lock = None;
                self.reforger_addon_lock_loaded = false;
                if let Some(selected) = selected {
                    self.select(&selected);
                    if saved && self.page == super::ServerPage::Add {
                        self.page = super::ServerPage::Setup;
                        self.new_name.clear();
                    }
                }
                self.status = Some(if saved {
                    "Server draft saved."
                } else {
                    "Server drafts loaded."
                });
                self.error = None;
                self.dialog = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeActivationPlan(plan),
                ..
            }) => {
                self.activation_plan = Some(plan);
                self.activation_resolution = None;
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeActivationStatus(status),
                ..
            }) => {
                self.activation_status = Some(status);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeResolutionReview(review),
                ..
            }) => {
                self.activation_resolution = Some(review);
                self.activation_plan = None;
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::NativeActivationOutcome(outcome),
                ..
            }) => {
                self.activation_outcome = Some(outcome);
                self.activation_status = None;
                self.activation_plan = None;
                self.activation_resolution = None;
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ReforgerImport(review),
                ..
            }) => {
                self.reforger_import_review = Some(review);
                self.error = None;
                self.status = Some("Reforger import reviewed. Secrets were removed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ReforgerExported,
                ..
            }) => {
                self.error = None;
                self.status = Some("Saved Reforger server.json exported.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ScenarioDiscovery(discovery),
                ..
            }) => {
                self.scenario_discovery = discovery;
                self.error = None;
                self.status =
                    Some("Scenario listing updated. The selected scenario was not changed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerRestorePlan(plan),
                ..
            }) => {
                self.backups.restore_plan = Some(plan);
                self.backups.restore_outcome = None;
                self.error = None;
                self.status = Some("Server restore review is ready; no files were written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::SessionInventory(inventory),
                ..
            })
            | Ok(ServerTaskResult {
                output: ServerTaskOutput::SessionApplied(inventory),
                ..
            }) => {
                use crate::core::server::ReforgerSessionIntent;
                self.sessions.mode = match &inventory.intent {
                    ReforgerSessionIntent::Configured => "configured",
                    ReforgerSessionIntent::Latest => "latest",
                    ReforgerSessionIntent::Selected { .. } => "selected",
                    ReforgerSessionIntent::New => "new",
                }
                .into();
                self.sessions.save_id = match &inventory.intent {
                    ReforgerSessionIntent::Selected { save_id } => save_id.clone(),
                    _ => String::new(),
                };
                self.sessions.inventory = Some(inventory);
                self.sessions.plan = None;
                self.sessions.confirmation = false;
                self.lifecycle_plan = None;
                self.error = None;
                self.status = Some("Session inventory and next-launch intent loaded.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::SessionPlan(plan),
                ..
            }) => {
                self.sessions.plan = Some(plan);
                self.sessions.confirmation = false;
                self.error = None;
                self.status = Some("Session intent review is ready; no files were written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerRestoreStatus(status),
                ..
            }) => {
                self.backups.restore_status = Some(status);
                self.error = None;
                self.status = Some("Server restore recovery status loaded.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerRestoreOutcome(outcome, listing, status),
                ..
            }) => {
                self.status = Some(if outcome.rolled_back {
                    "Interrupted server restore rolled back."
                } else {
                    "Server backup restored and verified."
                });
                self.backups.restore_outcome = Some(outcome);
                self.backups.restore_status = Some(status);
                self.backups.restore_plan = None;
                self.backups.plan = None;
                self.backups.prune_plan = None;
                self.backups.listing = Some(listing);
                self.error = None;
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerBackupPlan(plan),
                ..
            }) => {
                self.backups.plan = Some(plan);
                self.backups.outcome = None;
                self.error = None;
                self.status = Some("Server backup review is ready; no files were written.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerBackupOutcome(outcome, listing),
                ..
            }) => {
                self.backups.selection = Some(outcome.manifest.backup_id.clone());
                self.backups.outcome = Some(outcome);
                self.backups.plan = None;
                self.backups.prune_plan = None;
                self.backups.prune_outcome = None;
                self.backups.listing = Some(listing);
                self.error = None;
                self.status = Some("Verified server backup created.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerBackupListing(listing),
                ..
            }) => {
                self.backups.sync_retention_inputs(&listing);
                if self.backups.selection.as_deref().is_some_and(|selected| {
                    !listing
                        .backups
                        .iter()
                        .any(|item| item.backup_id == selected)
                }) {
                    self.backups.selection = None;
                }
                self.backups.listing = Some(listing);
                self.error = None;
                self.status = Some("Server backup inventory and retention loaded.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerBackupVerification(report),
                ..
            }) => {
                self.backups.verification = Some(report);
                self.error = None;
                self.status = Some("Server backup verification passed.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerBackupPrunePlan(plan),
                ..
            }) => {
                self.backups.prune_plan = Some(plan);
                self.backups.prune_outcome = None;
                self.error = None;
                self.status = Some("Server backup prune review is ready; nothing was deleted.");
            }
            Ok(ServerTaskResult {
                output: ServerTaskOutput::ServerBackupPruneOutcome(outcome, listing),
                ..
            }) => {
                self.backups.sync_retention_inputs(&listing);
                if self.backups.selection.as_deref().is_some_and(|selected| {
                    !listing
                        .backups
                        .iter()
                        .any(|item| item.backup_id == selected)
                }) {
                    self.backups.selection = None;
                }
                self.backups.prune_outcome = Some(outcome);
                self.backups.prune_plan = None;
                self.backups.listing = Some(listing);
                self.error = None;
                self.status = Some("Reviewed server backups were pruned.");
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
}

fn deployment_record_selection_matches(
    inventory: &DeploymentRecordState,
    plan_instance: &ServerInstanceId,
    target_space: &str,
    plan_generation: u64,
    plan_previous: Option<&str>,
    plan_target: &str,
    plan_fingerprint: &str,
) -> bool {
    &inventory.instance_id == plan_instance
        && inventory.game_space_id == target_space
        && inventory.generation == plan_generation
        && inventory.selected.as_deref() == plan_previous
        && inventory.records.iter().any(|record| record == plan_target)
        && serde_json::to_vec(inventory)
            .is_ok_and(|bytes| blake3::hash(&bytes).to_hex().as_str() == plan_fingerprint)
}

#[cfg(test)]
#[path = "state/tests.rs"]
mod tests;
