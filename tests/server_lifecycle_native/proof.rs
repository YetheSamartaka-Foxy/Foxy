use std::fs;
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::GAME;
#[path = "../server_monitoring_native/proof.rs"]
mod monitoring;

pub(super) fn wait_for_mission(
    config: &Path,
    id: &str,
    identity: (&str, u16),
    owner_id: &str,
    run_generation: u64,
    timeout: Duration,
) -> io::Result<()> {
    let (identity, query_port) = identity;
    let state = config
        .join("games")
        .join(GAME)
        .join("server/instances")
        .join(id)
        .join("deployments/runtime-state");
    let expected_prefix = format!("FOXY_NATIVE|{identity}|mission|{identity}|true|false|");
    let owned_log = state.join(format!("run_{owner_id}_{run_generation}.log"));
    let deadline = Instant::now() + timeout;
    let mut log_started = None;
    while Instant::now() < deadline {
        let owned = match fs::symlink_metadata(&owned_log) {
            Ok(metadata) if metadata.is_file() && !is_link(&metadata) => metadata,
            Ok(_) => {
                return Err(io::Error::other(
                    "Current generation owner log is not an ordinary file",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(error),
        };
        if owned.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other(
                "Current generation owner log exceeded size bound",
            ));
        }
        let owner_started =
            *log_started.get_or_insert(owned.created().or_else(|_| owned.modified())?);
        let bytes = fs::read(&owned_log)?;
        let console = String::from_utf8_lossy(&bytes);
        if console.contains(&expected_prefix) {
            eprintln!("Fresh native owned-run marker observed for {identity}");
            monitoring::wait(
                config,
                id,
                identity,
                owner_id,
                run_generation,
                (Ipv4Addr::LOCALHOST, query_port).into(),
                super::invoke_cli,
            )?;
            return Ok(());
        }
        let mut stack = vec![state.clone()];
        let mut count = 0usize;
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(&directory)? {
                count += 1;
                if count > 4096 {
                    return Err(io::Error::other(
                        "Native runtime output inventory exceeded bound",
                    ));
                }
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                if is_link(&metadata) {
                    return Err(io::Error::other("Native runtime output contains a link"));
                }
                if metadata.is_dir() {
                    stack.push(entry.path());
                } else if metadata.is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("rpt"))
                {
                    if metadata.modified()? < owner_started {
                        continue;
                    }
                    if metadata.len() > 16 * 1024 * 1024 {
                        return Err(io::Error::other(
                            "Native runtime report exceeded size bound",
                        ));
                    }
                    let bytes = fs::read(entry.path())?;
                    let report = String::from_utf8_lossy(&bytes);
                    let exact_content = report.contains(&format!("{expected_prefix}true|true"))
                        || report.contains(&format!("{expected_prefix}false|false"));
                    if exact_content {
                        eprintln!("Fresh native mission marker observed for {identity}");
                        monitoring::wait(
                            config,
                            id,
                            identity,
                            owner_id,
                            run_generation,
                            (Ipv4Addr::LOCALHOST, query_port).into(),
                            super::invoke_cli,
                        )?;
                        return Ok(());
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err(io::Error::other(
        "Fresh native engine output did not contain the exact mission identity before deadline",
    ))
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn query_info(port: u16, hostname: &str) -> bool {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) else {
        return false;
    };
    if socket.connect((Ipv4Addr::LOCALHOST, port)).is_err()
        || socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .is_err()
        || socket
            .set_write_timeout(Some(Duration::from_millis(100)))
            .is_err()
    {
        return false;
    }
    let initial = b"\xff\xff\xff\xffTSource Engine Query\0";
    let mut request = initial.to_vec();
    let mut buffer = [0; 4096];
    for _ in 0..2 {
        if socket.send(&request).is_err() {
            return false;
        }
        let Ok(length) = socket.recv(&mut buffer) else {
            return false;
        };
        let reply = &buffer[..length];
        if length == 9 && reply.starts_with(b"\xff\xff\xff\xffA") {
            request = initial.to_vec();
            request.extend_from_slice(&reply[5..]);
        } else {
            return info_matches(reply, hostname.as_bytes());
        }
    }
    false
}

fn info_matches(mut reply: &[u8], hostname: &[u8]) -> bool {
    if !reply.starts_with(b"\xff\xff\xff\xffI") || reply.len() < 6 {
        return false;
    }
    reply = &reply[6..];
    for index in 0..4 {
        let Some(end) = reply.iter().position(|byte| *byte == 0) else {
            return false;
        };
        if end > 256 || (index == 0 && &reply[..end] != hostname) {
            return false;
        }
        reply = &reply[end + 1..];
    }
    true
}
