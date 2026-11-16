mod controller;
mod deployment;
mod lifecycle;
mod maintenance;
mod permit;
mod runtime;
mod service;
mod update_gate;
mod worker;
pub(crate) use controller::IdleGuard;
pub(crate) use deployment::DeploymentExecution;
pub(crate) use update_gate::{UpdateBlocker, UpdateBlockerKind, update_blockers};

pub(crate) use permit::{
    MetadataPermit, claim_write, permit, pin_write, scope_for_write, with_write,
};
#[cfg(test)]
pub(crate) use service::supervise;
pub(crate) use service::{ForegroundOwner, start_detached, start_or_attach, supervise_with_launch};

use super::ServerContext;
use foxy_supervisor::control::{
    Capability, ClientMessage, ClientSession, Operation, Request, RequestReply, Scope,
    ServerMessage,
    handoff::ProtectedDirectory,
    transport::{self, IoBudget, LocalEndpoint},
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const RECORD_VERSION: u32 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IDENTITY: &str = "space_catalog";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerRecord {
    version: u32,
    scope: Scope,
    endpoint: String,
    capability: [u8; 32],
}

impl OwnerRecord {
    fn endpoint(&self) -> io::Result<LocalEndpoint> {
        #[cfg(windows)]
        {
            Ok(LocalEndpoint::NamedPipe(self.endpoint.clone()))
        }
        #[cfg(target_os = "linux")]
        {
            Ok(LocalEndpoint::UnixSocket(self.endpoint.clone().into()))
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Owner transport unavailable",
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnerLifetime {
    Foreground,
    ParentBound,
    Detached,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerSnapshot {
    pub scope: Scope,
    pub game_id: String,
    pub database_claimed: bool,
    pub database_opened: bool,
    pub metadata_permit: bool,
    pub lifecycle_available: bool,
    pub lifetime: OwnerLifetime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
    #[serde(default)]
    pub active_sessions: u32,
}

#[derive(Clone)]
pub(crate) struct Attachment {
    record: Arc<OwnerRecord>,
    runtime_directory: PathBuf,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("scope", &self.record.scope)
            .finish_non_exhaustive()
    }
}

impl Attachment {
    pub(crate) fn scope(&self) -> &Scope {
        &self.record.scope
    }

    pub(crate) fn inspect(&self) -> io::Result<OwnerSnapshot> {
        self.request(Operation::Inspect {
            expected_revision: None,
        })
    }

    pub(crate) fn replay_events(
        &self,
        after: u64,
    ) -> io::Result<foxy_supervisor::control::EventBatch> {
        let cancelled = AtomicBool::new(false);
        let budget = IoBudget {
            deadline: Instant::now() + CONNECT_TIMEOUT,
            cancelled: &cancelled,
        };
        let connection = transport::connect(&self.record.endpoint()?, budget)?;
        let capability = Capability::from_bytes(self.record.capability)?;
        let mut session = ClientSession::authenticate(
            connection,
            &self.record.scope,
            &capability,
            "metadata_controller",
            after,
            budget,
        )?;
        session.send(&ClientMessage::Events { after }, budget)?;
        let ServerMessage::Events { batch } = session.receive(budget)? else {
            return Err(invalid("Owner event replay was not returned"));
        };
        if batch.after != after {
            return Err(invalid("Owner replay cursor does not match the request"));
        }
        session.send(&ClientMessage::Close, budget)?;
        if session.receive(budget)? != ServerMessage::Closed {
            return Err(invalid("Owner replay close was not confirmed"));
        }
        Ok(batch)
    }

    fn request(&self, operation: Operation) -> io::Result<OwnerSnapshot> {
        let cancelled = AtomicBool::new(false);
        let budget = IoBudget {
            deadline: Instant::now() + CONNECT_TIMEOUT,
            cancelled: &cancelled,
        };
        match operation {
            Operation::Inspect { .. }
            | Operation::MetadataPermit { .. }
            | Operation::DetachOwner {}
            | Operation::ShutdownOwner {} => {}
            _ => {
                return Err(invalid(
                    "Deployment effects require their retained operation path",
                ));
            }
        }
        let request_id = rand::random::<u64>().max(1);
        loop {
            match self.request_once(operation.clone(), request_id, budget) {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) if transient_connection(&error) => {
                    std::thread::sleep(budget.remaining()?.min(Duration::from_millis(10)));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn request_once(
        &self,
        operation: Operation,
        request_id: u64,
        budget: IoBudget<'_>,
    ) -> io::Result<OwnerSnapshot> {
        let endpoint = self.record.endpoint()?;
        let connection = transport::connect(&endpoint, budget)?;
        let capability = Capability::from_bytes(self.record.capability)?;
        // Each connection is acknowledged and closed before returning; these read/permit
        // requests carry no repeatable file mutation or engine effect.
        let mut session = ClientSession::authenticate(
            connection,
            &self.record.scope,
            &capability,
            "metadata_controller",
            0,
            budget,
        )?;
        session.send(
            &ClientMessage::Submit {
                request: Request {
                    request_id,
                    operation,
                },
            },
            budget,
        )?;
        let ServerMessage::Request {
            reply:
                RequestReply::Complete {
                    request_id: actual,
                    result,
                },
        } = session.receive(budget)?
        else {
            return Err(invalid("Owner request was not completed"));
        };
        if actual != request_id || !result.succeeded {
            return Err(invalid("Owner refused metadata admission"));
        }
        let snapshot: OwnerSnapshot = serde_json::from_slice(&result.payload)
            .map_err(|_| invalid("Invalid owner snapshot"))?;
        if snapshot.scope != self.record.scope
            || !snapshot.database_claimed
            || snapshot.database_opened
            || !valid_lifetime_snapshot(&snapshot)
        {
            return Err(invalid(
                "Owner snapshot does not match the requested authority",
            ));
        }
        session.send(&ClientMessage::Close, budget)?;
        if session.receive(budget)? != ServerMessage::Closed {
            return Err(invalid("Owner close was not confirmed"));
        }
        Ok(snapshot)
    }
}

fn valid_lifetime_snapshot(snapshot: &OwnerSnapshot) -> bool {
    let valid_launch = snapshot.launch_id.as_deref().is_none_or(|id| {
        id.strip_prefix("launch_").is_some_and(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    });
    valid_launch
        && match snapshot.lifetime {
            OwnerLifetime::Foreground => snapshot.launch_id.is_none(),
            OwnerLifetime::ParentBound => true,
            OwnerLifetime::Detached => snapshot.launch_id.is_some(),
        }
}

fn transient_connection(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    ) {
        return true;
    }
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(code) if code == winapi::shared::winerror::ERROR_NO_DATA as i32
        || code == winapi::shared::winerror::ERROR_PIPE_NOT_CONNECTED as i32
        || code == winapi::shared::winerror::ERROR_BROKEN_PIPE as i32)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub(crate) fn attach(context: &ServerContext) -> io::Result<Option<Attachment>> {
    let (directory, name, key) = location(context, false)?;
    let Some(directory) = directory else {
        return Ok(None);
    };
    let bytes = match directory.read(&name) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let record: OwnerRecord =
        serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid protected owner record"))?;
    if record.version != RECORD_VERSION
        || record.scope.context_key != key
        || record.scope.space_id != context.space_id()
        || record.scope.instance_id != IDENTITY
    {
        return Err(invalid("Protected owner record belongs to another context"));
    }
    record.scope.validate()?;
    let attachment = Attachment {
        record: Arc::new(record),
        runtime_directory: directory.path().to_path_buf(),
    };
    let snapshot = attachment.inspect().map_err(unreachable_owner)?;
    if snapshot.game_id != context.game_id() {
        return Err(invalid("Owner game module mismatch"));
    }
    Ok(Some(attachment))
}

/// An owner that died leaves its protected record; its missing endpoint is a fact to report, not
/// authority to remove the record or anything the owner held.
fn unreachable_owner(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => io::Error::new(
            io::ErrorKind::NotFound,
            "The recorded owner for this game space is not running. Start a new owner with server supervise, then review crash recovery with server instance recovery-plan.",
        ),
        _ => error,
    }
}

fn context_key(context: &ServerContext) -> io::Result<String> {
    let root = context.data_root().canonicalize()?;
    let mut hash = blake3::Hasher::new();
    hash.update(root.as_os_str().as_encoded_bytes());
    hash.update(&[0]);
    hash.update(context.space_id().as_bytes());
    hash.update(&[0]);
    hash.update(context.game_id().as_bytes());
    Ok(hash.finalize().to_hex().to_string())
}

fn location(
    context: &ServerContext,
    create: bool,
) -> io::Result<(Option<ProtectedDirectory>, String, String)> {
    let key = context_key(context)?;
    #[cfg(windows)]
    let base = std::env::temp_dir().join(format!("foxy-owner-{}", &key[..24]));
    #[cfg(target_os = "linux")]
    let base = std::path::Path::new("/tmp").join(format!(
        "foxy-owner-{}-{}",
        unsafe { libc::geteuid() },
        &key[..16]
    ));
    #[cfg(not(any(windows, target_os = "linux")))]
    let base = PathBuf::new();
    let base = base
        .parent()
        .ok_or_else(|| invalid("Invalid owner runtime directory"))?
        .canonicalize()?
        .join(
            base.file_name()
                .ok_or_else(|| invalid("Invalid owner runtime directory"))?,
        );
    let data_root = context.data_root().canonicalize()?;
    if base.starts_with(&data_root) || data_root.starts_with(&base) {
        return Err(invalid(
            "Owner runtime directory overlaps configuration data",
        ));
    }
    if let Some(local) = dirs::data_local_dir() {
        let local = local.canonicalize().unwrap_or(local);
        let registry = local.join("Foxy/server-installation-locks");
        if base.starts_with(&registry) || registry.starts_with(&base) {
            return Err(invalid(
                "Owner runtime directory overlaps installation coordination",
            ));
        }
    }
    let directory = if create {
        Some(ProtectedDirectory::create(&base)?)
    } else {
        match ProtectedDirectory::open(&base) {
            Ok(directory) => Some(directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        }
    };
    Ok((directory, "owner.json".into(), key))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn operation_file(directory: &ProtectedDirectory) -> io::Result<std::fs::File> {
    coordination_file(directory, "operation.lock", b"foxy-metadata-operation-v1")
}

fn coordination_file(
    directory: &ProtectedDirectory,
    name: &str,
    marker: &[u8],
) -> io::Result<std::fs::File> {
    let path = directory.path().join(name);
    match directory.publish_new(name, marker) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let bytes = directory.read(name).map_err(|error| {
                if error.raw_os_error() == Some(33) {
                    io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "A metadata operation retains ownership",
                    )
                } else {
                    error
                }
            })?;
            if bytes != marker {
                return Err(invalid("Unknown metadata operation lock"));
            }
        }
        Err(error) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(winapi::um::winbase::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(test)]
mod tests;
