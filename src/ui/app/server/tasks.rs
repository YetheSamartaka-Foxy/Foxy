use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use crate::core::server::{
    ConfigurationCommitPlan, ConfigurationPreview, ConfigurationRevisions,
    ConfigurationRollbackPlan, ConfigurationVerification, ContentAcquisitionOutcome,
    ContentAcquisitionPlan, ContentBindingsPlan, ContentBindingsSnapshot, ContentConsumerReport,
    ContentDeploymentPlan, ContentPlanRequest, ContentSourceSnapshot, ContentSources,
    ContentStageControl, ContentStageList, ContentStagePlan, ContentStageReport,
    DeploymentActivationPlan, DeploymentActivationStatus, DeploymentCompositionPlan,
    DeploymentCompositionRequest, DeploymentOperationOutcome, DeploymentPins, DeploymentPreflight,
    DeploymentPreparationList, DeploymentPreparationPlan, DeploymentPreparationReport,
    DeploymentRecordCommit, DeploymentRecordPlan, DeploymentRecordSelectionPlan,
    DeploymentRecordState, DeploymentRecordVerification, DeploymentResolutionReview, DraftChange,
    InstallationBuildObservation, NativeInstallOutcome, NativeInstallPlan, NativeInstallRequest,
    NativeInstallationCheck, NativeInstallationValidation, NativeMaintenanceAction,
    PlayerPublicationOutcome, PlayerPublicationPlan, PlayerPublicationRequest,
    ReconciliationOutcome, ReconciliationPlan, ServerCatalog, ServerContext, ServerInstallation,
    ServerInstanceId, ServerStore, ServerStoreError, WorkshopOutcome, WorkshopPlan,
    WorkshopRequest, WorkshopSnapshot, WorkshopVerification,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServerTaskScope {
    pub root: PathBuf,
    pub space_id: String,
    pub game_id: String,
    pub request_id: String,
    pub owner: Option<(String, u64)>,
}

impl ServerTaskScope {
    pub fn database_path(&self) -> PathBuf {
        self.root
            .join("games")
            .join(&self.space_id)
            .join("database.db")
    }

    pub fn matches(
        &self,
        active_space: &str,
        active_game: &str,
        held_path: Option<&Path>,
        request: &str,
    ) -> bool {
        self.space_id == active_space
            && self.game_id == active_game
            && held_path.is_some_and(|path| path == self.database_path())
            && self.request_id == request
    }
}

pub(crate) struct ServerTaskResult {
    pub scope: ServerTaskScope,
    pub output: ServerTaskOutput,
}

pub(crate) enum ServerTaskAction {
    PlanServiceExport,
    WriteServiceExport {
        plan: crate::core::server::ServiceExportPlan,
        path: PathBuf,
    },
    ReviewServerKeys {
        id: ServerInstanceId,
        stage_id: Option<String>,
        operator_files: Vec<PathBuf>,
    },
    ApplyServerKeys {
        review: Box<crate::core::server::ServerKeyReview>,
        selected: Vec<String>,
    },
    LoadMissionLibrary,
    PlanMissionImport(PathBuf),
    PlanMissionRefresh(String),
    PlanMissionAcquisition {
        url: String,
        template: String,
        expected_blake3: String,
    },
    ApplyMissionImport(Box<crate::core::server::MissionImportPlan>),
    PlanConfigurationImport {
        id: ServerInstanceId,
        expected_revision: u64,
        inputs: Vec<crate::core::server::ConfigurationImportInput>,
    },
    ApplyConfigurationImport(Box<crate::core::server::ConfigurationImportPlan>),
    ExportInstancePack {
        id: ServerInstanceId,
        path: PathBuf,
        sanitized: bool,
    },
    ExportMonitoringSnapshot {
        snapshot: Box<crate::core::server::MonitoringSnapshot>,
        path: PathBuf,
    },
    PlanInstancePackImport {
        pack_path: PathBuf,
        name: String,
    },
    ApplyInstancePackImport(Box<crate::core::server::InstancePackImportPlan>),
    PlanInstanceStart(ServerInstanceId),
    InspectInstanceRuntime(ServerInstanceId),
    PollInstanceMonitoring(ServerInstanceId),
    OpenInstanceRuntimeLogs(ServerInstanceId),
    StartInstance(Box<crate::core::server::InstanceStartPlan>),
    StopInstance {
        id: ServerInstanceId,
        expected_run_generation: u64,
        force: bool,
        timeout_ms: u32,
    },
    RestartInstance {
        plan: Box<crate::core::server::InstanceStartPlan>,
        expected_run_generation: u64,
        force: bool,
        timeout_ms: u32,
    },
    StartHeadlessClients {
        id: ServerInstanceId,
        expected_run_generation: u64,
        client: Option<String>,
    },
    StopHeadlessClients {
        id: ServerInstanceId,
        expected_run_generation: u64,
        client: Option<String>,
        force: bool,
    },
    RestartHeadlessClients {
        id: ServerInstanceId,
        expected_run_generation: u64,
        client: Option<String>,
    },
    PlanNativeActivation {
        id: ServerInstanceId,
        preparation_id: String,
        expected_selection_generation: u64,
        expected_activation_generation: u64,
    },
    ApplyNativeActivation(Box<DeploymentActivationPlan>),
    InspectNativeActivation(ServerInstanceId),
    PlanNativeRecovery(ServerInstanceId),
    ResolveNativeActivation(Box<DeploymentResolutionReview>),
    PlanConfiguration(ServerInstanceId),
    CommitConfiguration(Box<ConfigurationCommitPlan>),
    ListConfigurations(ServerInstanceId),
    VerifyConfiguration {
        id: ServerInstanceId,
        revision: String,
    },
    PlanConfigurationRollback {
        id: ServerInstanceId,
        revision: String,
    },
    RollbackConfiguration(Box<ConfigurationRollbackPlan>),
    Render(ServerInstanceId),
    Catalog(Option<DraftChange>),
    Inspect(PathBuf),
    Preflight(ServerInstanceId),
    LoadContentSources,
    SaveContentSources {
        sources: ContentSources,
        expected_fingerprint: Option<String>,
    },
    ScanAddonFolder(PathBuf),
    ReloadContentBindings(ServerInstanceId),
    PlanContent {
        request: ContentPlanRequest,
        save_bindings: bool,
        expected_bindings_fingerprint: Option<String>,
    },
    SaveContentBindings(Box<ContentBindingsPlan>),
    PlanContentStage {
        id: ServerInstanceId,
        expected_bindings_fingerprint: String,
    },
    ApplyContentStage(Box<ContentStagePlan>),
    PlanContentAcquisition {
        id: ServerInstanceId,
        seed_stage: Option<String>,
    },
    AcquireContent(Box<ContentAcquisitionPlan>),
    LoadReforgerAddonLock(ServerInstanceId),
    PlanReforgerAddonAcquisition {
        id: ServerInstanceId,
        scenario_provider: Option<String>,
    },
    AcquireReforgerAddons(Box<crate::core::server::ReforgerAddonAcquisitionPlan>),
    VerifyContentStage {
        id: ServerInstanceId,
        stage_id: String,
    },
    ListContentStages(ServerInstanceId),
    PlanDeploymentComposition(DeploymentCompositionRequest),
    PrepareDeploymentRecord {
        request: DeploymentCompositionRequest,
        expected_selection_generation: Option<u64>,
    },
    CommitDeploymentRecord(Box<DeploymentRecordPlan>),
    ListDeploymentRecords(ServerInstanceId),
    VerifyDeploymentRecord {
        id: ServerInstanceId,
        record_id: String,
    },
    PlanDeploymentRecordSelection {
        id: ServerInstanceId,
        record_id: String,
        expected_generation: u64,
    },
    SelectDeploymentRecord(Box<DeploymentRecordSelectionPlan>),
    PlanPlayerPublication(PlayerPublicationRequest),
    ApplyPlayerPublication(Box<PlayerPublicationPlan>),
    VerifyPlayerPublication(Box<PlayerPublicationPlan>),
    PlanDeploymentPreparation {
        id: ServerInstanceId,
        expected_generation: u64,
    },
    BuildDeploymentPreparation(Box<DeploymentPreparationPlan>),
    ListDeploymentPreparations(ServerInstanceId),
    VerifyDeploymentPreparation {
        id: ServerInstanceId,
        preparation_id: String,
    },
    LoadContentConsumers,
    LoadDeploymentPins(ServerInstanceId),
    SetDeploymentPin {
        id: ServerInstanceId,
        record_id: String,
        pinned: bool,
        expected_generation: u64,
    },
    PlanReconciliation(ServerInstanceId),
    PlanConsumerRecovery(ServerInstanceId),
    ApplyConsumerRecovery {
        id: ServerInstanceId,
        expected_plan_fingerprint: String,
    },
    ApplyReconciliation(Box<ReconciliationPlan>),
    PlanNativeInstall(NativeInstallRequest),
    CheckNativeInstallation {
        id: ServerInstanceId,
        account: Option<String>,
    },
    ValidateNativeInstallation(ServerInstanceId),
    PlanNativeMaintenance {
        request: NativeInstallRequest,
        action: NativeMaintenanceAction,
    },
    ApplyNativeInstall(Box<NativeInstallPlan>),
    LoadWorkshop,
    PlanWorkshop(WorkshopRequest),
    ApplyWorkshop(Box<WorkshopPlan>),
    VerifyWorkshop(String),
    ReviewReforgerImport(PathBuf),
    ExportReforgerJson {
        id: ServerInstanceId,
        path: PathBuf,
    },
    ImportScenarioLog {
        id: ServerInstanceId,
        path: PathBuf,
    },
    LoadScenarioDiscovery(ServerInstanceId),
    LoadMaintenance(ServerInstanceId),
    ChangeMaintenance {
        id: ServerInstanceId,
        expected_revision: u64,
        change: crate::core::server::jobs::MaintenanceChange,
    },
    RunMaintenanceJob {
        id: ServerInstanceId,
        job_id: String,
        expected_policy_revision: u64,
    },
    CancelMaintenanceOccurrence {
        id: ServerInstanceId,
        occurrence_id: String,
    },
    LoadRcon(ServerInstanceId),
    ConfigureRcon {
        id: ServerInstanceId,
        expected_revision: u64,
        input: Option<crate::core::server::rcon::RconSettingsInput>,
    },
    SetRconPassword {
        id: ServerInstanceId,
        expected_revision: u64,
        password: crate::core::server::rcon::RconPassword,
    },
    TestRcon {
        id: ServerInstanceId,
        run_generation: u64,
    },
    SendRconCommand {
        id: ServerInstanceId,
        run_generation: u64,
        text: String,
    },
    SendArmaAdminCommand {
        id: ServerInstanceId,
        run_generation: u64,
        command: crate::core::server::rcon::arma3::ArmaAdminCommand,
    },
    SendReforgerAdminCommand {
        id: ServerInstanceId,
        run_generation: u64,
        command: crate::core::server::rcon::reforger::ReforgerAdminCommand,
        timeout_ms: u32,
    },
    PlanServerBackup {
        id: ServerInstanceId,
        mode: crate::core::server::BackupConsistencyMode,
    },
    CreateServerBackup {
        id: ServerInstanceId,
        mode: crate::core::server::BackupConsistencyMode,
        expected_plan_fingerprint: String,
    },
    LoadServerBackups(ServerInstanceId),
    VerifyServerBackup {
        id: ServerInstanceId,
        backup_id: String,
    },
    SetServerBackupPin {
        id: ServerInstanceId,
        backup_id: String,
        pinned: bool,
        expected_generation: u64,
    },
    SetServerBackupRetention {
        id: ServerInstanceId,
        policy: crate::core::server::BackupRetentionPolicy,
        expected_generation: u64,
    },
    PlanServerBackupPrune {
        id: ServerInstanceId,
        keep_last_override: Option<u16>,
    },
    ApplyServerBackupPrune {
        id: ServerInstanceId,
        expected_plan_fingerprint: String,
        keep_last_override: Option<u16>,
    },
    PlanServerRestore {
        id: ServerInstanceId,
        backup_id: String,
    },
    LoadServerRestoreStatus(ServerInstanceId),
    ApplyServerRestore {
        id: ServerInstanceId,
        backup_id: String,
        expected_plan_fingerprint: String,
    },
    RollbackServerRestore {
        id: ServerInstanceId,
        expected_journal_fingerprint: String,
    },
    LoadServerSessions(ServerInstanceId),
    PlanServerSession {
        id: ServerInstanceId,
        intent: crate::core::server::ReforgerSessionIntent,
        expected_generation: u64,
    },
    ApplyServerSession {
        plan: Box<crate::core::server::ReforgerSessionPlan>,
    },
}

pub(crate) enum ServerTaskOutput {
    ServiceExportPlan(Box<crate::core::server::ServiceExportPlan>),
    ServiceExportWritten,
    ServerKeys(Box<crate::core::server::ServerKeyReview>),
    MissionLibrary(Box<crate::core::server::MissionLibrarySnapshot>),
    MissionImportPlan(Box<crate::core::server::MissionImportPlan>),
    MissionImported(Box<crate::core::server::MissionLibrarySnapshot>),
    ConfigurationImportPlan(Box<crate::core::server::ConfigurationImportPlan>),
    InstancePackImportPlan(Box<crate::core::server::InstancePackImportPlan>),
    InstancePackExported,
    InstanceStartPlan(Box<crate::core::server::InstanceStartPlan>),
    InstanceMonitoring(crate::core::server::InstanceMonitoring),
    InstanceRuntimeLogsOpened(ServerInstanceId),
    MonitoringSnapshotExported(crate::core::server::ServerInstanceId),
    InstanceRuntimeOutcome(crate::core::server::RuntimeOperationOutcome),
    NativeActivationPlan(Box<DeploymentActivationPlan>),
    NativeActivationStatus(DeploymentActivationStatus),
    NativeActivationOutcome(DeploymentOperationOutcome),
    NativeResolutionReview(Box<DeploymentResolutionReview>),
    ConfigurationPlan(Box<ConfigurationCommitPlan>),
    ConfigurationRevisions(ConfigurationRevisions, bool),
    ConfigurationVerification(ConfigurationVerification),
    ConfigurationRollbackPlan(Box<ConfigurationRollbackPlan>),
    Configuration(ConfigurationPreview),
    Catalog {
        catalog: ServerCatalog,
        selected: Option<ServerInstanceId>,
        saved: bool,
        migration_pending: bool,
    },
    Inspection(ServerInstallation, Box<InstallationBuildObservation>),
    Preflight(Box<DeploymentPreflight>),
    ContentSources(ContentSourceSnapshot),
    ContentSourcesSaved(ContentSourceSnapshot),
    AddonCandidates {
        folder: PathBuf,
        names: Vec<String>,
    },
    ContentPlan(Box<ContentDeploymentPlan>),
    ContentBindings {
        sources: ContentSourceSnapshot,
        snapshot: ContentBindingsSnapshot,
    },
    ContentBindingsPlan(Box<ContentBindingsPlan>),
    ContentBindingsSaved(ContentBindingsSnapshot),
    ContentStagePlan(Box<ContentStagePlan>),
    ContentStageReport(ContentStageReport),
    ContentAcquisitionPlan(Box<ContentAcquisitionPlan>),
    ContentAcquisitionOutcome(Box<ContentAcquisitionOutcome>),
    ReforgerAddonLock {
        instance_id: ServerInstanceId,
        lock: Option<crate::core::server::ReforgerAddonLock>,
    },
    ReforgerAddonAcquisitionPlan(Box<crate::core::server::ReforgerAddonAcquisitionPlan>),
    ReforgerAddonsAcquired {
        instance_id: ServerInstanceId,
        lock: crate::core::server::ReforgerAddonLock,
    },
    ContentStages(ContentStageList),
    DeploymentComposition(Box<DeploymentCompositionPlan>),
    DeploymentRecordPlan(Box<DeploymentRecordPlan>),
    DeploymentRecordCommit(DeploymentRecordCommit),
    DeploymentRecordState(DeploymentRecordState),
    DeploymentRecordVerification(DeploymentRecordVerification),
    DeploymentRecordSelectionPlan(Box<DeploymentRecordSelectionPlan>),
    PlayerPublicationPlan(Box<PlayerPublicationPlan>),
    PlayerPublicationOutcome(PlayerPublicationOutcome),
    DeploymentPreparationPlan(Box<DeploymentPreparationPlan>),
    DeploymentPreparationList(DeploymentPreparationList),
    DeploymentPreparationReport(DeploymentPreparationReport),
    ContentConsumers(Box<ContentConsumerReport>),
    DeploymentPins(DeploymentPins, Vec<String>),
    ReconciliationPlan(Box<ReconciliationPlan>),
    ConsumerRecoveryPlan(Box<crate::core::server::ConsumerRecoveryPlan>),
    ConsumerRecoveryOutcome(crate::core::server::ConsumerRecoveryOutcome),
    ReconciliationOutcome(ReconciliationOutcome),
    NativeInstallPlan(Box<NativeInstallPlan>),
    NativeInstallationCheck(Box<NativeInstallationCheck>),
    NativeInstallationValidation(Box<NativeInstallationValidation>),
    NativeInstallOutcome(Box<NativeInstallOutcome>, ServerCatalog),
    Workshop(WorkshopSnapshot),
    WorkshopPlan(Box<WorkshopPlan>),
    WorkshopOutcome(Box<WorkshopOutcome>, WorkshopSnapshot),
    WorkshopVerification(WorkshopVerification),
    ReforgerImport(Box<crate::core::server::ReforgerImportReview>),
    ReforgerExported,
    ScenarioDiscovery(Option<Box<crate::core::server::ScenarioDiscovery>>),
    Maintenance(
        Box<super::maintenance::MaintenanceSnapshot>,
        Option<super::maintenance::MaintenanceNotice>,
    ),
    Rcon(
        Box<super::rcon::RconSnapshot>,
        Option<super::rcon::RconNotice>,
        Option<Box<crate::core::server::rcon::RconCommandRecord>>,
    ),
    ServerBackupPlan(Box<crate::core::server::ServerBackupPlan>),
    SessionInventory(Box<crate::core::server::ReforgerSessionInventory>),
    SessionPlan(Box<crate::core::server::ReforgerSessionPlan>),
    SessionApplied(Box<crate::core::server::ReforgerSessionInventory>),
    ServerBackupOutcome(
        Box<crate::core::server::ServerBackupOutcome>,
        Box<crate::core::server::ServerBackupListing>,
    ),
    ServerBackupListing(Box<crate::core::server::ServerBackupListing>),
    ServerBackupVerification(Box<crate::core::server::ServerBackupVerificationReport>),
    ServerBackupPrunePlan(Box<crate::core::server::BackupPrunePlan>),
    ServerBackupPruneOutcome(
        Box<crate::core::server::BackupPruneOutcome>,
        Box<crate::core::server::ServerBackupListing>,
    ),
    ServerRestorePlan(Box<crate::core::server::ServerRestorePlan>),
    ServerRestoreStatus(crate::core::server::ServerRestoreStatus),
    ServerRestoreOutcome(
        Box<crate::core::server::ServerRestoreOutcome>,
        Box<crate::core::server::ServerBackupListing>,
        crate::core::server::ServerRestoreStatus,
    ),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServerTaskKind {
    Keys,
    Mission,
    Lifecycle,
    Activation,
    ContentStage,
    Acquisition,
    Composition,
    DeploymentRecord,
    PlayerPublication,
    Preparation,
    Reconciliation,
    Backup,
    Session,
    ConsumerRecovery,
    NativeInstall,
    Workshop,
}

pub(crate) struct ServerTask {
    pub scope: ServerTaskScope,
    pub passive: bool,
    pub restore_focus: Option<&'static str>,
    pub worker: JoinHandle<Result<ServerTaskResult, ServerStoreError>>,
    pub stage_control: Option<ContentStageControl>,
    pub kind: Option<ServerTaskKind>,
    /// A background maintenance load, whose failure is shown in its section.
    pub maintenance_poll: bool,
    pub rcon_poll: bool,
}

pub(crate) fn execute(
    scope: ServerTaskScope,
    action: ServerTaskAction,
    selected: Option<ServerInstanceId>,
    stage_control: ContentStageControl,
) -> Result<ServerTaskResult, ServerStoreError> {
    let context = ServerContext::capture(&scope.root, Some(&scope.space_id))
        .map_err(ServerStoreError::Invalid)?;
    if context.game_id() != scope.game_id || context.database_path() != scope.database_path() {
        return Err(ServerStoreError::Invalid(
            "Selected game space changed before the draft operation".into(),
        ));
    }
    let store = ServerStore::new(context)?;
    let deployment_effect = matches!(
        &action,
        ServerTaskAction::ApplyNativeActivation(_)
            | ServerTaskAction::ResolveNativeActivation(_)
            | ServerTaskAction::PlanInstanceStart(_)
            | ServerTaskAction::InspectInstanceRuntime(_)
            | ServerTaskAction::PollInstanceMonitoring(_)
            | ServerTaskAction::OpenInstanceRuntimeLogs(_)
            | ServerTaskAction::ExportMonitoringSnapshot { .. }
            | ServerTaskAction::PlanServiceExport
            | ServerTaskAction::WriteServiceExport { .. }
            | ServerTaskAction::StartInstance(_)
            | ServerTaskAction::StopInstance { .. }
            | ServerTaskAction::RestartInstance { .. }
            | ServerTaskAction::StartHeadlessClients { .. }
            | ServerTaskAction::StopHeadlessClients { .. }
            | ServerTaskAction::RestartHeadlessClients { .. }
            | ServerTaskAction::LoadMaintenance(_)
            | ServerTaskAction::RunMaintenanceJob { .. }
            | ServerTaskAction::CancelMaintenanceOccurrence { .. }
            | ServerTaskAction::LoadRcon(_)
            | ServerTaskAction::TestRcon { .. }
            | ServerTaskAction::SendRconCommand { .. }
            | ServerTaskAction::SendArmaAdminCommand { .. }
            | ServerTaskAction::SendReforgerAdminCommand { .. }
            | ServerTaskAction::CreateServerBackup { .. }
            | ServerTaskAction::ApplyServerBackupPrune { .. }
            | ServerTaskAction::ApplyServerRestore { .. }
            | ServerTaskAction::RollbackServerRestore { .. }
            | ServerTaskAction::PlanPlayerPublication(_)
            | ServerTaskAction::ApplyPlayerPublication(_)
            | ServerTaskAction::VerifyPlayerPublication(_)
    );
    let _permit = if deployment_effect {
        None
    } else {
        crate::core::server::owner::permit(store.context()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                ServerStoreError::Invalid(
                "Another server metadata operation is running. Wait for it to finish and retry."
                    .into(),
            )
            } else {
                ServerStoreError::Ownership
            }
        })?
    };
    if scope.owner.is_some() && _permit.is_none() && !deployment_effect {
        return Err(ServerStoreError::Ownership);
    }
    let player_publication = matches!(
        &action,
        ServerTaskAction::PlanPlayerPublication(_)
            | ServerTaskAction::ApplyPlayerPublication(_)
            | ServerTaskAction::VerifyPlayerPublication(_)
    );
    if !player_publication && let Some((owner_id, generation)) = &scope.owner {
        let attachment = crate::core::server::owner::attach(store.context())
            .map_err(|_| ServerStoreError::Ownership)?
            .ok_or(ServerStoreError::Ownership)?;
        if attachment.scope().owner_id != *owner_id || attachment.scope().generation != *generation
        {
            return Err(ServerStoreError::Ownership);
        }
    }
    let output = match action {
        ServerTaskAction::ReviewServerKeys {
            id,
            stage_id,
            operator_files,
        } => ServerTaskOutput::ServerKeys(Box::new(store.review_server_keys(
            &id,
            stage_id.as_deref(),
            &operator_files,
            &stage_control,
        )?)),
        ServerTaskAction::ApplyServerKeys { review, selected } => {
            let instance = store.apply_server_key_review(&review, &selected, &stage_control)?;
            ServerTaskOutput::Catalog {
                catalog: store.load()?,
                selected: Some(instance.id),
                saved: true,
                migration_pending: false,
            }
        }
        ServerTaskAction::LoadMissionLibrary => {
            ServerTaskOutput::MissionLibrary(Box::new(store.mission_library(&stage_control)?))
        }
        ServerTaskAction::PlanMissionImport(path) => ServerTaskOutput::MissionImportPlan(Box::new(
            store.plan_mission_import(&path, &stage_control)?,
        )),
        ServerTaskAction::PlanMissionRefresh(id) => ServerTaskOutput::MissionImportPlan(Box::new(
            store.plan_mission_refresh(&id, &stage_control)?,
        )),
        ServerTaskAction::PlanMissionAcquisition {
            url,
            template,
            expected_blake3,
        } => ServerTaskOutput::MissionImportPlan(Box::new(store.plan_mission_acquisition(
            &url,
            &template,
            &expected_blake3,
            &stage_control,
        )?)),
        ServerTaskAction::ApplyMissionImport(plan) => {
            store.apply_mission_import(*plan, &stage_control)?;
            ServerTaskOutput::MissionImported(Box::new(store.mission_library(&stage_control)?))
        }
        ServerTaskAction::PlanConfigurationImport {
            id,
            expected_revision,
            inputs,
        } => ServerTaskOutput::ConfigurationImportPlan(Box::new(store.plan_configuration_import(
            &id,
            expected_revision,
            &inputs,
        )?)),
        ServerTaskAction::ApplyConfigurationImport(plan) => {
            let instance = store.apply_configuration_import(*plan)?;
            ServerTaskOutput::Catalog {
                catalog: store.load()?,
                selected: Some(instance.id),
                saved: true,
                migration_pending: false,
            }
        }
        ServerTaskAction::PlanServiceExport => {
            let executable = std::env::current_exe().map_err(|_| ServerStoreError::Io)?;
            ServerTaskOutput::ServiceExportPlan(Box::new(crate::core::server::plan_service_export(
                store.context(),
                &executable,
                crate::ui::app::Foxy::server_service_export_format(),
            )?))
        }
        ServerTaskAction::WriteServiceExport { plan, path } => {
            let executable = std::env::current_exe().map_err(|_| ServerStoreError::Io)?;
            let current = crate::core::server::plan_service_export(
                store.context(),
                &executable,
                plan.format,
            )?;
            if current.fingerprint != plan.fingerprint {
                return Err(ServerStoreError::Invalid(
                    "Service export review changed. Preview it again before saving.".into(),
                ));
            }
            crate::core::server::write_service_export(&plan, &path)?;
            ServerTaskOutput::ServiceExportWritten
        }
        ServerTaskAction::ExportInstancePack {
            id,
            path,
            sanitized,
        } => {
            let pack = store.export_instance_pack(&id)?;
            let pack = if sanitized { pack.sanitized()? } else { pack };
            pack.write_new(&path)?;
            ServerTaskOutput::InstancePackExported
        }
        ServerTaskAction::ExportMonitoringSnapshot { snapshot, path } => {
            let instance_id = snapshot.instance_id.clone();
            let bytes = serde_json::to_vec(&snapshot).map_err(|_| ServerStoreError::Corrupt)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        ServerStoreError::Invalid(
                            "The export destination already exists. Choose a new file.".into(),
                        )
                    } else {
                        ServerStoreError::Io
                    }
                })?;
            file.write_all(&bytes).map_err(|_| ServerStoreError::Io)?;
            file.sync_all().map_err(|_| ServerStoreError::Io)?;
            ServerTaskOutput::MonitoringSnapshotExported(instance_id)
        }
        ServerTaskAction::PlanInstancePackImport { pack_path, name } => {
            ServerTaskOutput::InstancePackImportPlan(Box::new(store.plan_instance_pack_import(
                crate::core::server::PortableInstancePack::read(&pack_path)?,
                &name,
            )?))
        }
        ServerTaskAction::ApplyInstancePackImport(plan) => {
            let instance = store.apply_instance_pack_import(*plan)?;
            ServerTaskOutput::Catalog {
                catalog: store.load()?,
                selected: Some(instance.id),
                saved: true,
                migration_pending: false,
            }
        }
        ServerTaskAction::PlanInstanceStart(id) => ServerTaskOutput::InstanceStartPlan(Box::new(
            store.plan_instance_start(&id, &stage_control)?,
        )),
        ServerTaskAction::InspectInstanceRuntime(id)
        | ServerTaskAction::PollInstanceMonitoring(id) => {
            let owner = crate::core::server::owner::attach(store.context())
                .map_err(|_| ServerStoreError::Ownership)?
                .ok_or(ServerStoreError::Ownership)?;
            if scope.owner.as_ref().is_none_or(|(id, generation)| {
                id != &owner.scope().owner_id || *generation != owner.scope().generation
            }) {
                return Err(ServerStoreError::Ownership);
            }
            ServerTaskOutput::InstanceMonitoring(
                owner
                    .instance_monitoring(&store, &id)
                    .map_err(|error| ServerStoreError::Invalid(error.to_string()))?,
            )
        }
        ServerTaskAction::OpenInstanceRuntimeLogs(id) => {
            let directory = store.runtime_log_directory(&id)?;
            open_runtime_log_directory(&directory)?;
            ServerTaskOutput::InstanceRuntimeLogsOpened(id)
        }
        ServerTaskAction::StartInstance(plan) => {
            let operation = foxy_supervisor::control::Operation::StartInstance {
                instance_id: plan.instance_id.as_str().into(),
                expected_activation_generation: plan.activation_generation,
                review_fingerprint: plan.fingerprint,
            };
            execute_lifecycle(&store, &scope, operation, &stage_control)?
        }
        ServerTaskAction::StartHeadlessClients {
            id,
            expected_run_generation,
            client,
        } => execute_lifecycle(
            &store,
            &scope,
            foxy_supervisor::control::Operation::StartHeadlessClients {
                instance_id: id.as_str().into(),
                expected_run_generation,
                headless_id: client,
            },
            &stage_control,
        )?,
        ServerTaskAction::StopHeadlessClients {
            id,
            expected_run_generation,
            client,
            force,
        } => execute_lifecycle(
            &store,
            &scope,
            foxy_supervisor::control::Operation::StopHeadlessClients {
                instance_id: id.as_str().into(),
                expected_run_generation,
                headless_id: client,
                force,
                timeout_ms: 30_000,
            },
            &stage_control,
        )?,
        ServerTaskAction::RestartHeadlessClients {
            id,
            expected_run_generation,
            client,
        } => execute_lifecycle(
            &store,
            &scope,
            foxy_supervisor::control::Operation::RestartHeadlessClients {
                instance_id: id.as_str().into(),
                expected_run_generation,
                headless_id: client,
                timeout_ms: 30_000,
            },
            &stage_control,
        )?,
        ServerTaskAction::StopInstance {
            id,
            expected_run_generation,
            force,
            timeout_ms,
        } => execute_lifecycle(
            &store,
            &scope,
            foxy_supervisor::control::Operation::StopInstance {
                instance_id: id.as_str().into(),
                expected_run_generation,
                force,
                timeout_ms,
            },
            &stage_control,
        )?,
        ServerTaskAction::RestartInstance {
            plan,
            expected_run_generation,
            force,
            timeout_ms,
        } => execute_lifecycle(
            &store,
            &scope,
            foxy_supervisor::control::Operation::RestartInstance {
                instance_id: plan.instance_id.as_str().into(),
                expected_run_generation,
                expected_activation_generation: plan.activation_generation,
                review_fingerprint: plan.fingerprint,
                force,
                timeout_ms,
            },
            &stage_control,
        )?,
        ServerTaskAction::PlanNativeActivation {
            id,
            preparation_id,
            expected_selection_generation,
            expected_activation_generation,
        } => ServerTaskOutput::NativeActivationPlan(Box::new(store.plan_deployment_activation(
            &id,
            &preparation_id,
            expected_selection_generation,
            expected_activation_generation,
            &stage_control,
        )?)),
        ServerTaskAction::InspectNativeActivation(id) => ServerTaskOutput::NativeActivationStatus(
            store.deployment_activation_status(&id, &stage_control)?,
        ),
        ServerTaskAction::PlanNativeRecovery(id) => ServerTaskOutput::NativeResolutionReview(
            Box::new(store.plan_deployment_resolution(&id, &stage_control)?),
        ),
        ServerTaskAction::ApplyNativeActivation(plan) => {
            let owner = crate::core::server::owner::attach(store.context())
                .map_err(|_| ServerStoreError::Ownership)?
                .ok_or(ServerStoreError::Ownership)?;
            if scope.owner.as_ref().is_none_or(|(id, generation)| {
                id != &owner.scope().owner_id || *generation != owner.scope().generation
            }) {
                return Err(ServerStoreError::Ownership);
            }
            let operation = foxy_supervisor::control::Operation::ActivateDeployment {
                instance_id: plan.instance_id.as_str().into(),
                preparation_id: plan.preparation_id,
                expected_selection_generation: plan.expected_selection_generation,
                expected_activation_generation: plan.expected_activation_generation,
                review_fingerprint: plan.fingerprint,
            };
            ServerTaskOutput::NativeActivationOutcome(
                owner
                    .run_deployment(operation, &stage_control)
                    .map_err(|error| {
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            ServerStoreError::DeploymentCancelled
                        } else {
                            ServerStoreError::Invalid(error.to_string())
                        }
                    })?,
            )
        }
        ServerTaskAction::ResolveNativeActivation(review) => {
            let owner = crate::core::server::owner::attach(store.context())
                .map_err(|_| ServerStoreError::Ownership)?
                .ok_or(ServerStoreError::Ownership)?;
            if scope.owner.as_ref().is_none_or(|(id, generation)| {
                id != &owner.scope().owner_id || *generation != owner.scope().generation
            }) {
                return Err(ServerStoreError::Ownership);
            }
            ServerTaskOutput::NativeActivationOutcome(
                owner
                    .run_deployment(
                        foxy_supervisor::control::Operation::ResolveDeployment {
                            instance_id: review.instance_id.as_str().into(),
                            review_fingerprint: review.fingerprint,
                        },
                        &stage_control,
                    )
                    .map_err(|error| {
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            ServerStoreError::DeploymentCancelled
                        } else {
                            ServerStoreError::Invalid(error.to_string())
                        }
                    })?,
            )
        }
        ServerTaskAction::PlanConfiguration(id) => {
            ServerTaskOutput::ConfigurationPlan(Box::new(store.plan_configuration_commit(&id)?))
        }
        ServerTaskAction::CommitConfiguration(plan) => {
            ServerTaskOutput::ConfigurationRevisions(store.apply_configuration_commit(*plan)?, true)
        }
        ServerTaskAction::ListConfigurations(id) => {
            ServerTaskOutput::ConfigurationRevisions(store.configuration_revisions(&id)?, false)
        }
        ServerTaskAction::VerifyConfiguration { id, revision } => {
            ServerTaskOutput::ConfigurationVerification(store.verify_configuration(&id, &revision)?)
        }
        ServerTaskAction::PlanConfigurationRollback { id, revision } => {
            ServerTaskOutput::ConfigurationRollbackPlan(Box::new(
                store.plan_configuration_rollback(&id, &revision)?,
            ))
        }
        ServerTaskAction::RollbackConfiguration(plan) => ServerTaskOutput::ConfigurationRevisions(
            store.apply_configuration_rollback(*plan)?,
            true,
        ),
        ServerTaskAction::Render(id) => {
            ServerTaskOutput::Configuration(store.configuration_preview(&id)?)
        }
        ServerTaskAction::Inspect(directory) => {
            let installation = store.inspect_installation(&directory)?;
            let build = store.inspect_installation_build(&installation)?;
            ServerTaskOutput::Inspection(installation, Box::new(build))
        }
        ServerTaskAction::Preflight(id) => {
            ServerTaskOutput::Preflight(Box::new(store.preflight(&id)?))
        }
        ServerTaskAction::LoadContentSources => {
            ServerTaskOutput::ContentSources(store.content_sources()?)
        }
        ServerTaskAction::SaveContentSources {
            sources,
            expected_fingerprint,
        } => {
            store.save_content_sources(&sources, expected_fingerprint.as_deref())?;
            ServerTaskOutput::ContentSourcesSaved(store.content_sources()?)
        }
        ServerTaskAction::ScanAddonFolder(folder) => ServerTaskOutput::AddonCandidates {
            names: ContentSources::addon_candidates(&folder).map_err(ServerStoreError::Invalid)?,
            folder,
        },
        ServerTaskAction::ReloadContentBindings(id) => {
            let sources = store.content_sources()?;
            let snapshot = store.content_bindings(&id)?;
            let confirmed_sources = store.content_sources()?;
            if sources.fingerprint != confirmed_sources.fingerprint {
                return Err(ServerStoreError::StaleRevision);
            }
            ServerTaskOutput::ContentBindings { sources, snapshot }
        }
        ServerTaskAction::PlanContent {
            request,
            save_bindings,
            expected_bindings_fingerprint,
        } => {
            if save_bindings {
                let plan = store
                    .plan_content_bindings(request, expected_bindings_fingerprint.as_deref())?;
                ServerTaskOutput::ContentBindingsPlan(Box::new(plan))
            } else {
                ServerTaskOutput::ContentPlan(Box::new(store.plan_content(request)?))
            }
        }
        ServerTaskAction::SaveContentBindings(plan) => {
            ServerTaskOutput::ContentBindingsSaved(store.apply_content_bindings(*plan)?)
        }
        ServerTaskAction::PlanContentStage {
            id,
            expected_bindings_fingerprint,
        } => ServerTaskOutput::ContentStagePlan(Box::new(store.plan_content_stage(
            &id,
            &expected_bindings_fingerprint,
            &stage_control,
        )?)),
        ServerTaskAction::ApplyContentStage(plan) => {
            ServerTaskOutput::ContentStageReport(store.apply_content_stage(*plan, &stage_control)?)
        }
        ServerTaskAction::PlanContentAcquisition { id, seed_stage } => {
            ServerTaskOutput::ContentAcquisitionPlan(Box::new(store.plan_content_acquisition(
                &id,
                seed_stage.as_deref(),
                &stage_control,
            )?))
        }
        ServerTaskAction::AcquireContent(plan) => ServerTaskOutput::ContentAcquisitionOutcome(
            Box::new(store.acquire_content(&plan, &stage_control)?),
        ),
        ServerTaskAction::LoadReforgerAddonLock(id) => ServerTaskOutput::ReforgerAddonLock {
            lock: store.selected_reforger_addon_lock(&id)?,
            instance_id: id,
        },
        ServerTaskAction::PlanReforgerAddonAcquisition {
            id,
            scenario_provider,
        } => ServerTaskOutput::ReforgerAddonAcquisitionPlan(Box::new(
            store.plan_reforger_addon_acquisition(
                &id,
                crate::core::server::ReforgerAddonChannel::Stable,
                scenario_provider.as_deref(),
            )?,
        )),
        ServerTaskAction::AcquireReforgerAddons(plan) => {
            let instance_id = plan.instance_id.clone();
            let lock = store.acquire_reforger_addons(*plan, &stage_control)?;
            ServerTaskOutput::ReforgerAddonsAcquired { instance_id, lock }
        }
        ServerTaskAction::VerifyContentStage { id, stage_id } => {
            ServerTaskOutput::ContentStageReport(store.verify_content_stage(
                &id,
                &stage_id,
                &stage_control,
            )?)
        }
        ServerTaskAction::ListContentStages(id) => {
            let stages = store.content_stages(&id)?;
            if stage_control.snapshot().cancel_requested {
                return Err(ServerStoreError::Cancelled);
            }
            ServerTaskOutput::ContentStages(stages)
        }
        ServerTaskAction::PlanDeploymentComposition(request) => {
            ServerTaskOutput::DeploymentComposition(Box::new(
                store.plan_deployment_composition(request, &stage_control)?,
            ))
        }
        ServerTaskAction::PrepareDeploymentRecord {
            request,
            expected_selection_generation,
        } => ServerTaskOutput::DeploymentRecordPlan(Box::new(store.plan_deployment_record(
            request,
            expected_selection_generation,
            &stage_control,
        )?)),
        ServerTaskAction::CommitDeploymentRecord(plan) => ServerTaskOutput::DeploymentRecordCommit(
            store.apply_deployment_record(*plan, &stage_control)?,
        ),
        ServerTaskAction::ListDeploymentRecords(id) => {
            ServerTaskOutput::DeploymentRecordState(store.deployment_records(&id)?)
        }
        ServerTaskAction::VerifyDeploymentRecord { id, record_id } => {
            ServerTaskOutput::DeploymentRecordVerification(store.verify_deployment_record(
                &id,
                &record_id,
                &stage_control,
            )?)
        }
        ServerTaskAction::PlanDeploymentRecordSelection {
            id,
            record_id,
            expected_generation,
        } => ServerTaskOutput::DeploymentRecordSelectionPlan(Box::new(
            store.plan_deployment_record_selection(
                &id,
                &record_id,
                expected_generation,
                &stage_control,
            )?,
        )),
        ServerTaskAction::SelectDeploymentRecord(plan) => ServerTaskOutput::DeploymentRecordCommit(
            store.apply_deployment_record_selection(*plan, &stage_control)?,
        ),
        ServerTaskAction::PlanPlayerPublication(request) => {
            ServerTaskOutput::PlayerPublicationPlan(Box::new(
                store.plan_player_publication(&request, &stage_control)?,
            ))
        }
        ServerTaskAction::ApplyPlayerPublication(plan) => {
            ServerTaskOutput::PlayerPublicationOutcome(
                store.apply_player_publication(&plan, &stage_control)?,
            )
        }
        ServerTaskAction::VerifyPlayerPublication(plan) => {
            ServerTaskOutput::PlayerPublicationOutcome(
                store.verify_player_publication(&plan, &stage_control)?,
            )
        }
        ServerTaskAction::PlanDeploymentPreparation {
            id,
            expected_generation,
        } => ServerTaskOutput::DeploymentPreparationPlan(Box::new(
            store.plan_deployment_preparation(&id, expected_generation, &stage_control)?,
        )),
        ServerTaskAction::BuildDeploymentPreparation(plan) => {
            ServerTaskOutput::DeploymentPreparationReport(
                store.apply_deployment_preparation(*plan, &stage_control)?,
            )
        }
        ServerTaskAction::ListDeploymentPreparations(id) => {
            ServerTaskOutput::DeploymentPreparationList(store.deployment_preparations(&id)?)
        }
        ServerTaskAction::VerifyDeploymentPreparation { id, preparation_id } => {
            ServerTaskOutput::DeploymentPreparationReport(store.verify_deployment_preparation(
                &id,
                &preparation_id,
                &stage_control,
            )?)
        }
        ServerTaskAction::LoadContentConsumers => {
            ServerTaskOutput::ContentConsumers(Box::new(store.content_consumers()?))
        }
        ServerTaskAction::LoadDeploymentPins(id) => ServerTaskOutput::DeploymentPins(
            store.deployment_pins(&id)?,
            store.deployment_records(&id)?.records,
        ),
        ServerTaskAction::SetDeploymentPin {
            id,
            record_id,
            pinned,
            expected_generation,
        } => {
            let pins = store.set_deployment_pin(&id, &record_id, pinned, expected_generation)?;
            ServerTaskOutput::DeploymentPins(pins, store.deployment_records(&id)?.records)
        }
        ServerTaskAction::PlanReconciliation(id) => ServerTaskOutput::ReconciliationPlan(Box::new(
            store.plan_reconciliation(&id, &stage_control)?,
        )),
        ServerTaskAction::PlanConsumerRecovery(id) => {
            ServerTaskOutput::ConsumerRecoveryPlan(Box::new(store.plan_consumer_recovery(&id)?))
        }
        ServerTaskAction::ApplyConsumerRecovery {
            id,
            expected_plan_fingerprint,
        } => ServerTaskOutput::ConsumerRecoveryOutcome(store.recover_consumers(
            &id,
            &expected_plan_fingerprint,
            &stage_control,
        )?),
        ServerTaskAction::ApplyReconciliation(plan) => ServerTaskOutput::ReconciliationOutcome(
            store.apply_reconciliation(&plan, &stage_control)?,
        ),
        ServerTaskAction::PlanNativeInstall(request) => ServerTaskOutput::NativeInstallPlan(
            Box::new(store.plan_native_install(request, &stage_control)?),
        ),
        ServerTaskAction::CheckNativeInstallation { id, account } => {
            ServerTaskOutput::NativeInstallationCheck(Box::new(store.check_native_installation(
                &id,
                account.as_deref(),
                &stage_control,
            )?))
        }
        ServerTaskAction::ValidateNativeInstallation(id) => {
            ServerTaskOutput::NativeInstallationValidation(Box::new(
                store.validate_native_installation(&id, &stage_control)?,
            ))
        }
        ServerTaskAction::PlanNativeMaintenance { request, action } => {
            ServerTaskOutput::NativeInstallPlan(Box::new(store.plan_native_maintenance(
                request,
                action,
                &stage_control,
            )?))
        }
        ServerTaskAction::ApplyNativeInstall(plan) => {
            let outcome = store.apply_native_install(&plan, &stage_control)?;
            ServerTaskOutput::NativeInstallOutcome(Box::new(outcome), store.load()?)
        }
        ServerTaskAction::LoadWorkshop => ServerTaskOutput::Workshop(store.workshop_items()?),
        ServerTaskAction::PlanWorkshop(request) => {
            ServerTaskOutput::WorkshopPlan(Box::new(store.plan_workshop(request, &stage_control)?))
        }
        ServerTaskAction::ApplyWorkshop(plan) => {
            let outcome = store.apply_workshop(&plan, &stage_control)?;
            ServerTaskOutput::WorkshopOutcome(Box::new(outcome), store.workshop_items()?)
        }
        ServerTaskAction::VerifyWorkshop(item) => ServerTaskOutput::WorkshopVerification(
            store.verify_workshop_item(&item, &stage_control)?,
        ),
        ServerTaskAction::ReviewReforgerImport(path) => {
            let bytes = crate::core::server::read_bounded(&path).map_err(|_| {
                ServerStoreError::Invalid("Cannot read Reforger server.json (maximum 4 MiB)".into())
            })?;
            ServerTaskOutput::ReforgerImport(Box::new(
                crate::core::server::import_server_json(&bytes)
                    .map_err(ServerStoreError::Invalid)?,
            ))
        }
        ServerTaskAction::ExportReforgerJson { id, path } => {
            let instance = store.show(&id)?;
            let crate::core::server::ServerDefinition::Reforger(settings) = &instance.definition
            else {
                return Err(ServerStoreError::UnsupportedGame);
            };
            crate::core::server::write_server_json(
                &path,
                &crate::core::server::render_server_json(settings),
            )
            .map_err(ServerStoreError::Invalid)?;
            ServerTaskOutput::ReforgerExported
        }
        ServerTaskAction::ImportScenarioLog { id, path } => {
            let output =
                crate::core::server::read_scenario_log(&path).map_err(ServerStoreError::Invalid)?;
            let listing = crate::core::server::parse_scenario_listing(&output)
                .map_err(ServerStoreError::Invalid)?;
            let discovery = store.write_scenario_discovery(&id, &listing)?;
            ServerTaskOutput::ScenarioDiscovery(Some(Box::new(discovery)))
        }
        ServerTaskAction::LoadMaintenance(id) => maintenance_output(&store, &id, None)?,
        ServerTaskAction::ChangeMaintenance {
            id,
            expected_revision,
            change,
        } => {
            let notice = if matches!(
                change,
                crate::core::server::jobs::MaintenanceChange::Remove { .. }
            ) {
                super::maintenance::MaintenanceNotice::Removed
            } else {
                super::maintenance::MaintenanceNotice::Saved
            };
            store
                .change_maintenance(&id, expected_revision, change)
                .map_err(|error| match error {
                    ServerStoreError::StaleRevision => ServerStoreError::Invalid(
                        "The maintenance jobs changed elsewhere since this was opened. Review them again.".into(),
                    ),
                    error => error,
                })?;
            maintenance_output(&store, &id, Some(notice))?
        }
        ServerTaskAction::RunMaintenanceJob {
            id,
            job_id,
            expected_policy_revision,
        } => maintenance_request(
            &store,
            &id,
            foxy_supervisor::control::Operation::RunMaintenanceJob {
                instance_id: id.as_str().into(),
                job_id,
                expected_policy_revision,
            },
        )?,
        ServerTaskAction::CancelMaintenanceOccurrence { id, occurrence_id } => maintenance_request(
            &store,
            &id,
            foxy_supervisor::control::Operation::CancelMaintenanceOccurrence {
                instance_id: id.as_str().into(),
                occurrence_id,
            },
        )?,
        ServerTaskAction::LoadRcon(id) => rcon_output(&store, &id, None, None)?,
        ServerTaskAction::ConfigureRcon {
            id,
            expected_revision,
            input,
        } => {
            let notice = if input.is_some() {
                super::rcon::RconNotice::SettingsSaved
            } else {
                super::rcon::RconNotice::Disabled
            };
            store
                .configure_rcon(&id, expected_revision, input)
                .map_err(stale_rcon)?;
            rcon_output(&store, &id, Some(notice), None)?
        }
        ServerTaskAction::SetRconPassword {
            id,
            expected_revision,
            password,
        } => {
            store
                .set_rcon_password(&id, expected_revision, &password)
                .map_err(stale_rcon)?;
            drop(password);
            rcon_output(
                &store,
                &id,
                Some(super::rcon::RconNotice::PasswordStored),
                None,
            )?
        }
        ServerTaskAction::TestRcon { id, run_generation } => {
            let owner = rcon_owner(&store)?;
            let reply = owner
                .rcon_test(&id, run_generation)
                .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
            if !reply.replied {
                return Err(ServerStoreError::Invalid(
                    reply.error.unwrap_or_else(|| "RCon did not answer.".into()),
                ));
            }
            rcon_output(
                &store,
                &id,
                Some(super::rcon::RconNotice::TestReplied {
                    round_trip_ms: reply.round_trip_ms.unwrap_or_default(),
                }),
                None,
            )?
        }
        ServerTaskAction::SendRconCommand {
            id,
            run_generation,
            text,
        } => {
            let owner = rcon_owner(&store)?;
            let reply = owner
                .rcon_command(&id, run_generation, &text, 10_000)
                .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
            let record = reply
                .command_id
                .as_deref()
                .and_then(|command_id| store.rcon_command(&id, command_id).ok().flatten());
            if record.is_none()
                && let Some(error) = reply.error
            {
                return Err(ServerStoreError::Invalid(error));
            }
            rcon_output(&store, &id, None, record)?
        }
        ServerTaskAction::SendArmaAdminCommand {
            id,
            run_generation,
            command,
        } => {
            let owner = rcon_owner(&store)?;
            if scope.owner.as_ref().is_none_or(|(id, generation)| {
                id != &owner.scope().owner_id || *generation != owner.scope().generation
            }) {
                return Err(ServerStoreError::Ownership);
            }
            let reply = owner
                .arma_admin_command(&id, run_generation, &command, 10_000)
                .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
            let record = reply
                .command_id
                .as_deref()
                .and_then(|command_id| store.rcon_command(&id, command_id).ok().flatten());
            if record.is_none()
                && let Some(error) = reply.error
            {
                return Err(ServerStoreError::Invalid(error));
            }
            rcon_output(&store, &id, None, record)?
        }
        ServerTaskAction::SendReforgerAdminCommand {
            id,
            run_generation,
            command,
            timeout_ms,
        } => {
            let owner = rcon_owner(&store)?;
            if scope.owner.as_ref().is_none_or(|(id, generation)| {
                id != &owner.scope().owner_id || *generation != owner.scope().generation
            }) {
                return Err(ServerStoreError::Ownership);
            }
            if command == crate::core::server::rcon::reforger::ReforgerAdminCommand::ReloadScenario
            {
                let outcome = owner
                    .run_instance(
                        foxy_supervisor::control::Operation::ReloadReforgerScenario {
                            instance_id: id.as_str().into(),
                            expected_run_generation: run_generation,
                            timeout_ms,
                        },
                        &stage_control,
                    )
                    .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
                ServerTaskOutput::InstanceRuntimeOutcome(outcome)
            } else {
                let reply = owner
                    .reforger_admin_command(&id, run_generation, &command, timeout_ms)
                    .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
                let record = reply
                    .command_id
                    .as_deref()
                    .and_then(|command_id| store.rcon_command(&id, command_id).ok().flatten());
                if record.is_none()
                    && let Some(error) = reply.error
                {
                    return Err(ServerStoreError::Invalid(error));
                }
                rcon_output(&store, &id, None, record)?
            }
        }
        ServerTaskAction::LoadScenarioDiscovery(id) => {
            ServerTaskOutput::ScenarioDiscovery(store.read_scenario_discovery(&id)?.map(Box::new))
        }
        ServerTaskAction::LoadServerSessions(id) => ServerTaskOutput::SessionInventory(Box::new(
            store.reforger_sessions(&id, &stage_control)?,
        )),
        ServerTaskAction::PlanServerSession {
            id,
            intent,
            expected_generation,
        } => ServerTaskOutput::SessionPlan(Box::new(store.plan_reforger_session(
            &id,
            intent,
            expected_generation,
            &stage_control,
        )?)),
        ServerTaskAction::ApplyServerSession { plan } => ServerTaskOutput::SessionApplied(
            Box::new(store.apply_reforger_session(*plan, &stage_control)?),
        ),
        ServerTaskAction::PlanServerBackup { id, mode } => ServerTaskOutput::ServerBackupPlan(
            Box::new(store.plan_server_backup(&id, mode, None, &stage_control)?),
        ),
        ServerTaskAction::CreateServerBackup {
            id,
            mode,
            expected_plan_fingerprint,
        } => {
            let outcome = crate::core::server::owner::create_server_backup(
                &store,
                &id,
                mode,
                Some(&expected_plan_fingerprint),
                true,
                &stage_control,
            )?;
            let listing = store.list_server_backups(&id)?;
            ServerTaskOutput::ServerBackupOutcome(Box::new(outcome), Box::new(listing))
        }
        ServerTaskAction::LoadServerBackups(id) => {
            ServerTaskOutput::ServerBackupListing(Box::new(store.list_server_backups(&id)?))
        }
        ServerTaskAction::VerifyServerBackup { id, backup_id } => {
            ServerTaskOutput::ServerBackupVerification(Box::new(store.verify_server_backup(
                &id,
                &backup_id,
                &stage_control,
            )?))
        }
        ServerTaskAction::SetServerBackupPin {
            id,
            backup_id,
            pinned,
            expected_generation,
        } => {
            store.set_backup_pin(&id, &backup_id, pinned, expected_generation)?;
            ServerTaskOutput::ServerBackupListing(Box::new(store.list_server_backups(&id)?))
        }
        ServerTaskAction::SetServerBackupRetention {
            id,
            policy,
            expected_generation,
        } => {
            store.set_backup_retention_policy(&id, policy, expected_generation)?;
            ServerTaskOutput::ServerBackupListing(Box::new(store.list_server_backups(&id)?))
        }
        ServerTaskAction::PlanServerBackupPrune {
            id,
            keep_last_override,
        } => ServerTaskOutput::ServerBackupPrunePlan(Box::new(store.plan_backup_prune(
            &id,
            keep_last_override,
            &stage_control,
        )?)),
        ServerTaskAction::ApplyServerBackupPrune {
            id,
            expected_plan_fingerprint,
            keep_last_override,
        } => {
            let outcome = crate::core::server::owner::prune_server_backups(
                &store,
                &id,
                &expected_plan_fingerprint,
                keep_last_override,
                &stage_control,
            )?;
            let listing = store.list_server_backups(&id)?;
            ServerTaskOutput::ServerBackupPruneOutcome(Box::new(outcome), Box::new(listing))
        }
        ServerTaskAction::PlanServerRestore { id, backup_id } => {
            ServerTaskOutput::ServerRestorePlan(Box::new(store.plan_server_restore(
                &id,
                &backup_id,
                None,
                &stage_control,
            )?))
        }
        ServerTaskAction::LoadServerRestoreStatus(id) => {
            ServerTaskOutput::ServerRestoreStatus(store.server_restore_status(&id)?)
        }
        ServerTaskAction::ApplyServerRestore {
            id,
            backup_id,
            expected_plan_fingerprint,
        } => {
            let outcome = crate::core::server::owner::restore_server_backup(
                &store,
                &id,
                &backup_id,
                &expected_plan_fingerprint,
                &stage_control,
            )?;
            let listing = store.list_server_backups(&id)?;
            let status = store.server_restore_status(&id)?;
            ServerTaskOutput::ServerRestoreOutcome(Box::new(outcome), Box::new(listing), status)
        }
        ServerTaskAction::RollbackServerRestore {
            id,
            expected_journal_fingerprint,
        } => {
            let outcome = crate::core::server::owner::rollback_server_restore(
                &store,
                &id,
                &expected_journal_fingerprint,
                &stage_control,
            )?;
            let listing = store.list_server_backups(&id)?;
            let status = store.server_restore_status(&id)?;
            ServerTaskOutput::ServerRestoreOutcome(Box::new(outcome), Box::new(listing), status)
        }
        ServerTaskAction::Catalog(Some(change)) => {
            let plan = store.plan(change)?;
            let selected = (!plan.removes_instance).then(|| plan.instance.id.clone());
            let catalog = plan.catalog().clone();
            store.apply(plan)?;
            ServerTaskOutput::Catalog {
                catalog,
                selected,
                saved: true,
                migration_pending: false,
            }
        }
        ServerTaskAction::Catalog(None) => {
            let (catalog, migration_pending) = store.load_with_status()?;
            ServerTaskOutput::Catalog {
                catalog,
                selected,
                saved: false,
                migration_pending,
            }
        }
    };
    Ok(ServerTaskResult { scope, output })
}

