use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::time::{Duration, Instant};

use foxy_supervisor::control::{Operation, Scope};
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::process::{CommandSpec, ConsoleSignalHelper, TreeExit};
use foxy_supervisor::session::{ForegroundSession, SessionIdentity, SessionPhase};

use super::{
    InstanceRuntimePhase, InstanceRuntimeStatus, RuntimeOperationOutcome,
    manager::{RconTargets, Snapshots},
};
use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError,
    monitoring::{PreparedObserver, RunObserver},
    owner::DeploymentExecution,
};

mod administration;
mod headless;
mod reforger;

const MAX_RETAINED_INSTANCES: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RconShutdownDelivery {
    Unattempted,
    NotSent,
    SentOrUnknown,
}

struct Run {
    session: Option<ForegroundSession>,
    status: InstanceRuntimeStatus,
    observed: Instant,
    ports: Vec<u16>,
    monitoring: Option<RunObserver>,
    headless: headless::HeadlessGroup,
    rcon: Option<crate::core::server::deployment_activation::NativeRconPlan>,
    rcon_stop: RconShutdownDelivery,
    reforger_scenario: Option<String>,
}

pub(super) struct Runs {
    store: ServerStore,
    scope: Scope,
    snapshots: Snapshots,
    rcon_targets: RconTargets,
    runs: BTreeMap<String, Run>,
}

impl Runs {
    pub(super) fn new(
        store: ServerStore,
        scope: Scope,
        snapshots: Snapshots,
        rcon_targets: RconTargets,
    ) -> Self {
        Self {
            store,
            scope,
            snapshots,
            rcon_targets,
            runs: BTreeMap::new(),
        }
    }

    fn publish(&self) {
        let mut snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (id, run) in &self.runs {
            snapshots.insert(id.clone(), (run.status.clone(), run.observed));
        }
        drop(snapshots);
        let mut targets = self
            .rcon_targets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        targets.clear();
        for (id, run) in &self.runs {
            if let (Some(_), Some(plan)) = (&run.session, &run.rcon)
                && run.status.process_exists
            {
                targets.insert(id.clone(), (run.status.run_generation, plan.clone()));
            }
        }
    }

    pub(super) fn observe_all(&mut self) {
        for run in self.runs.values_mut() {
            Self::observe(run);
            if run.session.is_none() && run.headless.running() > 0 {
                Self::stop_orphaned_headless(run);
            }
            let ready = run.monitoring.as_ref().is_some_and(|observer| {
                observer.headless_readiness()
                    == crate::core::server::headless::HeadlessReadiness::Ready
            });
            run.headless.publish(ready);
        }
        self.publish();
    }

    /// Unconfirmed cleanup is retried on a slow cadence so it cannot stall every owner tick.
    fn stop_orphaned_headless(run: &mut Run) {
        run.headless.server_ended();
        if !run.headless.orphan_cleanup_due() {
            return;
        }
        let clean = match Self::helper(false) {
            Ok(helper) => run.headless.stop_with_owner_policy(&helper),
            Err(_) => run.headless.force(None),
        };
        run.headless.orphan_cleanup_result(clean);
        if !clean {
            run.status.phase = InstanceRuntimePhase::CleanupUncertain;
        }
        run.status.headless_running = run.headless.running();
    }

    fn observe(run: &mut Run) {
        if let Some(observer) = &run.monitoring {
            run.status.monitoring_fingerprint = observer.fingerprint();
        }
        run.headless.observe();
        run.status.headless_running = run.headless.running();
        let Some(session) = &mut run.session else {
            return;
        };
        run.observed = Instant::now();
        match session.observe() {
            Ok(Some(exit)) => {
                Self::exited(run, exit);
            }
            Ok(None) => {
                run.status.process_exists = true;
                run.status.phase = match session.event().phase {
                    SessionPhase::Spawned => InstanceRuntimePhase::Running,
                    SessionPhase::StopRequested | SessionPhase::Terminating => {
                        InstanceRuntimePhase::StopRequested
                    }
                    SessionPhase::CleanupUncertain => InstanceRuntimePhase::CleanupUncertain,
                    SessionPhase::Exited => InstanceRuntimePhase::Exited,
                };
            }
            Err(_) => run.status.phase = InstanceRuntimePhase::CleanupUncertain,
        }
    }

