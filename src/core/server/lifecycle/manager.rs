use std::collections::BTreeMap;
use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use foxy_supervisor::control::{Operation, Scope};

use super::{InstanceRuntimeStatus, RuntimeOperationOutcome, engine::Runs};
use crate::core::server::{
    ContentStageControl, ServerContext, ServerInstanceId, ServerStore, owner::DeploymentExecution,
};

pub(super) type Snapshots = Arc<Mutex<BTreeMap<String, (InstanceRuntimeStatus, Instant)>>>;
/// Live runs whose committed runtime configures RCon, keyed by instance id.
pub(super) type RconTargets = Arc<
    Mutex<
        BTreeMap<
            String,
            (
                u64,
                crate::core::server::deployment_activation::NativeRconPlan,
            ),
        >,
    >,
>;

struct Command {
    operation: Operation,
    execution: DeploymentExecution,
    control: ContentStageControl,
}

pub(crate) struct RuntimeOwner {
    scope: Scope,
    snapshots: Snapshots,
    rcon_targets: RconTargets,
    commands: Option<mpsc::SyncSender<Command>>,
    results: mpsc::Receiver<Result<RuntimeOperationOutcome, String>>,
    stopped: Arc<AtomicBool>,
    handle: Option<JoinHandle<io::Result<()>>>,
}

impl RuntimeOwner {
    pub(crate) fn active_sessions(&self) -> u32 {
        let snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        snapshots
            .values()
            .filter(|(status, _)| {
                !matches!(
                    status.phase,
                    super::InstanceRuntimePhase::NotOwned | super::InstanceRuntimePhase::Exited
                )
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }
    pub(crate) fn new(context: ServerContext, scope: Scope) -> io::Result<Self> {
        let snapshots = Arc::new(Mutex::new(BTreeMap::new()));
        let rcon_targets: RconTargets = Arc::new(Mutex::new(BTreeMap::new()));
        let shared_targets = rcon_targets.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let (commands, requests) = mpsc::sync_channel::<Command>(1);
        let (replies, results) = mpsc::sync_channel(1);
        let shared = snapshots.clone();
        let stopping = stopped.clone();
        let captured = scope.clone();
        let handle = std::thread::Builder::new()
            .name("foxy-instance-owner".into())
            .spawn(move || {
                let store = ServerStore::new(context)
                    .map_err(|_| io::Error::other("Runtime owner store is unavailable"))?;
                let mut runs = Runs::new(store, captured, shared, shared_targets);
                loop {
                    runs.observe_all();
                    runs.tick_headless();
                    if stopping.load(Ordering::Acquire) {
                        return runs.shutdown();
                    }
                    match requests.recv_timeout(Duration::from_millis(50)) {
                        Ok(command) => {
                            let result = runs.execute(command.operation, &command.execution, &command.control);
                            if replies.send(result.map_err(|error| {
                                if error == crate::core::server::ServerStoreError::Cancelled {
                                    "Foreground operation cancelled; inspect the retained runtime status before retrying.".into()
                                } else { error.to_string() }
                            })).is_err() {
                                return runs.shutdown();
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return runs.shutdown(),
                    }
                }
            })?;
        Ok(Self {
            scope,
            snapshots,
            rcon_targets,
            commands: Some(commands),
            results,
            stopped,
            handle: Some(handle),
        })
    }

    pub(crate) fn rcon_targets(
        &self,
    ) -> BTreeMap<
        String,
        (
            u64,
            crate::core::server::deployment_activation::NativeRconPlan,
        ),
    > {
        self.rcon_targets
            .lock()
            .map(|targets| targets.clone())
            .unwrap_or_default()
    }

    pub(crate) fn status(&self, id: &ServerInstanceId) -> io::Result<InstanceRuntimeStatus> {
        if self
            .handle
            .as_ref()
            .is_none_or(|handle| handle.is_finished())
        {
            return Err(io::Error::other("Runtime owner observation is unavailable"));
        }
        let snapshots = self
            .snapshots
            .lock()
            .map_err(|_| io::Error::other("Runtime observation is unavailable"))?;
        Ok(match snapshots.get(id.as_str()) {
            Some((status, observed)) => {
                let mut status = status.clone();
                status.observation_age_ms =
                    u64::try_from(observed.elapsed().as_millis()).unwrap_or(u64::MAX);
                status
            }
            None => InstanceRuntimeStatus::not_owned(id.clone(), &self.scope),
        })
    }

    pub(crate) fn submit(
        &self,
        operation: Operation,
        execution: DeploymentExecution,
        control: ContentStageControl,
    ) -> io::Result<()> {
        self.commands
            .as_ref()
            .ok_or_else(|| io::Error::other("Runtime owner is closing"))?
            .try_send(Command {
                operation,
                execution,
                control,
            })
            .map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "Runtime operation queue is busy")
            })
    }

    pub(crate) fn try_complete(&self) -> Option<Result<RuntimeOperationOutcome, String>> {
        match self.results.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                "Runtime owner stopped unexpectedly; inspect status and recovery before retrying"
                    .into(),
            )),
        }
    }

    pub(crate) fn shutdown(&mut self) -> io::Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.commands.take();
        match self.handle.take() {
            Some(handle) => handle
                .join()
                .map_err(|_| io::Error::other("Runtime owner stopped unexpectedly"))?,
            None => Ok(()),
        }
    }
}

impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