fn open_runtime_log_directory(directory: &Path) -> Result<(), ServerStoreError> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .arg(directory.as_os_str())
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| ServerStoreError::Io)?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        crate::core::utils::platform::open_with_default_app(directory)
            .map_err(|_| ServerStoreError::Io)
    }
}

fn maintenance_output(
    store: &ServerStore,
    id: &ServerInstanceId,
    notice: Option<super::maintenance::MaintenanceNotice>,
) -> Result<ServerTaskOutput, ServerStoreError> {
    let (overview, history) = store.maintenance_overview(id)?;
    Ok(ServerTaskOutput::Maintenance(
        Box::new(super::maintenance::MaintenanceSnapshot {
            instance_id: id.clone(),
            overview,
            recent: history
                .occurrences
                .into_iter()
                .take(super::maintenance::RECENT_OCCURRENCES)
                .collect(),
        }),
        notice,
    ))
}

fn stale_rcon(error: ServerStoreError) -> ServerStoreError {
    match error {
        ServerStoreError::StaleRevision => ServerStoreError::Invalid(
            "The draft changed elsewhere since this was opened. Review the RCon settings again."
                .into(),
        ),
        error => error,
    }
}

fn rcon_owner(
    store: &ServerStore,
) -> Result<crate::core::server::owner::Attachment, ServerStoreError> {
    crate::core::server::owner::attach(store.context())
        .map_err(|_| ServerStoreError::Ownership)?
        .ok_or(ServerStoreError::Ownership)
}

