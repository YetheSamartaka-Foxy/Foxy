use super::*;
use std::fs::File;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    next: u64,
}

pub(super) struct RequestIdentity {
    pub(super) client_id: &'static str,
    pub(super) request_id: u64,
    _lease: File,
}

pub(crate) struct IdleGuard {
    _lease: File,
}

impl Attachment {
    pub(crate) fn idle_guard(&self) -> io::Result<IdleGuard> {
        let directory = ProtectedDirectory::open(&self.runtime_directory)?;
        let lease = coordination_file(&directory, "controller.lock", b"foxy-controller-lock-v1")?;
        lease.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "A foreground controller operation is running",
            )
        })?;
        if self.inspect()?.active_sessions != 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Stop owned server sessions and verify cleanup before changing mode or game space.",
            ));
        }
        Ok(IdleGuard { _lease: lease })
    }
    pub(super) fn request_identity(&self) -> io::Result<RequestIdentity> {
        let directory = ProtectedDirectory::open(&self.runtime_directory)?;
        allocate(&directory)
    }
}

fn allocate(directory: &ProtectedDirectory) -> io::Result<RequestIdentity> {
    let lease = coordination_file(directory, "controller.lock", b"foxy-controller-lock-v1")?;
    lease.try_lock().map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another foreground controller operation is running",
        )
    })?;
    let cursor = match directory.read("controller-cursor.json") {
        Ok(bytes) => serde_json::from_slice::<Cursor>(&bytes)
            .map_err(|_| invalid("Invalid controller request cursor"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Cursor {
            version: 1,
            next: 1,
        },
        Err(error) => return Err(error),
    };
    if cursor.version != 1 || cursor.next == 0 {
        return Err(invalid("Unsupported controller request cursor"));
    }
    let next = cursor
        .next
        .checked_add(2)
        .ok_or_else(|| invalid("Controller request identities are exhausted"))?;
    let bytes = serde_json::to_vec(&Cursor { version: 1, next })
        .map_err(|_| invalid("Controller cursor encoding failed"))?;
    let temporary = format!("controller-{:032x}.json", rand::random::<u128>());
    directory.publish_new(&temporary, &bytes)?;
    if let Err(error) = std::fs::rename(
        directory.path().join(&temporary),
        directory.path().join("controller-cursor.json"),
    ) {
        let _ = directory.remove_exact(&temporary, &bytes);
        return Err(error);
    }
    Ok(RequestIdentity {
        client_id: "foreground_controller_v1",
        request_id: cursor.next,
        _lease: lease,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_cursor_is_serialized_and_persists_monotone_request_pairs() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = ProtectedDirectory::create(&temporary.path().join("controller")).unwrap();
        let first = allocate(&directory).unwrap();
        assert_eq!(first.request_id, 1);
        assert_eq!(
            allocate(&directory).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        for expected in (3..=85).step_by(2) {
            let identity = allocate(&directory).unwrap();
            assert_eq!(identity.request_id, expected);
            assert_eq!(identity.client_id, "foreground_controller_v1");
        }
    }

    #[test]
    fn invalid_controller_cursor_is_preserved_without_reusing_a_request() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = ProtectedDirectory::create(&temporary.path().join("controller")).unwrap();
        directory
            .publish_new("controller-cursor.json", br#"{"version":2,"next":1}"#)
            .unwrap();
        assert!(allocate(&directory).is_err());
        assert_eq!(
            directory.read("controller-cursor.json").unwrap(),
            br#"{"version":2,"next":1}"#
        );
    }
}
