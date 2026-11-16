#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceSample {
    pub pid: Option<u32>,
    pub process_start_time_secs: Option<u64>,
    pub cpu_percent: Option<f32>,
    pub resident_bytes: Option<u64>,
    pub uptime_ms: Option<u64>,
    pub state: ResourceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceState {
    Available,
    WarmingUp,
    Unavailable,
    IdentityChanged,
}

pub(super) struct ResourceSampler {
    anchor_pid: u32,
    expected_executable: Option<PathBuf>,
    anchor_start_identity: Option<u64>,
    system: System,
    cpu_ready: bool,
    last_cpu_sample: Option<Instant>,
    last_target: Option<(u32, u64)>,
}

impl ResourceSampler {
    pub(super) fn new(anchor_pid: u32, expected_executable: PathBuf) -> Self {
        let mut system = System::new();
        let anchor = Pid::from_u32(anchor_pid);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[anchor]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let anchor_start_identity = process_identity(anchor_pid, &system);
        Self {
            anchor_pid,
            expected_executable: std::fs::canonicalize(expected_executable).ok(),
            anchor_start_identity,
            system,
            cpu_ready: false,
            last_cpu_sample: None,
            last_target: None,
        }
    }

    pub(super) fn sample(&mut self) -> ResourceSample {
        let Some(expected_executable) = self.expected_executable.as_deref() else {
            return unavailable(ResourceState::Unavailable);
        };
        let Some(anchor_identity) = self.anchor_start_identity else {
            return unavailable(ResourceState::Unavailable);
        };
        let anchor = Pid::from_u32(self.anchor_pid);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[anchor]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let Some(current_anchor_identity) = process_identity(self.anchor_pid, &self.system) else {
            return unavailable(ResourceState::Unavailable);
        };
        if current_anchor_identity != anchor_identity {
            return unavailable(ResourceState::IdentityChanged);
        }

        #[cfg(target_os = "linux")]
        let target_pid = match direct_game_child(self.anchor_pid, expected_executable) {
            ChildLookup::One(pid) => pid,
            ChildLookup::None | ChildLookup::Ambiguous => {
                return unavailable(ResourceState::Unavailable);
            }
        };
        #[cfg(not(target_os = "linux"))]
        let target_pid = self.anchor_pid;

        let target = Pid::from_u32(target_pid);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[target]),
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_exe(UpdateKind::Always),
        );
        let Some(process) = self.system.process(target) else {
            return unavailable(ResourceState::Unavailable);
        };
        let Some(target_identity) = process_identity(target_pid, &self.system) else {
            return unavailable(ResourceState::Unavailable);
        };
        let Some(executable) = process
            .exe()
            .and_then(|path| std::fs::canonicalize(path).ok())
        else {
            return unavailable(ResourceState::Unavailable);
        };
        if executable != expected_executable {
            return unavailable(ResourceState::IdentityChanged);
        }

        #[cfg(target_os = "linux")]
        {
            if proc_start_ticks(self.anchor_pid) != Some(anchor_identity)
                || proc_start_ticks(target_pid) != Some(target_identity)
                || !matches!(direct_game_child(self.anchor_pid, expected_executable), ChildLookup::One(pid) if pid == target_pid)
            {
                return unavailable(ResourceState::IdentityChanged);
            }
        }
        #[cfg(windows)]
        {
            if process_identity(self.anchor_pid, &self.system) != Some(anchor_identity) {
                return unavailable(ResourceState::IdentityChanged);
            }
        }

        let start_time_secs = process.start_time();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());
        let uptime_ms = now_ms.and_then(|now| now.checked_sub(start_time_secs.checked_mul(1_000)?));
        let now = Instant::now();
        let cpu_percent = (self.cpu_ready
            && self.last_target == Some((target_pid, target_identity))
            && self
                .last_cpu_sample
                .is_some_and(|last| now.duration_since(last) >= Duration::from_millis(100)))
        .then(|| process.cpu_usage())
        .and_then(sanitize_cpu_percent);
        self.cpu_ready = true;
        self.last_cpu_sample = Some(now);
        self.last_target = Some((target_pid, target_identity));
        ResourceSample {
            pid: Some(target_pid),
            process_start_time_secs: Some(start_time_secs),
            cpu_percent,
            resident_bytes: Some(process.memory()),
            uptime_ms,
            state: if cpu_percent.is_some() {
                ResourceState::Available
            } else {
                ResourceState::WarmingUp
            },
        }
    }
}

