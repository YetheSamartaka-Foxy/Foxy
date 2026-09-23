//! CPU time per thread, grouped by thread name, so a long operation can say
//! which threads its CPU went to. Windows only; elsewhere every snapshot is
//! empty and the breakdown says nothing.

use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CpuTime {
    pub(crate) user: Duration,
    pub(crate) kernel: Duration,
}

impl CpuTime {
    fn total(self) -> Duration {
        self.user + self.kernel
    }

    fn since(self, earlier: CpuTime) -> CpuTime {
        CpuTime {
            user: self.user.saturating_sub(earlier.user),
            kernel: self.kernel.saturating_sub(earlier.kernel),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ThreadCpuSnapshot {
    /// Thread id to its name and CPU time so far.
    threads: HashMap<u32, (String, CpuTime)>,
    process: CpuTime,
}

/// One named group of threads in a [`breakdown`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadGroupCpu {
    pub(crate) name: String,
    pub(crate) threads: usize,
    pub(crate) cpu: CpuTime,
}

/// CPU per thread name between two snapshots, largest first, and the process
/// CPU that no thread alive at `after` accounts for (threads that exited).
pub(crate) fn breakdown(
    before: &ThreadCpuSnapshot,
    after: &ThreadCpuSnapshot,
) -> (Vec<ThreadGroupCpu>, CpuTime) {
    let mut groups: HashMap<&str, ThreadGroupCpu> = HashMap::new();
    let mut accounted = CpuTime::default();
    for (id, (name, now)) in &after.threads {
        let earlier = before
            .threads
            .get(id)
            .filter(|(earlier_name, _)| earlier_name == name)
            .map(|(_, cpu)| *cpu)
            .unwrap_or_default();
        let used = now.since(earlier);
        if used.total().is_zero() {
            continue;
        }
        accounted.user += used.user;
        accounted.kernel += used.kernel;
        let group = groups.entry(name).or_insert_with(|| ThreadGroupCpu {
            name: name.clone(),
            threads: 0,
            cpu: CpuTime::default(),
        });
        group.threads += 1;
        group.cpu.user += used.user;
        group.cpu.kernel += used.kernel;
    }
    let mut groups: Vec<ThreadGroupCpu> = groups.into_values().collect();
    groups.sort_by(|a, b| {
        b.cpu
            .total()
            .cmp(&a.cpu.total())
            .then_with(|| a.name.cmp(&b.name))
    });
    let unaccounted = after.process.since(before.process).since(accounted);
    (groups, unaccounted)
}

/// `name=user+kernel/threads` for each group, then the exited remainder.
pub(crate) fn describe(groups: &[ThreadGroupCpu], unaccounted: CpuTime) -> String {
    let mut parts: Vec<String> = groups
        .iter()
        .map(|group| {
            format!(
                "{}={:.2}+{:.2}s/{}",
                group.name,
                group.cpu.user.as_secs_f64(),
                group.cpu.kernel.as_secs_f64(),
                group.threads
            )
        })
        .collect();
    parts.push(format!(
        "exited={:.2}+{:.2}s",
        unaccounted.user.as_secs_f64(),
        unaccounted.kernel.as_secs_f64()
    ));
    parts.join(" ")
}

#[cfg(windows)]
pub(crate) fn snapshot() -> ThreadCpuSnapshot {
    use winapi::shared::minwindef::FILETIME;
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::processthreadsapi::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, GetThreadTimes, OpenThread,
    };
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use winapi::um::winnt::THREAD_QUERY_LIMITED_INFORMATION;

    fn cpu(kernel: &FILETIME, user: &FILETIME) -> CpuTime {
        let duration = |time: &FILETIME| {
            let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
            Duration::from_nanos(ticks * 100)
        };
        CpuTime {
            user: duration(user),
            kernel: duration(kernel),
        }
    }

    let mut snapshot = ThreadCpuSnapshot::default();
    unsafe {
        let mut times: [FILETIME; 4] = std::mem::zeroed();
        let [created, exited, kernel, user] = &mut times;
        if GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user) != 0 {
            snapshot.process = cpu(&times[2], &times[3]);
        }

        let pid = GetCurrentProcessId();
        let list = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if list == INVALID_HANDLE_VALUE {
            return snapshot;
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut more = Thread32First(list, &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    let mut times: [FILETIME; 4] = std::mem::zeroed();
                    let [created, exited, kernel, user] = &mut times;
                    if GetThreadTimes(thread, created, exited, kernel, user) != 0 {
                        snapshot.threads.insert(
                            entry.th32ThreadID,
                            (thread_name(thread.cast()), cpu(&times[2], &times[3])),
                        );
                    }
                    CloseHandle(thread);
                }
            }
            more = Thread32Next(list, &mut entry) != 0;
        }
        CloseHandle(list);
    }
    snapshot
}

