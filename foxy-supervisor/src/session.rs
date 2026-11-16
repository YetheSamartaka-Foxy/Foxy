use crate::leases::{Admission, RuntimeLease};
use crate::process::{CommandSpec, OwnedProcess, TreeExit};
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    pub owner_id: String,
    pub owner_generation: u64,
    pub game_space_id: String,
    pub instance_id: String,
    pub operation_id: String,
    pub run_generation: u64,
}

impl SessionIdentity {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let identifier = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        };
        if self.owner_generation == 0
            || self.run_generation == 0
            || ![
                &self.owner_id,
                &self.game_space_id,
                &self.instance_id,
                &self.operation_id,
            ]
            .into_iter()
            .all(|value| identifier(value))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid foreground session identity.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Spawned,
    StopRequested,
    Terminating,
    Exited,
    CleanupUncertain,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionEvent {
    pub identity: SessionIdentity,
    pub sequence: u64,
    pub phase: SessionPhase,
    pub exit: Option<TreeExit>,
    pub readiness_verified: bool,
}

pub struct EventCursor {
    identity: SessionIdentity,
    sequence: u64,
    terminal: bool,
}

impl EventCursor {
    pub fn new(identity: SessionIdentity) -> Self {
        Self {
            identity,
            sequence: 0,
            terminal: false,
        }
    }

    pub fn accept(&mut self, event: &SessionEvent) -> bool {
        if self.terminal || event.identity != self.identity || event.sequence <= self.sequence {
            return false;
        }
        if (event.phase == SessionPhase::Exited) != event.exit.is_some() {
            return false;
        }
        self.sequence = event.sequence;
        self.terminal = event.phase == SessionPhase::Exited;
        true
    }
}

pub struct ForegroundSession {
    process: OwnedProcess,
    lease: Option<RuntimeLease>,
    registration: Option<crate::recovery::Registration>,
    event: SessionEvent,
}

impl ForegroundSession {
    pub fn spawn(
        identity: SessionIdentity,
        spec: &CommandSpec,
        admission: Admission<'_>,
        cancelled: &AtomicBool,
        revalidate: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::spawn_inner(identity, spec, None, None, admission, cancelled, revalidate)
    }

    pub fn spawn_controlled(
        identity: SessionIdentity,
        spec: &CommandSpec,
        output: &std::fs::File,
        admission: Admission<'_>,
        cancelled: &AtomicBool,
        revalidate: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::spawn_inner(
            identity,
            spec,
            Some(output),
            None,
            admission,
            cancelled,
            revalidate,
        )
    }

    pub fn spawn_controlled_witnessed(
        identity: SessionIdentity,
        spec: &CommandSpec,
        output: &std::fs::File,
        witness: &crate::process::CleanupWitnessCommand,
        admission: Admission<'_>,
        cancelled: &AtomicBool,
        revalidate: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::spawn_inner(
            identity,
            spec,
            Some(output),
            Some(witness),
            admission,
            cancelled,
            revalidate,
        )
    }

    fn spawn_inner(
        identity: SessionIdentity,
        spec: &CommandSpec,
        output: Option<&std::fs::File>,
        witness: Option<&crate::process::CleanupWitnessCommand>,
        admission: Admission<'_>,
        cancelled: &AtomicBool,
        revalidate: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        identity.validate()?;
        let lease = admission.try_runtime()?;
        if cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Foreground start cancelled before spawn.",
            ));
        }
        let mut registration = if let Some(witness) = witness {
            crate::recovery::Registration::create_witnessed(
                &admission,
                &identity,
                &witness.group_id,
                &witness.context_key,
            )?
        } else {
            crate::recovery::Registration::create(&admission, &identity)?
        };
        if let Err(error) = revalidate() {
            registration.retire_unspawned()?;
            return Err(error);
        }
        if cancelled.load(Ordering::Acquire) {
            registration.retire_unspawned()?;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Foreground start cancelled before spawn.",
            ));
        }
        let process = match (output, witness) {
            (Some(output), Some(command)) => OwnedProcess::spawn_controlled_witnessed(
                spec,
                output,
                &crate::process::CleanupWitnessSpec {
                    command,
                    receipt: registration.receipt()?,
                },
            )?,
            (None, Some(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "A cleanup witness requires controlled output",
                ));
            }
            (Some(output), None) => OwnedProcess::spawn_controlled(spec, output)?,
            (None, None) => OwnedProcess::spawn(spec)?,
        };
        let session = Self {
            process,
            lease: Some(lease),
            registration: Some(registration),
            event: SessionEvent {
                identity,
                sequence: 1,
                phase: SessionPhase::Spawned,
                exit: None,
                readiness_verified: false,
            },
        };
        drop(admission);
        Ok(session)
    }

    pub fn process_id(&self) -> u32 {
        self.process.id()
    }

    pub fn event(&self) -> &SessionEvent {
        &self.event
    }

    fn phase(&mut self, phase: SessionPhase) {
        if self.event.phase != phase {
            self.event.phase = phase;
            self.event.sequence += 1;
        }
    }

    pub fn request_stop(&mut self) {
        if self.event.exit.is_none()
            && !matches!(
                self.event.phase,
                SessionPhase::Terminating | SessionPhase::CleanupUncertain
            )
        {
            self.phase(SessionPhase::StopRequested);
        }
    }

    pub fn deliver_soft_stop(
        &mut self,
        helper: Option<&crate::process::ConsoleSignalHelper>,
    ) -> io::Result<()> {
        self.request_stop();
        if self.event.exit.is_none() {
            self.process.deliver_soft_stop(helper)
        } else {
            Ok(())
        }
    }

    fn reconcile(&mut self, result: io::Result<Option<TreeExit>>) -> io::Result<Option<TreeExit>> {
        match result {
            Ok(Some(exit)) => {
                if let Some(registration) = &mut self.registration
                    && let Err(error) = registration.retire()
                {
                    self.phase(SessionPhase::CleanupUncertain);
                    return Err(error);
                }
                self.registration.take();
                self.event.exit = Some(exit);
                self.phase(SessionPhase::Exited);
                self.lease.take();
                Ok(Some(exit))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.phase(SessionPhase::CleanupUncertain);
                Err(error)
            }
        }
    }

    pub fn observe(&mut self) -> io::Result<Option<TreeExit>> {
        let result = self.process.observe();
        self.reconcile(result)
    }

    pub fn wait(&mut self, timeout: Duration) -> io::Result<Option<TreeExit>> {
        let result = self.process.wait(timeout);
        self.reconcile(result)
    }

    pub fn force_stop(&mut self, timeout: Duration) -> io::Result<Option<TreeExit>> {
        if self.event.exit.is_some() {
            return Ok(self.event.exit);
        }
        self.phase(SessionPhase::Terminating);
        let result = self.process.terminate_and_wait(timeout);
        self.reconcile(result)
    }
}

impl Drop for ForegroundSession {
    fn drop(&mut self) {
        if self.lease.is_some() && !matches!(self.force_stop(Duration::from_secs(5)), Ok(Some(_))) {
            // Uncertain writers must not unlock maintenance while this owner is alive.
            if let Some(lease) = self.lease.take() {
                std::mem::forget(lease);
            }
        }
    }
}

#[cfg(test)]
mod tests;
