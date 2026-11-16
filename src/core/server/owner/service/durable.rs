use super::*;

pub(super) const FOREGROUND: u8 = 0;
pub(super) const PARENT_BOUND: u8 = 1;
pub(super) const DETACHED: u8 = 2;

pub(super) fn launch_id() -> String {
    format!("launch_{:032x}", rand::random::<u128>())
}

pub(super) fn valid_launch_id(value: &str) -> bool {
    value.strip_prefix("launch_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(super) fn isolate_owner_process(command: &mut Command) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        use winapi::um::handleapi::{INVALID_HANDLE_VALUE, SetHandleInformation};
        use winapi::um::winbase::HANDLE_FLAG_INHERIT;
        // A detached owner must not retain the controller's captured output pipes.
        for handle in [
            std::io::stdin().as_raw_handle(),
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ] {
            if !handle.is_null()
                && handle != INVALID_HANDLE_VALUE.cast()
                && unsafe { SetHandleInformation(handle.cast(), HANDLE_FLAG_INHERIT, 0) } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        command.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // The child performs only setsid before exec; it does not access inherited Foxy state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    Ok(())
}

pub(super) fn launch_is_isolated() -> bool {
    #[cfg(windows)]
    {
        unsafe { winapi::um::wincon::GetConsoleWindow() }.is_null()
    }
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::getsid(0) == libc::getpid() }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}

fn wait_for_child_exit(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn parent_eof_stops(lifetime: OwnerLifetime, parent_eof: bool) -> bool {
    parent_eof && lifetime == OwnerLifetime::ParentBound
}

pub(super) fn detach_foreground(owner: &mut ForegroundOwner) -> io::Result<OwnerSnapshot> {
    if owner.child.is_none() {
        return Err(invalid("This owner was not prepared for detachment"));
    }
    let current = owner.attachment.inspect()?;
    let launch_id = current
        .launch_id
        .as_deref()
        .ok_or_else(|| invalid("This owner was not prepared for detachment"))?;
    if current.lifetime == OwnerLifetime::Detached {
        owner.lifetime.take();
        owner.child.take();
        return Ok(current);
    }
    if current.lifetime != OwnerLifetime::ParentBound {
        return Err(invalid("Owner is not parent-bound"));
    }
    let detached = owner.attachment.detach_owner()?;
    if detached.scope != current.scope
        || detached.launch_id.as_deref() != Some(launch_id)
        || detached.lifetime != OwnerLifetime::Detached
    {
        return Err(invalid("Owner detachment was not confirmed"));
    }
    owner.lifetime.take();
    owner.child.take();
    Ok(detached)
}

pub(crate) fn start_detached(context: &ServerContext) -> io::Result<OwnerSnapshot> {
    if let Ok(Some(existing)) = attach(context) {
        let snapshot = existing.inspect()?;
        if snapshot.lifetime == OwnerLifetime::Detached {
            return Ok(snapshot);
        }
        if snapshot.lifetime == OwnerLifetime::ParentBound
            && let Some(launch_id) = snapshot.launch_id
        {
            return promote_until_confirmed(context, existing, launch_id, None, None);
        }
        return Err(invalid("Existing owner was not prepared for detachment"));
    }

    let launch_id = launch_id();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--config-dir")
        .arg(context.data_root())
        .args([
            "server",
            "supervise",
            "--game-space",
            context.space_id(),
            "--parent-bound",
            "--launch-id",
            launch_id.as_str(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    isolate_owner_process(&mut command)?;
    let mut child = command.spawn()?;
    let stdin = child.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(io::Error::other(format!(
                    "Prepared owner exited before readiness ({status})"
                )));
            }
            Ok(None) => {}
            Err(error) => {
                drop(stdin);
                wait_for_child_exit(&mut child);
                return Err(error);
            }
        }
        if let Ok(Some(attachment)) = attach(context)
            && let Ok(snapshot) = attachment.inspect()
            && snapshot.launch_id.as_deref() == Some(launch_id.as_str())
        {
            if snapshot.lifetime == OwnerLifetime::Detached {
                drop(stdin);
                drop(child);
                return Ok(snapshot);
            }
            return promote_until_confirmed(
                context,
                attachment,
                launch_id.clone(),
                Some(child),
                stdin,
            );
        }
        if Instant::now() >= deadline {
            drop(stdin);
            wait_for_child_exit(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Prepared owner readiness timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn promote_until_confirmed(
    context: &ServerContext,
    initial: Attachment,
    expected_launch: String,
    mut child: Option<Child>,
    stdin: Option<ChildStdin>,
) -> io::Result<OwnerSnapshot> {
    let expected_scope = initial.scope().clone();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Instant::now() >= deadline {
            if let Some(snapshot) = inspect_exact(context, &expected_scope, &expected_launch)
                && snapshot.lifetime == OwnerLifetime::Detached
            {
                drop(stdin);
                drop(child);
                return Ok(snapshot);
            }
            if let Some(child) = child.as_mut() {
                drop(stdin);
                wait_for_child_exit(child);
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owner detachment outcome remains uncertain",
            ));
        }
        if let Some(child) = child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(io::Error::other(format!(
                        "Prepared owner exited before detachment ({status})"
                    )));
                }
                Ok(None) => {}
                Err(error) => {
                    drop(stdin);
                    wait_for_child_exit(child);
                    return Err(error);
                }
            }
        }
        let attachment = match attach(context) {
            Ok(Some(attachment)) if attachment.scope() == &expected_scope => attachment,
            Ok(Some(_)) => {
                drop(stdin);
                if let Some(child) = child.as_mut() {
                    wait_for_child_exit(child);
                }
                return Err(invalid("Prepared owner ended before detachment"));
            }
            Ok(None) if child.is_none() => {
                return Err(invalid("Prepared owner ended before detachment"));
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
        };
        let snapshot = match attachment.inspect() {
            Ok(snapshot) if snapshot.scope == expected_scope => snapshot,
            _ => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
        };
        if snapshot.launch_id.as_deref() != Some(expected_launch.as_str()) {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        if snapshot.lifetime == OwnerLifetime::Detached {
            drop(stdin);
            drop(child);
            return Ok(snapshot);
        }
        match attachment.detach_owner() {
            Ok(detached)
                if detached.scope == expected_scope
                    && detached.launch_id.as_deref() == Some(expected_launch.as_str())
                    && detached.lifetime == OwnerLifetime::Detached =>
            {
                drop(stdin);
                drop(child);
                return Ok(detached);
            }
            _ => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn inspect_exact(
    context: &ServerContext,
    expected_scope: &foxy_supervisor::control::Scope,
    expected_launch: &str,
) -> Option<OwnerSnapshot> {
    let attachment = attach(context).ok()??;
    if attachment.scope() != expected_scope {
        return None;
    }
    let snapshot = attachment.inspect().ok()?;
    (snapshot.scope == *expected_scope && snapshot.launch_id.as_deref() == Some(expected_launch))
        .then_some(snapshot)
}

impl Attachment {
    pub(crate) fn detach_owner(&self) -> io::Result<OwnerSnapshot> {
        self.request(Operation::DetachOwner {})
    }

    pub(crate) fn shutdown_owner(&self) -> io::Result<OwnerSnapshot> {
        self.request(Operation::ShutdownOwner {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_pipe_eof_only_stops_parent_bound_owners() {
        assert!(parent_eof_stops(OwnerLifetime::ParentBound, true));
        assert!(!parent_eof_stops(OwnerLifetime::Detached, true));
        assert!(!parent_eof_stops(OwnerLifetime::Foreground, true));
        assert!(!parent_eof_stops(OwnerLifetime::ParentBound, false));
    }

    #[test]
    fn launch_identity_requires_fixed_prefix_and_hex_payload() {
        assert!(valid_launch_id(&format!("launch_{}", "a".repeat(32))));
        assert!(!valid_launch_id("launch_short"));
        assert!(!valid_launch_id(&format!("launch_{}", "g".repeat(32))));
    }
}