#[cfg(not(windows))]
pub(crate) fn snapshot() -> ThreadCpuSnapshot {
    ThreadCpuSnapshot::default()
}

/// The thread's description, looked up at runtime because it only exists on
/// Windows 10 1607 and later.
#[cfg(windows)]
fn thread_name(thread: *mut std::ffi::c_void) -> String {
    use std::sync::OnceLock;
    use winapi::um::libloaderapi::{GetModuleHandleA, GetProcAddress};
    use winapi::um::winbase::LocalFree;

    type GetThreadDescription =
        unsafe extern "system" fn(*mut std::ffi::c_void, *mut *mut u16) -> i32;
    static LOOKUP: OnceLock<Option<GetThreadDescription>> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(|| unsafe {
        let kernel32 = GetModuleHandleA(c"kernel32.dll".as_ptr());
        if kernel32.is_null() {
            return None;
        }
        let address = GetProcAddress(kernel32, c"GetThreadDescription".as_ptr());
        (!address.is_null()).then(|| std::mem::transmute::<_, GetThreadDescription>(address))
    });
    let Some(get_description) = lookup else {
        return "unnamed".to_string();
    };
    unsafe {
        let mut text: *mut u16 = std::ptr::null_mut();
        if get_description(thread, &mut text) < 0 || text.is_null() {
            return "unnamed".to_string();
        }
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let name = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        if name.is_empty() {
            "unnamed".to_string()
        } else {
            name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(user_ms: u64, kernel_ms: u64) -> CpuTime {
        CpuTime {
            user: Duration::from_millis(user_ms),
            kernel: Duration::from_millis(kernel_ms),
        }
    }

    fn snapshot_of(threads: &[(u32, &str, CpuTime)], process: CpuTime) -> ThreadCpuSnapshot {
        ThreadCpuSnapshot {
            threads: threads
                .iter()
                .map(|(id, name, cpu)| (*id, (name.to_string(), *cpu)))
                .collect(),
            process,
        }
    }

    #[test]
    fn groups_threads_by_name_and_reports_what_exited_threads_used() {
        let before = snapshot_of(
            &[
                (1, "main", cpu(100, 10)),
                (2, "worker", cpu(50, 0)),
                (3, "gone", cpu(5, 5)),
            ],
            cpu(155, 15),
        );
        let after = snapshot_of(
            &[
                (1, "main", cpu(300, 30)),
                (2, "worker", cpu(450, 20)),
                (4, "worker", cpu(100, 10)),
                (5, "idle", cpu(0, 0)),
            ],
            cpu(1_000, 100),
        );
        let (groups, exited) = breakdown(&before, &after);
        assert_eq!(
            groups,
            vec![
                ThreadGroupCpu {
                    name: "worker".into(),
                    threads: 2,
                    cpu: cpu(500, 30),
                },
                ThreadGroupCpu {
                    name: "main".into(),
                    threads: 1,
                    cpu: cpu(200, 20),
                },
            ]
        );
        assert_eq!(exited, cpu(145, 35));
        assert_eq!(
            describe(&groups, exited),
            "worker=0.50+0.03s/2 main=0.20+0.02s/1 exited=0.14+0.04s"
        );
    }

    #[test]
    fn a_reused_thread_id_with_a_new_name_counts_from_zero() {
        let before = snapshot_of(&[(7, "old", cpu(500, 0))], cpu(500, 0));
        let after = snapshot_of(&[(7, "new", cpu(40, 0))], cpu(540, 0));
        let (groups, _) = breakdown(&before, &after);
        assert_eq!(groups[0].name, "new");
        assert_eq!(groups[0].cpu, cpu(40, 0));
    }

    #[cfg(windows)]
    #[test]
    fn the_current_thread_is_in_the_snapshot_by_name() {
        let handle = std::thread::Builder::new()
            .name("thread-cpu-probe".into())
            .spawn(|| {
                let started = std::time::Instant::now();
                while started.elapsed() < Duration::from_millis(30) {
                    std::hint::black_box(0u64);
                }
                snapshot()
            })
            .unwrap();
        let snapshot = handle.join().unwrap();
        assert!(
            snapshot
                .threads
                .values()
                .any(|(name, _)| name == "thread-cpu-probe")
        );
    }
}