fn unavailable(state: ResourceState) -> ResourceSample {
    ResourceSample {
        pid: None,
        process_start_time_secs: None,
        cpu_percent: None,
        resident_bytes: None,
        uptime_ms: None,
        state,
    }
}

#[cfg(target_os = "linux")]
fn process_identity(pid: u32, _system: &System) -> Option<u64> {
    proc_start_ticks(pid)
}

#[cfg(windows)]
fn process_identity(pid: u32, system: &System) -> Option<u64> {
    system
        .process(Pid::from_u32(pid))
        .map(|process| process.start_time())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_identity(pid: u32, system: &System) -> Option<u64> {
    system
        .process(Pid::from_u32(pid))
        .map(|process| process.start_time())
}

#[cfg(target_os = "linux")]
fn proc_start_ticks(pid: u32) -> Option<u64> {
    let path = format!("/proc/{pid}/stat");
    let stat = std::fs::read_to_string(path).ok()?;
    parse_proc_start_ticks(&stat)
}

#[cfg(target_os = "linux")]
fn parse_proc_start_ticks(stat: &str) -> Option<u64> {
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[cfg(target_os = "linux")]
enum ChildLookup {
    One(u32),
    None,
    Ambiguous,
}

#[cfg(target_os = "linux")]
fn direct_game_child(anchor_pid: u32, expected_executable: &std::path::Path) -> ChildLookup {
    let path = format!("/proc/{anchor_pid}/task/{anchor_pid}/children");
    let Ok(file) = std::fs::File::open(path) else {
        return ChildLookup::None;
    };
    let mut bytes = Vec::with_capacity(4097);
    if std::io::Read::take(file, 4097)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return ChildLookup::None;
    }
    if bytes.len() > 4096 {
        return ChildLookup::Ambiguous;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return ChildLookup::Ambiguous;
    };
    let mut matching = None;
    let mut examined = 0;
    for pid in text
        .split_ascii_whitespace()
        .filter_map(|value| value.parse::<u32>().ok())
    {
        examined += 1;
        if examined > 128 {
            return ChildLookup::Ambiguous;
        }
        if std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .is_some_and(|path| path == expected_executable)
        {
            if matching.replace(pid).is_some() {
                return ChildLookup::Ambiguous;
            }
        }
    }
    matching.map_or(ChildLookup::None, ChildLookup::One)
}

fn sanitize_cpu_percent(value: f32) -> Option<f32> {
    value
        .is_finite()
        .then_some(value)
        .filter(|value| *value >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn proc_stat_start_time_parses_after_a_parenthesized_command() {
        let stat =
            "42 (a name ) with spaces) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 190 20";
        assert_eq!(parse_proc_start_ticks(stat), Some(190));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn current_process_birth_identity_is_available() {
        let pid = sysinfo::get_current_pid().unwrap().as_u32();
        assert!(proc_start_ticks(pid).is_some());
    }

    #[test]
    fn cpu_percent_rejects_non_finite_and_negative_values() {
        assert_eq!(super::sanitize_cpu_percent(f32::NAN), None);
        assert_eq!(super::sanitize_cpu_percent(f32::INFINITY), None);
        assert_eq!(super::sanitize_cpu_percent(-1.0), None);
        assert_eq!(super::sanitize_cpu_percent(125.0), Some(125.0));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn current_process_sampling_is_identity_scoped() {
        let Ok(pid) = sysinfo::get_current_pid() else {
            return;
        };
        let Some(path) = std::env::current_exe().ok() else {
            return;
        };
        let mut sampler = ResourceSampler::new(pid.as_u32(), path);
        let sample = sampler.sample();
        assert_eq!(sample.pid, Some(pid.as_u32()));
        assert!(sample.resident_bytes.is_some());
        assert!(matches!(
            sample.state,
            ResourceState::WarmingUp | ResourceState::Available
        ));
        assert_eq!(
            sample.cpu_percent.is_none(),
            sample.state == ResourceState::WarmingUp
        );
    }
}
