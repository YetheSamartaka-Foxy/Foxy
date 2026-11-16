use std::collections::BTreeMap;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use winapi::shared::basetsd::ULONG_PTR;
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::ERROR_MORE_DATA;
use winapi::um::handleapi::CompareObjectHandles;
use winapi::um::jobapi2::QueryInformationJobObject;
use winapi::um::processthreadsapi::OpenProcess;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{WAIT_FAILED, WAIT_OBJECT_0};
use winapi::um::winnt::{
    HANDLE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE,
};

const MAX_MEMBERS: usize = 4096;

#[repr(C)]
struct ProcessIdList {
    assigned: DWORD,
    listed: DWORD,
    pids: [ULONG_PTR; MAX_MEMBERS],
}

pub(super) struct JobTreeObservation {
    members_by_pid: BTreeMap<u32, Vec<OwnedHandle>>,
    member_count: usize,
}

impl JobTreeObservation {
    pub(super) fn new(job: HANDLE) -> io::Result<Self> {
        let mut observation = Self {
            members_by_pid: BTreeMap::new(),
            member_count: 0,
        };
        let pids = process_ids(job)?;
        let accounting = query_accounting(job)?;
        if accounting.ActiveProcesses == 0 || pids.len() != accounting.TotalProcesses as usize {
            return Err(incomplete_history());
        }
        for pid in pids {
            let member = open_member(job, pid).ok_or_else(incomplete_history)?;
            observation.add_member(
                u32::try_from(pid).map_err(|_| incomplete_history())?,
                member,
            )?;
        }
        if observation.member_count != accounting.TotalProcesses as usize {
            return Err(incomplete_history());
        }
        Ok(observation)
    }

    pub(super) fn observe(&mut self, job: HANDLE) -> io::Result<bool> {
        let pids = process_ids(job)?;
        let accounting = query_accounting(job)?;
        for pid in pids {
            self.member_for_pid(job, pid)?;
        }

        if accounting.ActiveProcesses != 0 {
            return Ok(false);
        }
        if self.member_count != accounting.TotalProcesses as usize {
            return Err(incomplete_history());
        }
        for handle in self.members_by_pid.values().flatten() {
            match unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) } {
                WAIT_OBJECT_0 => {}
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => return Ok(false),
            }
        }
        let final_accounting = query_accounting(job)?;
        if final_accounting.ActiveProcesses != 0
            || final_accounting.TotalProcesses as usize != self.member_count
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn member_for_pid(&mut self, job: HANDLE, pid: ULONG_PTR) -> io::Result<()> {
        let Some(candidate) = open_member(job, pid) else {
            return Ok(());
        };
        let pid = u32::try_from(pid).map_err(|_| incomplete_history())?;
        if let Some(members) = self.members_by_pid.get(&pid) {
            for member in members {
                if unsafe {
                    CompareObjectHandles(
                        member.as_raw_handle().cast(),
                        candidate.as_raw_handle().cast(),
                    )
                } != 0
                {
                    return Ok(());
                }
            }
        }
        self.add_member(pid, candidate)
    }

    fn add_member(&mut self, pid: u32, handle: OwnedHandle) -> io::Result<()> {
        if self.member_count == MAX_MEMBERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Job process history exceeds the supported bound.",
            ));
        }
        self.members_by_pid.entry(pid).or_default().push(handle);
        self.member_count += 1;
        Ok(())
    }
}

fn process_ids(job: HANDLE) -> io::Result<Vec<ULONG_PTR>> {
    let mut buffer: ProcessIdList = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicProcessIdList,
            (&mut buffer as *mut ProcessIdList).cast(),
            size_of::<ProcessIdList>() as DWORD,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_MORE_DATA as i32) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Job process list exceeds the supported bound.",
            ));
        }
        return Err(error);
    }
    let listed = buffer.listed as usize;
    if listed > MAX_MEMBERS
        || buffer.assigned as usize > MAX_MEMBERS
        || listed != buffer.assigned as usize
    {
        return Err(incomplete_history());
    }
    Ok(buffer.pids[..listed].to_vec())
}

fn query_accounting(job: HANDLE) -> io::Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
    let mut value = unsafe { std::mem::zeroed() };
    if unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&mut value as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as DWORD,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

fn open_member(job: HANDLE, pid: ULONG_PTR) -> Option<OwnedHandle> {
    let pid = u32::try_from(pid).ok()?;
    let raw = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return None;
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let mut in_job = 0;
    if unsafe { IsProcessInJob(raw, job, &mut in_job) } == 0 || in_job == 0 {
        return None;
    }
    Some(handle)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn IsProcessInJob(process: HANDLE, job: HANDLE, result: *mut i32) -> i32;
}

fn incomplete_history() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Job process membership history is incomplete.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn an_empty_job_cannot_hide_an_unobserved_lifetime_member() {
        let root = tempfile::tempdir().unwrap();
        let mut process = super::super::Process::spawn(
            &crate::process::tests::spec(root.path(), "root_exits"),
            None,
            None,
        )
        .unwrap();
        let leaf_pid = crate::process::tests::read_pid(root.path(), "leaf.pid");
        let leaf = open_member(process.job.as_raw_handle().cast(), leaf_pid as ULONG_PTR).unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(process.process.as_raw_handle().cast(), 10_000) },
            WAIT_OBJECT_0
        );
        std::fs::write(root.path().join("release"), b"release").unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(leaf.as_raw_handle().cast(), 10_000) },
            WAIT_OBJECT_0
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while query_accounting(process.job.as_raw_handle().cast())
            .unwrap()
            .ActiveProcesses
            != 0
        {
            assert!(
                Instant::now() < deadline,
                "Native job accounting did not empty"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            process.observe().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