    fn exited(run: &mut Run, exit: TreeExit) {
        run.status.phase = InstanceRuntimePhase::Exited;
        run.status.process_exists = false;
        run.status.cleanup_verified = true;
        run.status.forced = exit.forced;
        run.status.root_exit_code = exit.root_exit_code;
        run.observed = Instant::now();
        run.session.take();
        run.headless.server_ended();
        if run.headless.running() > 0 {
            Self::stop_orphaned_headless(run);
        }
        run.headless.publish(false);
        if let Some(observer) = &mut run.monitoring {
            observer.stop();
            run.status.monitoring_fingerprint = observer.fingerprint();
        }
    }

    pub(super) fn execute(
        &mut self,
        operation: Operation,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        execution
            .validate(self.store.context())
            .map_err(|_| ServerStoreError::Ownership)?;
        let id = execution.identity().target_instance.clone();
        self.store.show(&id)?;
        self.observe_all();
        match operation {
            Operation::StartInstance {
                instance_id,
                expected_activation_generation,
                review_fingerprint,
            } if instance_id == id.as_str() => {
                let created_backup = if execution.require_backup {
                    let status = self
                        .runs
                        .get(id.as_str())
                        .map(|run| run.status.clone())
                        .unwrap_or_else(|| {
                            InstanceRuntimeStatus::not_owned(id.clone(), &self.scope)
                        });
                    let outcome = self.store.create_server_backup(
                        &id,
                        crate::core::server::BackupConsistencyMode::StoppedVerified,
                        None,
                        true,
                        Some(&status),
                        control,
                    )?;
                    Some((
                        outcome.manifest.backup_id,
                        outcome.manifest.consistency.mode,
                    ))
                } else {
                    None
                };
                match self.start(
                    &id,
                    expected_activation_generation,
                    &review_fingerprint,
                    execution,
                    control,
                    None,
                ) {
                    Ok(mut outcome) => {
                        if let Some((backup_id, backup_consistency)) = created_backup {
                            outcome.backup_id = Some(backup_id);
                            outcome.backup_consistency = Some(backup_consistency);
                        }
                        Ok(outcome)
                    }
                    Err(error) => {
                        if let Some((backup_id, backup_consistency)) = created_backup {
                            let status = self
                                .runs
                                .get(id.as_str())
                                .map(|run| run.status.clone())
                                .unwrap_or_else(|| {
                                    InstanceRuntimeStatus::not_owned(id.clone(), &self.scope)
                                });
                            Ok(RuntimeOperationOutcome {
                                status,
                                completed: false,
                                cancelled: control.snapshot().cancel_requested
                                    || error == ServerStoreError::Cancelled,
                                message: Some(error.to_string()),
                                backup_id: Some(backup_id),
                                backup_consistency: Some(backup_consistency),
                            })
                        } else {
                            Err(error)
                        }
                    }
                }
            }
            Operation::StopInstance {
                instance_id,
                expected_run_generation,
                force,
                timeout_ms,
            } if instance_id == id.as_str() => {
                self.stop(&id, expected_run_generation, force, timeout_ms, control)
            }
            Operation::RestartInstance {
                instance_id,
                expected_run_generation,
                expected_activation_generation,
                review_fingerprint,
                force,
                timeout_ms,
            } if instance_id == id.as_str() => {
                let plan = self.store.plan_instance_start(&id, control)?;
                plan.require_review(expected_activation_generation, &review_fingerprint)?;
                let carried = self.runs.get(id.as_str()).map(|run| run.headless.carried());
                self.announce_maintenance(&id, expected_run_generation, execution, control)?;
                let stopped =
                    self.stop(&id, expected_run_generation, force, timeout_ms, control)?;
                let cancelled = control.snapshot().cancel_requested;
                if !stopped.completed || cancelled {
                    return Ok(interrupted_restart(stopped, cancelled));
                }
                let created_backup = if execution.require_backup {
                    match self.store.create_server_backup(
                        &id,
                        crate::core::server::BackupConsistencyMode::StoppedVerified,
                        None,
                        true,
                        Some(&stopped.status),
                        control,
                    ) {
                        Ok(outcome) => Some((
                            outcome.manifest.backup_id,
                            outcome.manifest.consistency.mode,
                        )),
                        Err(error) => {
                            let cancelled = control.snapshot().cancel_requested
                                || error == ServerStoreError::Cancelled;
                            let mut interrupted = interrupted_restart(stopped, cancelled);
                            if !cancelled {
                                interrupted.message = Some(format!(
                                    "Server stopped, but required backup failed before start: {error}"
                                ));
                            }
                            return Ok(interrupted);
                        }
                    }
                } else {
                    None
                };
                match self.start(
                    &id,
                    expected_activation_generation,
                    &review_fingerprint,
                    execution,
                    control,
                    carried.as_ref(),
                ) {
                    Ok(mut outcome) => {
                        if let Some((backup_id, backup_consistency)) = created_backup {
                            outcome.backup_id = Some(backup_id);
                            outcome.backup_consistency = Some(backup_consistency);
                        }
                        Ok(outcome)
                    }
                    Err(error) => {
                        if let Some((backup_id, backup_consistency)) = created_backup {
                            Ok(RuntimeOperationOutcome {
                                status: stopped.status,
                                completed: false,
                                cancelled: control.snapshot().cancel_requested
                                    || error == ServerStoreError::Cancelled,
                                message: Some(error.to_string()),
                                backup_id: Some(backup_id),
                                backup_consistency: Some(backup_consistency),
                            })
                        } else {
                            Err(error)
                        }
                    }
                }
            }
            Operation::RotateArmaMission {
                instance_id,
                expected_run_generation,
                template,
                timeout_ms,
            } if instance_id == id.as_str() => self.rotate_mission(
                &id,
                expected_run_generation,
                &template,
                timeout_ms,
                execution,
                control,
            ),
            Operation::ReloadReforgerScenario {
                instance_id,
                expected_run_generation,
                timeout_ms,
            } if instance_id == id.as_str() => self.reload_reforger_scenario(
                &id,
                expected_run_generation,
                timeout_ms,
                execution,
                control,
            ),
            Operation::StartHeadlessClients {
                instance_id,
                expected_run_generation,
                headless_id,
            } if instance_id == id.as_str() => self.start_headless(
                &id,
                expected_run_generation,
                headless_id.as_deref(),
                execution,
                control,
            ),
            Operation::StopHeadlessClients {
                instance_id,
                expected_run_generation,
                headless_id,
                force,
                timeout_ms,
            } if instance_id == id.as_str() => self.stop_headless(
                &id,
                expected_run_generation,
                headless_id.as_deref(),
                force,
                timeout_ms,
                control,
            ),
            Operation::RestartHeadlessClients {
                instance_id,
                expected_run_generation,
                headless_id,
                timeout_ms,
            } if instance_id == id.as_str() => self.restart_headless(
                &id,
                expected_run_generation,
                headless_id.as_deref(),
                timeout_ms,
                execution,
                control,
            ),
            Operation::CreateServerBackup {
                instance_id,
                mode,
                expected_plan_fingerprint,
                apply_retention,
            } if instance_id == id.as_str() => {
                let consistency_mode = match mode.as_str() {
                    "stopped_verified" => {
                        crate::core::server::BackupConsistencyMode::StoppedVerified
                    }
                    "best_effort_live" => {
                        crate::core::server::BackupConsistencyMode::BestEffortLive
                    }
                    _ => {
                        return Err(ServerStoreError::Invalid(
                            "Invalid backup consistency mode.".into(),
                        ));
                    }
                };
                if execution.arma_maintenance.is_some() {
                    let expected_run = self
                        .runs
                        .get(id.as_str())
                        .map_or(0, |run| run.status.run_generation);
                    if expected_run > 0 {
                        let _ = self.announce_maintenance(&id, expected_run, execution, control);
                    }
                }
                let status = self
                    .runs
                    .get(id.as_str())
                    .map(|run| run.status.clone())
                    .unwrap_or_else(|| InstanceRuntimeStatus::not_owned(id.clone(), &self.scope));
                let outcome = self.store.create_server_backup(
                    &id,
                    consistency_mode,
                    expected_plan_fingerprint.as_deref(),
                    apply_retention,
                    Some(&status),
                    control,
                )?;
                Ok(RuntimeOperationOutcome {
                    status,
                    completed: true,
                    cancelled: false,
                    message: outcome.retention_receipt_id,
                    backup_id: Some(outcome.manifest.backup_id),
                    backup_consistency: Some(outcome.manifest.consistency.mode),
                })
            }
            Operation::RestoreServerBackup {
                instance_id,
                backup_id,
                expected_plan_fingerprint,
            } if instance_id == id.as_str() => {
                let status = self
                    .runs
                    .get(id.as_str())
                    .map(|run| run.status.clone())
                    .unwrap_or_else(|| InstanceRuntimeStatus::not_owned(id.clone(), &self.scope));
                let outcome = self.store.restore_server_backup(
                    &id,
                    &backup_id,
                    &expected_plan_fingerprint,
                    Some(&status),
                    control,
                )?;
                Ok(RuntimeOperationOutcome {
                    status,
                    completed: true,
                    cancelled: false,
                    message: Some(outcome.restore_id),
                    backup_id: None,
                    backup_consistency: None,
                })
            }
            Operation::RollbackServerRestore {
                instance_id,
                expected_journal_fingerprint,
            } if instance_id == id.as_str() => {
                let status = self
                    .runs
                    .get(id.as_str())
                    .map(|run| run.status.clone())
                    .unwrap_or_else(|| InstanceRuntimeStatus::not_owned(id.clone(), &self.scope));
                let outcome = self.store.rollback_server_restore(
                    &id,
                    &expected_journal_fingerprint,
                    Some(&status),
                    control,
                )?;
                Ok(RuntimeOperationOutcome {
                    status,
                    completed: true,
                    cancelled: false,
                    message: Some(outcome.restore_id),
                    backup_id: None,
                    backup_consistency: None,
                })
            }
            Operation::PruneServerBackups {
                instance_id,
                expected_plan_fingerprint,
                keep_last_override,
            } if instance_id == id.as_str() => {
                let status = self
                    .runs
                    .get(id.as_str())
                    .map(|run| run.status.clone())
                    .unwrap_or_else(|| InstanceRuntimeStatus::not_owned(id.clone(), &self.scope));
                let outcome = self.store.apply_backup_prune(
                    &id,
                    &expected_plan_fingerprint,
                    keep_last_override,
                    control,
                )?;
                Ok(RuntimeOperationOutcome {
                    status,
                    completed: true,
                    cancelled: false,
                    message: Some(outcome.receipt_id),
                    backup_id: None,
                    backup_consistency: None,
                })
            }
            _ => Err(ServerStoreError::Invalid(
                "Invalid foreground instance operation".into(),
            )),
        }
    }

