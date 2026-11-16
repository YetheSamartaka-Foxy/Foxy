use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::time::{Duration, Instant};

use foxy_supervisor::control::{Operation, Scope};
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::process::{CommandSpec, ConsoleSignalHelper, TreeExit};
use foxy_supervisor::session::{ForegroundSession, SessionIdentity, SessionPhase};

use super::{
    InstanceRuntimePhase, InstanceRuntimeStatus, RuntimeOperationOutcome, manager::Snapshots,
};
use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError,
    monitoring::{PreparedObserver, RunObserver},
    owner::DeploymentExecution,
};

mod headless;

const MAX_RETAINED_INSTANCES: usize = 128;

struct Run {
    session: Option<ForegroundSession>,
    status: InstanceRuntimeStatus,
    observed: Instant,
    ports: Vec<u16>,
    monitoring: Option<RunObserver>,
    headless: headless::HeadlessGroup,
}

pub(super) struct Runs {
    store: ServerStore,
    scope: Scope,
    snapshots: Snapshots,
    runs: BTreeMap<String, Run>,
}

impl Runs {
    pub(super) fn new(store: ServerStore, scope: Scope, snapshots: Snapshots) -> Self {
        Self {
            store,
            scope,
            snapshots,
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
            } if instance_id == id.as_str() => self.start(
                &id,
                expected_activation_generation,
                &review_fingerprint,
                execution,
                control,
                None,
            ),
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
                let stopped =
                    self.stop(&id, expected_run_generation, force, timeout_ms, control)?;
                let cancelled = control.snapshot().cancel_requested;
                if !stopped.completed || cancelled {
                    return Ok(interrupted_restart(stopped, cancelled));
                }
                self.start(
                    &id,
                    expected_activation_generation,
                    &review_fingerprint,
                    execution,
                    control,
                    carried.as_ref(),
                )
            }
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
        let monitoring = plan.launch.monitoring.clone().map(|input| {
            PreparedObserver::new(
                &self.store,
                &self.runs.get(id.as_str()).expect("starting run").status,
                input,
                self.output_path(id, generation),
            )
        });
        let configuration_seeds = plan.launch.configuration_seeds.clone();
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
                    self.store
                        .prepare_secret_configuration(id, &configuration_seeds, control)
                        .map_err(|_| {
                            io::Error::other("Protected runtime configuration preparation failed")
                        })?;
                }
                if let Some(seed) = &profile_seed {
                    self.store
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
            }
            Err(_) => {
                run.status.phase = InstanceRuntimePhase::CleanupUncertain;
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
            };
            self.publish();
            return Ok(outcome);
        }
        let helper = Self::helper(
            self.store.capabilities().definition_kind
                == Some(super::super::ServerDefinitionKind::Reforger),
        )
        .map_err(|_| ServerStoreError::Io)?;
        let mut delivery = run
            .session
            .as_mut()
            .expect("retained run exists")
            .deliver_soft_stop(Some(&helper));
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
            if Instant::now() >= next_delivery {
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
        };
        self.publish();
        Ok(outcome)
    }

    pub(super) fn shutdown(&mut self) -> io::Result<()> {
        let helper = Self::helper(
            self.store.capabilities().definition_kind
                == Some(super::super::ServerDefinitionKind::Reforger),
        )?;
        for run in self.runs.values_mut() {
            run.headless.hold();
            run.headless.deliver_soft_stop_all(&helper);
            if let Some(session) = &mut run.session {
                let _ = session.deliver_soft_stop(Some(&helper));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.observe_all();
            if self
                .runs
                .values()
                .all(|run| run.session.is_none() && run.headless.running() == 0)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                for run in self.runs.values_mut() {
                    run.headless.force(None);
                    if let Some(session) = &mut run.session {
                        if let Ok(Some(exit)) = session.force_stop(Duration::from_millis(100)) {
                            Self::exited(run, exit);
                        } else {
                            run.status.phase = InstanceRuntimePhase::CleanupUncertain;
                        }
                    }
                }
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
        };
        let refused = interrupted_restart(timed_out, false);
        assert!(!refused.completed);
        assert!(!refused.cancelled);
        assert_eq!(refused.message.as_deref(), Some("Stop timed out"));
    }
}