fn rcon_output(
    store: &ServerStore,
    id: &ServerInstanceId,
    notice: Option<super::rcon::RconNotice>,
    record: Option<crate::core::server::rcon::RconCommandRecord>,
) -> Result<ServerTaskOutput, ServerStoreError> {
    let instance = store.show(id)?;
    let password_available = store.rcon_password_available(id)?;
    let history = store.rcon_history(id)?;
    let (run_generation, session, session_note) =
        match crate::core::server::owner::attach(store.context()) {
            Ok(Some(owner)) => {
                let run = owner
                    .instance_status(id)
                    .ok()
                    .filter(|status| status.process_exists && status.run_generation != 0)
                    .map(|status| status.run_generation);
                match owner.rcon_inspect(id) {
                    Ok(reply) => (run, reply.status, reply.error),
                    Err(error) => (run, None, Some(error.to_string())),
                }
            }
            Ok(None) => (
                None,
                None,
                Some("No server owner is running for this game space.".into()),
            ),
            Err(_) => (
                None,
                None,
                Some("The server owner could not be reached.".into()),
            ),
        };
    Ok(ServerTaskOutput::Rcon(
        Box::new(super::rcon::RconSnapshot {
            instance_id: id.clone(),
            draft_revision: instance.revision,
            reforger: instance.definition.kind()
                == crate::core::server::ServerDefinitionKind::Reforger,
            draft: crate::core::server::rcon::draft_settings(&instance),
            password_available,
            run_generation,
            session,
            session_note,
            recent: history
                .records
                .into_iter()
                .take(super::rcon::RECENT_COMMANDS)
                .collect(),
            unreadable: history.unreadable,
        }),
        notice,
        record.map(Box::new),
    ))
}

