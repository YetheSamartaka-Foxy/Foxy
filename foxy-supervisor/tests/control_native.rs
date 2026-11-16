#![cfg(any(windows, target_os = "linux"))]

use foxy_supervisor::control::transport::{IoBudget, Listener, LocalEndpoint, connect};
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn endpoint(directory: &TempDir) -> LocalEndpoint {
    let suffix = format!("{:016x}", rand::random::<u64>());
    #[cfg(windows)]
    {
        let _ = directory;
        LocalEndpoint::NamedPipe(format!(r"\\.\pipe\foxy-control-{suffix}"))
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        LocalEndpoint::UnixSocket(directory.path().join(format!("c-{suffix}")))
    }
}

fn budget<'a>(cancelled: &'a AtomicBool, duration: Duration) -> IoBudget<'a> {
    IoBudget {
        deadline: Instant::now() + duration,
        cancelled,
    }
}

#[test]
fn accept_and_blocked_read_are_bounded_and_listener_recovers() {
    let directory = tempfile::Builder::new()
        .prefix("f-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let endpoint = endpoint(&directory);
    let mut listener = Listener::bind(&endpoint).unwrap();

    let not_cancelled = AtomicBool::new(false);
    let timeout = listener.accept(budget(&not_cancelled, Duration::from_millis(40)));
    assert_eq!(timeout.err().unwrap().kind(), io::ErrorKind::TimedOut);

    let client_endpoint = endpoint.clone();
    let client_worker = std::thread::spawn(move || {
        connect(
            &client_endpoint,
            budget(&AtomicBool::new(false), Duration::from_secs(2)),
        )
    });
    let mut server_connection = listener
        .accept(budget(&not_cancelled, Duration::from_secs(1)))
        .unwrap();
    let raw_client = client_worker.join().unwrap().unwrap();
    let cancellation = Arc::new(AtomicBool::new(false));
    let cancellation_worker = Arc::clone(&cancellation);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        cancellation_worker.store(true, Ordering::Release);
    });
    ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    let mut byte = [0_u8; 1];
    let read = server_connection.read_exact(
        &mut byte,
        IoBudget {
            deadline: Instant::now() + Duration::from_secs(2),
            cancelled: &cancellation,
        },
    );
    assert_eq!(read.err().unwrap().kind(), io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(2));
    worker.join().unwrap();

    drop(server_connection);
    drop(raw_client);
    let client_endpoint = endpoint.clone();
    let client_worker = std::thread::spawn(move || {
        connect(
            &client_endpoint,
            budget(&AtomicBool::new(false), Duration::from_secs(2)),
        )
    });
    let mut server_connection = listener
        .accept(budget(&not_cancelled, Duration::from_secs(1)))
        .unwrap();
    let mut raw_client = client_worker.join().unwrap().unwrap();
    raw_client
        .write_all(b"ok", budget(&not_cancelled, Duration::from_secs(1)))
        .unwrap();
    let mut received = [0_u8; 2];
    server_connection
        .read_exact(
            &mut received,
            budget(&not_cancelled, Duration::from_secs(1)),
        )
        .unwrap();
    assert_eq!(&received, b"ok");
    drop(server_connection);
    drop(raw_client);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_bind_rejects_unsafe_parents_and_preserves_unowned_paths() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory = tempfile::Builder::new()
        .prefix("f-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let unsafe_parent = directory.path().join("public");
    std::fs::create_dir(&unsafe_parent).unwrap();
    std::fs::set_permissions(&unsafe_parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let unsafe_endpoint = LocalEndpoint::UnixSocket(unsafe_parent.join("socket"));
    assert!(Listener::bind(&unsafe_endpoint).is_err());

    let safe_parent = directory.path().join("safe");
    std::fs::create_dir(&safe_parent).unwrap();
    std::fs::set_permissions(&safe_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let linked_parent = directory.path().join("linked");
    symlink(&safe_parent, &linked_parent).unwrap();
    assert!(Listener::bind(&LocalEndpoint::UnixSocket(linked_parent.join("socket"))).is_err());

    let existing_file = safe_parent.join("existing");
    std::fs::write(&existing_file, b"preserve-existing").unwrap();
    assert!(Listener::bind(&LocalEndpoint::UnixSocket(existing_file.clone())).is_err());
    assert_eq!(std::fs::read(&existing_file).unwrap(), b"preserve-existing");

    let replaced_path = safe_parent.join("replacement");
    let listener = Listener::bind(&LocalEndpoint::UnixSocket(replaced_path.clone())).unwrap();
    std::fs::remove_file(&replaced_path).unwrap();
    std::fs::write(&replaced_path, b"preserve-replacement").unwrap();
    drop(listener);
    assert_eq!(
        std::fs::read(&replaced_path).unwrap(),
        b"preserve-replacement"
    );
}

#[cfg(windows)]
#[test]
fn windows_anonymous_peer_cannot_open_the_current_user_pipe() {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;
    use winapi::shared::minwindef::DWORD;
    use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::processthreadsapi::GetCurrentThread;
    use winapi::um::securitybaseapi::{ImpersonateAnonymousToken, RevertToSelf};
    use winapi::um::winbase::{FILE_FLAG_OVERLAPPED, SECURITY_ANONYMOUS, SECURITY_SQOS_PRESENT};
    use winapi::um::winnt::{GENERIC_READ, GENERIC_WRITE};

    let directory = tempfile::Builder::new()
        .prefix("f-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let endpoint = endpoint(&directory);
    let LocalEndpoint::NamedPipe(name) = &endpoint else {
        unreachable!()
    };
    let listener = Listener::bind(&endpoint).unwrap();
    let pipe = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let observed = std::thread::spawn(move || unsafe {
        let impersonated = ImpersonateAnonymousToken(GetCurrentThread()) != 0;
        let outcome = if impersonated {
            let handle = CreateFileW(
                pipe.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
                null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                Err(io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or_default() as DWORD)
            } else {
                CloseHandle(handle);
                Ok(0)
            }
        } else {
            Err(io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or_default() as DWORD)
        };
        if RevertToSelf() == 0 {
            std::process::abort();
        }
        (impersonated, outcome)
    })
    .join()
    .unwrap();
    drop(listener);
    assert!(
        observed.0,
        "anonymous-token fixture must actually impersonate"
    );
    assert_eq!(
        observed.1.unwrap_err(),
        winapi::shared::winerror::ERROR_ACCESS_DENIED,
    );
}
