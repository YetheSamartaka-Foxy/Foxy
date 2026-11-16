use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::handleapi::GetHandleInformation;
use winapi::um::jobapi2::TerminateJobObject;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{WAIT_FAILED, WAIT_OBJECT_0};
use winapi::um::winnt::HANDLE;

const POLL_MS: DWORD = 25;

fn handle(value: u64) -> io::Result<HANDLE> {
    let raw = usize::try_from(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid witness handle."))?;
    if raw == 0 || raw > isize::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid witness handle.",
        ));
    }
    let raw = raw as HANDLE;
    let mut flags = 0;
    if unsafe { GetHandleInformation(raw, &mut flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(raw)
}

pub(in crate::process) fn run_cleanup_witness(
    owner: u64,
    tree: u64,
    receipt: u64,
    ready: u64,
) -> io::Result<()> {
    let all = [owner, tree, receipt, ready];
    if all
        .iter()
        .enumerate()
        .any(|(index, value)| all[..index].contains(value))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Witness handles must be distinct inherited capabilities.",
        ));
    }
    let owner = handle(owner)?;
    let tree = handle(tree)?;
    let receipt = handle(receipt)?;
    let ready = handle(ready)?;
    let _owner = unsafe { OwnedHandle::from_raw_handle(owner.cast()) };
    let _tree = unsafe { OwnedHandle::from_raw_handle(tree.cast()) };
    let receipt = unsafe { std::fs::File::from_raw_handle(receipt.cast()) };
    let mut ready = unsafe { std::fs::File::from_raw_handle(ready.cast()) };

    let mut writer = crate::recovery::witness::WitnessReceiptWriter::open(receipt)?;
    let mut observation = super::tree::JobTreeObservation::new(tree)?;
    if unsafe { WaitForSingleObject(owner, 0) } == WAIT_FAILED {
        return Err(io::Error::last_os_error());
    }
    use std::io::Write;
    ready.write_all(&[1])?;
    loop {
        if observation.observe(tree)? {
            let owner_exit_observed = match unsafe { WaitForSingleObject(owner, 0) } {
                WAIT_OBJECT_0 => true,
                WAIT_TIMEOUT => false,
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => return Err(io::Error::other("Unexpected owner wait result.")),
            };
            return writer.complete("windows_job_waited_members", owner_exit_observed);
        }
        match unsafe { WaitForSingleObject(owner, POLL_MS) } {
            WAIT_OBJECT_0 => {
                let _ = observation.observe(tree);
                if unsafe { TerminateJobObject(tree, 1) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                loop {
                    if observation.observe(tree)? {
                        return writer.complete("windows_job_waited_members", true);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(POLL_MS.into()));
                }
            }
            WAIT_TIMEOUT => {}
            WAIT_FAILED => return Err(io::Error::last_os_error()),
            _ => return Err(io::Error::other("Unexpected owner wait result.")),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn inherited_capabilities_reject_null_and_pseudo_handles_before_ownership() {
        assert!(super::handle(0).is_err());
        assert!(super::handle(u64::MAX).is_err());
    }
}