    fn start(
        &mut self,
        id: &ServerInstanceId,
        expected: u64,
        fingerprint: &str,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
        carried: Option<&BTreeMap<String, bool>>,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        if self
            .runs
            .get(id.as_str())
            .is_some_and(|run| run.session.is_some() || run.headless.running() > 0)
        {
            return Err(ServerStoreError::Invalid(
                "This instance already has a retained foreground session.".into(),
            ));
        }
        if !self.runs.contains_key(id.as_str()) && self.runs.len() >= MAX_RETAINED_INSTANCES {
            return Err(ServerStoreError::Invalid(
                "This owner has reached its retained instance limit.".into(),
            ));
        }
        control.checkpoint()?;
        let installation = self.store.native_installation(id)?;
        let locks = InstallationLocks::capture(&installation.directory)
            .map_err(|_| ServerStoreError::Ownership)?;
        let admission = locks.try_admit().map_err(|_| ServerStoreError::Ownership)?;
        admission.check_consumer_slot(self.store.context().space_id(), id.as_str())
            .map_err(|_| ServerStoreError::Invalid("An instance consumer registration is unresolved; review consumer recovery before starting.".into()))?;
        let plan = self.store.plan_instance_start(id, control)?;
        plan.require_review(expected, fingerprint)?;
        let launch_store = ServerStore::new(self.store.context().canonicalized()?)?;
        let ports = plan.launch.udp_ports.clone();
        if self
            .runs
            .values()
            .any(|run| run.session.is_some() && run.ports.iter().any(|port| ports.contains(port)))
        {
            return Err(ServerStoreError::Invalid(
                "Another owned server uses an overlapping native game/query port range.".into(),
            ));
        }
        let generation = self
            .runs
            .get(id.as_str())
            .map_or(Some(1), |run| run.status.run_generation.checked_add(1))
            .ok_or_else(|| ServerStoreError::Invalid("Run generations are exhausted.".into()))?;
        let mut status = InstanceRuntimeStatus::not_owned(id.clone(), &self.scope);
        status.run_generation = generation;
        status.activation_generation = Some(plan.activation_generation);
        status.runtime_id = Some(plan.runtime_id.clone());
        status.phase = InstanceRuntimePhase::Starting;
        self.runs.insert(
            id.as_str().into(),
            Run {
                session: None,
                status,
                observed: Instant::now(),
                ports,
                monitoring: None,
                headless: headless::HeadlessGroup::default(),
                rcon: None,
                rcon_stop: RconShutdownDelivery::Unattempted,
                reforger_scenario: plan.launch.reforger_scenario.clone(),
            },
        );
        self.publish();
        let output = match self.output(id, generation) {
            Ok(output) => output,
            Err(error) => {
                let run = self.runs.get_mut(id.as_str()).expect("starting run exists");
                run.status.phase = InstanceRuntimePhase::NotOwned;
                run.status.cleanup_verified = true;
                run.observed = Instant::now();
                self.publish();
                return Err(error);
            }
        };
        let profile_seed = plan.launch.profile_seed.clone();
        let rcon_secret = plan.launch.rcon.as_ref().and_then(|rcon| {
            self.store
                .resolve_config_secret(id, &rcon.password_reference)
                .ok()
                .map(zeroize::Zeroizing::new)
        });
        let monitoring = plan.launch.monitoring.clone().map(|input| {
            let mut observer = PreparedObserver::new(
                &self.store,
                &self.runs.get(id.as_str()).expect("starting run").status,
                input,
                self.output_path(id, generation),
            );
            observer.redact_values(rcon_secret.into_iter().collect());
            observer
        });
        let configuration_seeds = plan.launch.configuration_seeds.clone();
        let battleye_seed = plan.launch.battleye.clone();
        let rcon_plan = plan.launch.rcon.clone();
        let headless_plans = plan.launch.headless.clone();
        let spec = CommandSpec {
            executable: plan.launch.executable,
            directory: plan.launch.directory,
            arguments: plan
                .launch
                .arguments
                .into_iter()
                .map(OsString::from)
                .collect(),
            environment: plan
                .launch
                .environment
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        };
        let identity = SessionIdentity {
            owner_id: self.scope.owner_id.clone(),
            owner_generation: self.scope.generation,
            game_space_id: self.scope.space_id.clone(),
            instance_id: id.as_str().into(),
            operation_id: execution.identity().operation_id.clone(),
            run_generation: generation,
        };
        let witness = foxy_supervisor::process::CleanupWitnessCommand {
            executable: std::env::current_exe().map_err(|_| ServerStoreError::Io)?,
            arguments: vec!["server".into(), "cleanup-witness".into()],
            group_id: id.as_str().into(),
            context_key: self.scope.context_key.clone(),
        };
        let result = ForegroundSession::spawn_controlled_witnessed(
            identity,
            &spec,
            &output,
            &witness,
            admission,
            control.cancellation_flag(),
            || {
                control
                    .checkpoint()
                    .map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "Start cancelled"))?;
                execution.validate(self.store.context())?;
                installation
                    .verify()
                    .map_err(|_| io::Error::other("Installation evidence changed"))?;
                let (active, _) = self
                    .store
                    .settled_activation_state(id, control)
                    .map_err(|_| io::Error::other("Deployment recovery or state changed"))?;
                if active.generation != expected
                    || active
                        .deployment
                        .runtime
                        .as_ref()
                        .map(|value| value.runtime_id.as_str())
                        != Some(plan.runtime_id.as_str())
                {
                    return Err(io::Error::other("Committed runtime changed"));
                }
                if !configuration_seeds.is_empty() {
                    launch_store
                        .prepare_secret_configuration(id, &configuration_seeds, control)
                        .map_err(|_| {
                            io::Error::other("Protected runtime configuration preparation failed")
                        })?;
                }
                if let Some(seed) = &battleye_seed {
                    launch_store
                        .prepare_battleye_state(id, seed, control)
                        .map_err(|_| {
                            io::Error::other("Private BattlEye state preparation failed")
                        })?;
                }
                if let Some(seed) = &profile_seed {
                    launch_store
                        .prepare_native_profile(id, seed, control)
                        .map_err(|_| io::Error::other("Managed profile preparation failed"))?;
                }
                Ok(())
            },
        );
        let run = self.runs.get_mut(id.as_str()).expect("starting run exists");
        match result {
            Ok(session) => {
                run.status.process_id = Some(session.process_id());
                run.status.process_exists = true;
                run.status.phase = InstanceRuntimePhase::Running;
                let shared = crate::core::server::headless::SharedHeadlessStatus::default();
                run.headless =
                    headless::HeadlessGroup::new(headless_plans, carried, shared.clone());
                run.monitoring = monitoring
                    .and_then(|observer| observer.start(session.process_id(), shared).ok());
                run.session = Some(session);
                run.rcon = rcon_plan;
            }
            Err(_) => {
                run.status.cleanup_verified = locks.inspect_consumers().is_ok_and(|records| {
                    !records.iter().any(|record| {
                        record.session.game_space_id == self.store.context().space_id()
                            && crate::core::server::headless::consumer_belongs_to(
                                &record.session.instance_id,
                                id.as_str(),
                            )
                    })
                });
                run.status.phase = if run.status.cleanup_verified {
                    InstanceRuntimePhase::NotOwned
                } else {
                    InstanceRuntimePhase::CleanupUncertain
                };
                run.observed = Instant::now();
                self.publish();
                return Err(ServerStoreError::Invalid("Foreground Start failed; inspect status and installation consumers before retrying.".into()));
            }
        }
        let message = control.snapshot().cancel_requested.then(|| "Cancellation arrived after Start. The server remains owned; inspect status and stop it explicitly if needed.".into());
        let outcome = RuntimeOperationOutcome {
            status: run.status.clone(),
            completed: true,
            cancelled: false,
            message,
            backup_id: None,
            backup_consistency: None,
        };
        self.publish();
        Ok(outcome)
    }

    fn output(&self, id: &ServerInstanceId, generation: u64) -> Result<File, ServerStoreError> {
        let root = self.store.record_root(id).join("runtime-state");
        self.store.check_deployment_destination(&root, None)?;
        let path = self.output_path(id, generation);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        options.open(path).map_err(|_| ServerStoreError::Io)
    }

    fn output_path(&self, id: &ServerInstanceId, generation: u64) -> std::path::PathBuf {
        self.store
            .record_root(id)
            .join("runtime-state")
            .join(format!("run_{}_{generation}.log", self.scope.owner_id))
    }

    fn helper(reforger: bool) -> io::Result<ConsoleSignalHelper> {
        let mut arguments = vec!["server".into(), "console-signal".into()];
        if cfg!(windows) && reforger {
            arguments.push("--ctrl-break".into());
        }
        Ok(ConsoleSignalHelper {
            executable: std::env::current_exe()?,
            arguments,
        })
    }

    fn stop(
        &mut self,
        id: &ServerInstanceId,
        expected: u64,
        force: bool,
        timeout_ms: u32,
        control: &ContentStageControl,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        if timeout_ms == 0 || timeout_ms > 120_000 {
            return Err(ServerStoreError::Invalid("Invalid stop timeout".into()));
        }
        let run = self.runs.get_mut(id.as_str()).ok_or_else(|| {
            ServerStoreError::Invalid(
                "This owner has no retained session for that instance.".into(),
            )
        })?;
        if expected == 0 || expected != run.status.run_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        if run.session.is_none() {
            return Ok(RuntimeOperationOutcome {
                status: run.status.clone(),
                completed: run.status.cleanup_verified,
                cancelled: false,
                message: None,
                backup_id: None,
                backup_consistency: None,
            });
        }
        control.checkpoint()?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
        run.headless.hold();
        if run.headless.running() > 0 && !force {
            let helper = Self::helper(false).map_err(|_| ServerStoreError::Io)?;
            let result = run.headless.stop(&helper, deadline, Some(control), None);
            run.status.headless_running = run.headless.running();
            if result != headless::GroupStop::Completed {
                let cancelled = result == headless::GroupStop::Cancelled;
                run.status.stop_timed_out = !cancelled;
                run.observed = Instant::now();
                let outcome = RuntimeOperationOutcome {
                    status: run.status.clone(),
                    completed: false,
                    cancelled,
                    message: Some(if cancelled {
                        "Stop was cancelled while headless clients were stopping. The server was not stopped.".into()
                    } else {
                        "A headless client did not stop within the timeout. The server was not stopped; retry, or force the stop.".into()
                    }),
                    backup_id: None,
                    backup_consistency: None,
                };
                self.publish();
                return Ok(outcome);
            }
        }
        if force {
            if run.headless.running() > 0 {
                run.headless.force(None);
            }
            run.status.headless_running = run.headless.running();
            let result = run
                .session
                .as_mut()
                .expect("retained run exists")
                .force_stop(Duration::from_secs(5));
            match result {
                Ok(Some(exit)) => Self::exited(run, exit),
                _ => run.status.phase = InstanceRuntimePhase::CleanupUncertain,
            }
            if run.headless.running() > 0 {
                run.status.phase = InstanceRuntimePhase::CleanupUncertain;
            }
            run.observed = Instant::now();
            let completed =
                run.session.is_none() && run.status.cleanup_verified && run.headless.running() == 0;
            let outcome = RuntimeOperationOutcome {
                status: run.status.clone(), completed, cancelled: false,
                message: (!completed).then(|| "Forced stop cleanup is unconfirmed; the retained session and protection remain owned.".into()),
                backup_id: None,
                backup_consistency: None,
            };
            self.publish();
            return Ok(outcome);
        }
        control.checkpoint()?;
        let use_rcon = if (self.store.capabilities().arma_administration
            || self.store.capabilities().reforger_administration)
            && let Some(rcon_plan) = run.rcon.as_ref()
            && rcon_plan
                .reforger
                .as_ref()
                .is_none_or(|access| access.permits("#shutdown", true))
        {
            if run.rcon_stop == RconShutdownDelivery::Unattempted {
                run.rcon_stop = RconShutdownDelivery::SentOrUnknown;
                let result = crate::core::server::rcon::owned::command_once(
                    &self.store,
                    id,
                    &self.scope,
                    run.status.run_generation,
                    rcon_plan,
                    "shutdown",
                    "#shutdown",
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(5)),
                    control,
                );
                match result {
                    Ok(record)
                        if record.state == crate::core::server::rcon::RconCommandState::NotSent =>
                    {
                        run.rcon_stop = RconShutdownDelivery::NotSent
                    }
                    Err(error) if error != ServerStoreError::RconOutcomeUnknown => {
                        run.rcon_stop = RconShutdownDelivery::NotSent
                    }
                    _ => {}
                }
            }
            run.rcon_stop != RconShutdownDelivery::NotSent
        } else {
            false
        };
        control.checkpoint()?;
        let helper = Self::helper(
            self.store.capabilities().definition_kind
                == Some(super::super::ServerDefinitionKind::Reforger),
        )
        .map_err(|_| ServerStoreError::Io)?;
        let mut delivery = if use_rcon {
            Ok(())
        } else {
            run.session
                .as_mut()
                .expect("retained run exists")
                .deliver_soft_stop(Some(&helper))
        };
        run.status.phase = InstanceRuntimePhase::StopRequested;
        run.status.stop_timed_out = false;
        run.observed = Instant::now();
        self.publish();
        let mut next_delivery = Instant::now() + Duration::from_millis(500);
        loop {
            let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
            Self::observe(run);
            if run.session.is_none() {
                break;
            }
            if control.snapshot().cancel_requested || Instant::now() >= deadline {
                break;
            }
            if !use_rcon && Instant::now() >= next_delivery {
                delivery = run
                    .session
                    .as_mut()
                    .expect("retained run exists")
                    .deliver_soft_stop(Some(&helper));
                next_delivery = Instant::now() + Duration::from_millis(500);
            }
            self.publish();
            std::thread::sleep(Duration::from_millis(20));
        }
        let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
        let cancel_requested = control.snapshot().cancel_requested;
        let completed = run.session.is_none() && run.status.cleanup_verified;
        let cancelled = cancel_requested && !completed;
        run.status.stop_timed_out = !completed && !cancelled;
        let message = if completed {
            None
        } else if cancelled {
            Some("Stop wait cancelled; the retained session and protection remain until cleanup is observed.".into())
        } else if let Err(error) = delivery {
            Some(format!(
                "Soft stop delivery failed{}; session remains owned. Review status before an explicit force stop.",
                error
                    .raw_os_error()
                    .map(|code| format!(" (OS error {code})"))
                    .unwrap_or_default()
            ))
        } else {
            Some("Stop timed out; session, leases and consumer protection remain owned.".into())
        };
        let outcome = RuntimeOperationOutcome {
            status: run.status.clone(),
            completed,
            cancelled,
            message,
            backup_id: None,
            backup_consistency: None,
        };
        self.publish();
        Ok(outcome)
    }

    pub(super) fn shutdown(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let helper = Self::helper(
            self.store.capabilities().definition_kind
                == Some(super::super::ServerDefinitionKind::Reforger),
        )?;
        for run in self.runs.values_mut() {
            run.headless.hold();
            run.headless.deliver_soft_stop_all(&helper);
        }
        let mut signalled_servers = BTreeSet::new();
        let mut next_delivery = Instant::now() + Duration::from_millis(500);
        loop {
            self.observe_all();
            if self
                .runs
                .values()
                .all(|run| run.session.is_none() && run.headless.running() == 0)
            {
                return Ok(());
            }
            let expired = Instant::now() >= deadline;
            let retry = Instant::now() >= next_delivery;
            for (id, run) in &mut self.runs {
                if run.headless.running() != 0 {
                    if expired {
                        run.headless
                            .force_with_timeout(None, Duration::from_millis(100));
                    } else if retry {
                        run.headless.deliver_soft_stop_all(&helper);
                    }
                    if run.headless.running() != 0 {
                        continue;
                    }
                }
                if let Some(session) = &mut run.session {
                    if expired {
                        if let Ok(Some(exit)) = session.force_stop(Duration::from_millis(100)) {
                            Self::exited(run, exit);
                        } else {
                            run.status.phase = InstanceRuntimePhase::CleanupUncertain;
                        }
                    } else if signalled_servers.insert(id.clone()) || retry {
                        let _ = session.deliver_soft_stop(Some(&helper));
                    }
                }
            }
            if retry {
                next_delivery = Instant::now() + Duration::from_millis(500);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn interrupted_restart(
    mut stopped: RuntimeOperationOutcome,
    cancelled: bool,
) -> RuntimeOperationOutcome {
    if cancelled && stopped.completed {
        stopped.message = Some("Restart cancelled after Stop. No new process was started; refresh runtime status before retrying.".into());
    }
    stopped.completed = false;
    stopped.cancelled = cancelled;
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_after_completed_stop_is_an_incomplete_restart_with_the_old_run() {
        let id = ServerInstanceId::try_from(format!("server_{}", "1".repeat(32))).unwrap();
        let scope = Scope {
            context_key: "a".repeat(64),
            space_id: "alpha".into(),
            instance_id: "space_catalog".into(),
            owner_id: "owner_alpha".into(),
            generation: 1,
        };
        let mut status = InstanceRuntimeStatus::not_owned(id, &scope);
        status.run_generation = 4;
        status.phase = InstanceRuntimePhase::Exited;
        status.cleanup_verified = true;
        let stopped = RuntimeOperationOutcome {
            status: status.clone(),
            completed: true,
            cancelled: false,
            message: None,
            backup_id: None,
            backup_consistency: None,
        };
        let cancelled = interrupted_restart(stopped, true);
        assert!(!cancelled.completed);
        assert!(cancelled.cancelled);
        assert_eq!(cancelled.status, status);
        assert!(
            cancelled
                .message
                .unwrap()
                .contains("No new process was started")
        );

        status.phase = InstanceRuntimePhase::StopRequested;
        status.cleanup_verified = false;
        status.process_exists = true;
        status.stop_timed_out = true;
        let timed_out = RuntimeOperationOutcome {
            status,
            completed: false,
            cancelled: false,
            message: Some("Stop timed out".into()),
            backup_id: None,
            backup_consistency: None,
        };
        let refused = interrupted_restart(timed_out, false);
        assert!(!refused.completed);
        assert!(!refused.cancelled);
        assert_eq!(refused.message.as_deref(), Some("Stop timed out"));
    }
}
