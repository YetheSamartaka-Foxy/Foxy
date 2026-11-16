use super::*;
use crate::core::tasks::db_process_lock::{self, HeldClaimPin, LockOutcome};
use std::{
    collections::HashMap,
    fs::{File, TryLockError},
    sync::{Mutex, OnceLock, Weak},
};

struct RemotePermit {
    path: PathBuf,
    attachment: Attachment,
    _operation: File,
}

static PERMITS: OnceLock<Mutex<HashMap<PathBuf, Weak<RemotePermit>>>> = OnceLock::new();

pub(crate) struct WritePin {
    _local: Option<HeldClaimPin>,
    _remote: Option<Arc<RemotePermit>>,
}

pub(crate) struct MetadataPermit {
    _permit: Arc<RemotePermit>,
}

pub(crate) fn permit(context: &ServerContext) -> io::Result<Option<MetadataPermit>> {
    let Some(attachment) = attach(context)? else {
        return Ok(None);
    };
    let (directory, _, _) = location(context, false)?;
    let directory = directory.ok_or_else(|| invalid("Owner directory disappeared"))?;
    let operation = operation_file(&directory)?;
    operation.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another server metadata operation is running",
        ),
        TryLockError::Error(error) => error,
    })?;
    let snapshot = attachment.request(Operation::MetadataPermit {
        permit_id: format!("permit_{:032x}", rand::random::<u128>()),
    })?;
    if !snapshot.metadata_permit {
        return Err(invalid("Owner did not grant metadata authority"));
    }
    let path = std::path::absolute(context.database_path())?;
    let permit = Arc::new(RemotePermit {
        path: path.clone(),
        attachment,
        _operation: operation,
    });
    let mut permits = PERMITS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| invalid("Metadata permit registry unavailable"))?;
    permits.retain(|_, permit| permit.strong_count() > 0);
    if permits.get(&path).and_then(Weak::upgrade).is_some() {
        return Err(invalid("Metadata permit already retained"));
    }
    permits.insert(path, Arc::downgrade(&permit));
    Ok(Some(MetadataPermit { _permit: permit }))
}

fn remote(path: &std::path::Path) -> Result<Arc<RemotePermit>, String> {
    let path =
        std::path::absolute(path).map_err(|_| "Cannot resolve metadata authority".to_string())?;
    PERMITS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "Metadata authority is unavailable".to_string())?
        .get(&path)
        .and_then(Weak::upgrade)
        .ok_or_else(|| "An authenticated server metadata permit is required".into())
}

pub(crate) fn scope_for_write(path: &std::path::Path) -> Result<Scope, String> {
    let permit = remote(path)?;
    let snapshot = permit
        .attachment
        .inspect()
        .map_err(|_| "Server owner connection was lost".to_string())?;
    if snapshot.scope != *permit.attachment.scope()
        || permit.path
            != std::path::absolute(path).map_err(|_| "Invalid metadata path".to_string())?
    {
        return Err("Server metadata authority changed".into());
    }
    Ok(snapshot.scope)
}

pub(crate) fn pin_write(path: &std::path::Path) -> Result<WritePin, String> {
    if db_process_lock::holds(path) {
        return db_process_lock::pin_held_claim(path).map(|pin| WritePin {
            _local: Some(pin),
            _remote: None,
        });
    }
    let permit = remote(path)?;
    let snapshot = permit
        .attachment
        .inspect()
        .map_err(|_| "Server owner connection was lost".to_string())?;
    if snapshot.scope != *permit.attachment.scope()
        || permit.path
            != std::path::absolute(path).map_err(|_| "Invalid metadata path".to_string())?
    {
        return Err("Server metadata authority changed".into());
    }
    Ok(WritePin {
        _local: None,
        _remote: Some(permit),
    })
}

pub(crate) fn with_write<T>(
    path: &std::path::Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if db_process_lock::holds(path) {
        return db_process_lock::with_held_claim(path, operation);
    }
    let _pin = pin_write(path)?;
    operation()
}

pub(crate) fn claim_write(path: &std::path::Path) -> LockOutcome {
    if remote(path).is_ok() {
        LockOutcome::Acquired
    } else {
        db_process_lock::acquire(path)
    }
}
