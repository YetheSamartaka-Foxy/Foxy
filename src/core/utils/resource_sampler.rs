//! Periodic process and machine resource lines for extended diagnostics.
//!
//! Sampling runs on its own thread and only while the switch is on. Idle
//! samples are thinned to a heartbeat so a machine that sits in the tray does
//! not fill the log; anything Foxy is actively doing (CPU, disk or network
//! traffic above a small floor) is logged at the full interval.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use sysinfo::{
    MemoryRefreshKind, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System,
    get_current_pid,
};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
const IDLE_HEARTBEAT: Duration = Duration::from_secs(120);
const POLL_TICK: Duration = Duration::from_millis(500);
const ACTIVE_CPU_PERCENT: f32 = 2.0;
const ACTIVE_IO_BYTES: u64 = 1024 * 1024;

static ENABLED: AtomicBool = AtomicBool::new(false);
static THREAD: OnceLock<Mutex<Option<thread::JoinHandle<()>>>> = OnceLock::new();

pub(crate) fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        return;
    }
    let slot = THREAD.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return;
    }
    *guard = thread::Builder::new()
        .name("foxy-resource-sampler".into())
        .spawn(sampler_loop)
        .ok();
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Sample {
    pub(crate) process_cpu_percent: f32,
    pub(crate) system_cpu_percent: f32,
    pub(crate) process_rss_bytes: u64,
    pub(crate) process_virtual_bytes: u64,
    pub(crate) system_used_bytes: u64,
    pub(crate) system_available_bytes: u64,
    pub(crate) system_total_bytes: u64,
    pub(crate) disk_read_bytes: u64,
    pub(crate) disk_written_bytes: u64,
    pub(crate) net_received_bytes: u64,
    pub(crate) net_transmitted_bytes: u64,
}

impl Sample {
    /// Whether the interval saw enough work to be worth its own log line.
    pub(crate) fn is_active(&self) -> bool {
        self.process_cpu_percent >= ACTIVE_CPU_PERCENT
            || self.disk_read_bytes + self.disk_written_bytes >= ACTIVE_IO_BYTES
            || self.net_received_bytes + self.net_transmitted_bytes >= ACTIVE_IO_BYTES
    }

    pub(crate) fn log_line(&self, interval: Duration) -> String {
        let secs = interval.as_secs_f64().max(0.001);
        let rate = |bytes: u64| bytes as f64 / secs / (1024.0 * 1024.0);
        format!(
            "RESOURCES cpu_proc={:.1}% cpu_sys={:.1}% rss={:.1}MB virt={:.1}MB \
             sys_used={:.1}GB sys_avail={:.1}GB sys_total={:.1}GB \
             disk_read={:.2}MB/s disk_write={:.2}MB/s net_rx={:.2}MB/s net_tx={:.2}MB/s",
            self.process_cpu_percent,
            self.system_cpu_percent,
            self.process_rss_bytes as f64 / (1024.0 * 1024.0),
            self.process_virtual_bytes as f64 / (1024.0 * 1024.0),
            self.system_used_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            self.system_available_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            self.system_total_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            rate(self.disk_read_bytes),
            rate(self.disk_written_bytes),
            rate(self.net_received_bytes),
            rate(self.net_transmitted_bytes),
        )
    }
}

/// Decide whether a sample gets logged: active samples always, idle ones only
/// once `IDLE_HEARTBEAT` has passed since the last logged line.
pub(crate) fn should_log(sample: &Sample, since_last_log: Duration, heartbeat: Duration) -> bool {
    sample.is_active() || since_last_log >= heartbeat
}

struct Sampler {
    pid: Pid,
    system: System,
    networks: Networks,
}

impl Sampler {
    fn new() -> Option<Self> {
        let pid = get_current_pid().ok()?;
        let mut system = System::new();
        // The first CPU reading is always zero; prime it so the first logged
        // sample carries a real value.
        system.refresh_cpu_usage();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            Self::refresh_kind(),
        );
        let networks = Networks::new_with_refreshed_list();
        Some(Self {
            pid,
            system,
            networks,
        })
    }

    fn refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_disk_usage()
    }

    fn sample(&mut self) -> Sample {
        self.system.refresh_cpu_usage();
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            Self::refresh_kind(),
        );
        self.networks.refresh(true);
        let process = self.system.process(self.pid);
        let disk = process
            .map(|process| process.disk_usage())
            .unwrap_or_default();
        Sample {
            process_cpu_percent: process.map_or(0.0, |process| process.cpu_usage()),
            system_cpu_percent: self.system.global_cpu_usage(),
            process_rss_bytes: process.map_or(0, |process| process.memory()),
            process_virtual_bytes: process.map_or(0, |process| process.virtual_memory()),
            system_used_bytes: self.system.used_memory(),
            system_available_bytes: self.system.available_memory(),
            system_total_bytes: self.system.total_memory(),
            disk_read_bytes: disk.read_bytes,
            disk_written_bytes: disk.written_bytes,
            net_received_bytes: self.networks.values().map(|net| net.received()).sum(),
            net_transmitted_bytes: self.networks.values().map(|net| net.transmitted()).sum(),
        }
    }
}

fn sampler_loop() {
    let Some(mut sampler) = Sampler::new() else {
        log::warn!("Resource sampler could not resolve its own process; sampling disabled");
        return;
    };
    let mut last_sample = Instant::now();
    let mut last_log: Option<Instant> = None;
    while ENABLED.load(Ordering::Relaxed) {
        thread::sleep(POLL_TICK);
        if last_sample.elapsed() < SAMPLE_INTERVAL {
            continue;
        }
        let interval = last_sample.elapsed();
        let sample = sampler.sample();
        last_sample = Instant::now();
        let since_last_log = last_log.map_or(IDLE_HEARTBEAT, |at| at.elapsed());
        if should_log(&sample, since_last_log, IDLE_HEARTBEAT) {
            log::info!("{}", sample.log_line(interval));
            last_log = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_samples_only_log_on_heartbeat() {
        let idle = Sample::default();
        assert!(!idle.is_active());
        assert!(!should_log(&idle, Duration::from_secs(10), IDLE_HEARTBEAT));
        assert!(should_log(&idle, IDLE_HEARTBEAT, IDLE_HEARTBEAT));
    }

    #[test]
    fn active_samples_always_log() {
        let busy = Sample {
            disk_read_bytes: ACTIVE_IO_BYTES,
            ..Sample::default()
        };
        assert!(busy.is_active());
        assert!(should_log(&busy, Duration::ZERO, IDLE_HEARTBEAT));
        let cpu = Sample {
            process_cpu_percent: ACTIVE_CPU_PERCENT,
            ..Sample::default()
        };
        assert!(cpu.is_active());
    }

    #[test]
    fn log_line_reports_rates_per_second() {
        let sample = Sample {
            disk_read_bytes: 20 * 1024 * 1024,
            net_received_bytes: 10 * 1024 * 1024,
            ..Sample::default()
        };
        let line = sample.log_line(Duration::from_secs(10));
        assert!(line.starts_with("RESOURCES cpu_proc=0.0%"), "{line}");
        assert!(line.contains("disk_read=2.00MB/s"), "{line}");
        assert!(line.contains("net_rx=1.00MB/s"), "{line}");
    }
}