fn maintenance_request(
    store: &ServerStore,
    id: &ServerInstanceId,
    operation: foxy_supervisor::control::Operation,
) -> Result<ServerTaskOutput, ServerStoreError> {
    use super::maintenance::MaintenanceNotice;
    let cancel = matches!(
        operation,
        foxy_supervisor::control::Operation::CancelMaintenanceOccurrence { .. }
    );
    let owner = crate::core::server::owner::attach(store.context())
        .map_err(|_| ServerStoreError::Ownership)?
        .ok_or(ServerStoreError::Ownership)?;
    let reply = owner
        .maintenance(operation)
        .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
    if let Some(error) = reply.error {
        return Err(ServerStoreError::Invalid(error));
    }
    let notice = match (cancel, reply.created) {
        (true, _) => MaintenanceNotice::for_cancel(reply.state),
        (false, true) => MaintenanceNotice::Recorded,
        (false, false) => MaintenanceNotice::AlreadyOpen,
    };
    maintenance_output(store, id, Some(notice))
}

fn execute_lifecycle(
    store: &ServerStore,
    scope: &ServerTaskScope,
    operation: foxy_supervisor::control::Operation,
    control: &ContentStageControl,
) -> Result<ServerTaskOutput, ServerStoreError> {
    let owner = crate::core::server::owner::attach(store.context())
        .map_err(|_| ServerStoreError::Ownership)?
        .ok_or(ServerStoreError::Ownership)?;
    if scope.owner.as_ref().is_none_or(|(id, generation)| {
        id != &owner.scope().owner_id || *generation != owner.scope().generation
    }) {
        return Err(ServerStoreError::Ownership);
    }
    owner
        .run_instance(operation, control)
        .map(ServerTaskOutput::InstanceRuntimeOutcome)
        .map_err(|error| ServerStoreError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_require_the_same_root_space_game_and_unique_request() {
        let scope = ServerTaskScope {
            root: PathBuf::from("root"),
            space_id: "alpha".into(),
            game_id: "arma3".into(),
            request_id: "request-1".into(),
            owner: None,
        };
        let held = scope.database_path();
        assert!(scope.matches("alpha", "arma3", Some(&held), "request-1"));
        assert!(!scope.matches("beta", "arma3", Some(&held), "request-1"));
        assert!(!scope.matches("alpha", "reforger", Some(&held), "request-1"));
        assert!(!scope.matches("alpha", "arma3", Some(Path::new("other")), "request-1"));
        assert!(!scope.matches("alpha", "arma3", Some(&held), "request-2"));
        assert!(!scope.matches("alpha", "arma3", None, "request-1"));
    }

    #[test]
    fn retained_stage_control_reports_cooperative_cancellation() {
        let control = ContentStageControl::default();
        assert!(!control.snapshot().cancel_requested);
        control.cancel();
        assert!(control.snapshot().cancel_requested);
    }
}
