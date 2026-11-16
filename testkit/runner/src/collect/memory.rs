//! Process memory sampling for the memory lane.
//!
//! The kit measures Foxy's own footprint from outside the process, so the
//! numbers are not perturbed by an allocator hook inside the binary under test
//! and a memory row can be recorded for any operation, not only the ones that
//! already publish telemetry.
//!
//! Two counters are read per sample and they answer different questions.
//! `PrivateUsage` is private commit: memory the process asked for and has not
//! given back, which is what an allocation regression moves. `WorkingSetSize`
//! is resident pages, which the OS trims under pressure, so it tracks what the
//! machine feels rather than what Foxy holds. Both are recorded; the ledger
//! gates on commit. The process I/O read counter rides along: it counts every
//! byte the process read, page-cache hits included, so it exposes re-reads a
//! disk counter would hide. Process user and kernel CPU time ride along too,
//! so an operation reports what it cost the machine as well as how long it took.
//!
//! Off Windows only the process's total CPU time is read, through `sysinfo`:
//! nothing there counts private commit or the working set the same way, so
//! those figures are left out rather than approximated.

use serde_json::{Map, Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The process id the sampler follows. A `startup` operation replaces the app
/// mid-run, so the sampler reads the pid every tick instead of capturing it.
pub type Target = Arc<Mutex<Option<u32>>>;

pub fn target() -> Target {
    Arc::new(Mutex::new(None))
}

pub fn set(target: &Target, pid: Option<u32>) {
    if let Ok(mut slot) = target.lock() {
        *slot = pid;
    }
}

#[derive(Clone, Copy)]
struct Counters {
    /// `None` off Windows.
    footprint: Option<Footprint>,
    /// Process user plus kernel CPU time, in 100 ns units.
    cpu: u64,
    /// The user share of `cpu`, where the platform reports it apart.
    cpu_user: Option<u64>,
}

#[derive(Clone, Copy)]
struct Footprint {
    working_set: u64,
    private: u64,
    peak_working_set: u64,
    page_faults: u64,
    read_transfer: u64,
}

#[derive(Clone, Copy)]
struct Sample {
    elapsed_ms: u64,
    counters: Counters,
}

#[cfg(windows)]
fn read(pid: u32) -> Option<Counters> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
            Threading::{
                GetProcessIoCounters, GetProcessTimes, IO_COUNTERS, OpenProcess,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = K32GetProcessMemoryInfo(handle, (&raw mut counters).cast(), counters.cb);
        let mut io: IO_COUNTERS = std::mem::zeroed();
        let io_ok = GetProcessIoCounters(handle, &raw mut io);
        let mut times: [windows_sys::Win32::Foundation::FILETIME; 4] = std::mem::zeroed();
        let [created, exited, kernel, user] = &mut times;
        let times_ok = GetProcessTimes(handle, created, exited, kernel, user);
        let ticks = |time: &windows_sys::Win32::Foundation::FILETIME| {
            (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
        };
        CloseHandle(handle);
        let (user, kernel) = if times_ok != 0 {
            (ticks(&times[3]), ticks(&times[2]))
        } else {
            (0, 0)
        };
        (ok != 0).then_some(Counters {
            footprint: Some(Footprint {
                working_set: counters.WorkingSetSize as u64,
                private: counters.PrivateUsage as u64,
                peak_working_set: counters.PeakWorkingSetSize as u64,
                page_faults: u64::from(counters.PageFaultCount),
                read_transfer: if io_ok != 0 { io.ReadTransferCount } else { 0 },
            }),
            cpu: user.saturating_add(kernel),
            cpu_user: Some(user),
        })
    }
}

#[cfg(not(windows))]
fn read(pid: u32) -> Option<Counters> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu(),
    );
    let process = system.process(pid)?;
    Some(Counters {
        footprint: None,
        cpu: process.accumulated_cpu_time().saturating_mul(10_000),
        cpu_user: None,
    })
}

/// A running sampler. Dropping it without [`Watch::finish`] stops the thread.
pub struct Watch {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Vec<Sample>>>,
    interval: Duration,
    settle: Duration,
}

/// Sampling cadence. Fast enough that a one-second allocation spike cannot hide
/// between two ticks, slow enough that the sampler itself is not the load.
pub const INTERVAL: Duration = Duration::from_millis(100);
/// Quiet window sampled after the operation's own work is done, so the row can
/// separate "peaked here" from "still holding it".
pub const SETTLE: Duration = Duration::from_millis(1_500);

impl Watch {
    pub fn start(target: &Target, interval: Duration, settle: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let target = target.clone();
        let handle = thread::spawn(move || {
            let mut samples = Vec::new();
            let mut epoch = Instant::now();
            let mut current = None;
            loop {
                let pid = target.lock().ok().and_then(|slot| *slot);
                if let Some(pid) = pid {
                    // A `startup` operation replaces the app mid-watch. The
                    // samples taken before the restart belong to the process
                    // that is going away, and carrying them over would report
                    // the outgoing app's footprint as the new one's starting
                    // point.
                    if current != Some(pid) {
                        current = Some(pid);
                        samples.clear();
                        epoch = Instant::now();
                    }
                    if let Some(counters) = read(pid) {
                        samples.push(Sample {
                            elapsed_ms: epoch.elapsed().as_millis() as u64,
                            counters,
                        });
                    }
                }
                if flag.load(Ordering::Relaxed) {
                    return samples;
                }
                thread::sleep(interval);
            }
        });
        Self {
            stop,
            handle: Some(handle),
            interval,
            settle,
        }
    }

    /// Stop sampling and reduce the series to the ledger's memory block.
    ///
    /// The settle window runs first: without it the last sample lands while the
    /// operation's transient buffers are still alive and every row would report
    /// its peak as its retained footprint.
    pub fn finish(mut self) -> Value {
        if !self.settle.is_zero() {
            thread::sleep(self.settle);
        }
        let settle_ms = self.settle.as_millis() as u64;
        self.stop.store(true, Ordering::Relaxed);
        let samples = self
            .handle
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        let threads = thread::available_parallelism().map_or(1, |n| n.get());
        summarize(&samples, self.interval, settle_ms, threads)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn summarize(samples: &[Sample], interval: Duration, settle_ms: u64, threads: usize) -> Value {
    if samples.is_empty() {
        return json!({
            "samples": 0,
            "interval_ms": interval.as_millis() as u64,
            "settle_ms": settle_ms,
        });
    }
    let last_ms = samples[samples.len() - 1].elapsed_ms;
    // The settle window is where a retained figure is honest, so read it from
    // the quiet tail rather than from the single last sample, which can catch a
    // collection that has not run yet.
    let tail_start = last_ms.saturating_sub(settle_ms);
    let footprints: Option<Vec<(u64, Footprint)>> = samples
        .iter()
        .map(|sample| {
            sample
                .counters
                .footprint
                .map(|footprint| (sample.elapsed_ms, footprint))
        })
        .collect();
    let mut stats = Map::new();
    if let Some(footprints) = &footprints {
        // The reduced numbers say how much; the series says where, which is
        // the difference between a one-time cache fill and a leak.
        let series = footprints
            .iter()
            .map(|(elapsed_ms, footprint)| {
                json!([elapsed_ms, footprint.private, footprint.working_set])
            })
            .collect();
        stats.insert("series".into(), Value::Array(series));
    }
    stats.insert("samples".into(), samples.len().into());
    stats.insert("interval_ms".into(), (interval.as_millis() as u64).into());
    stats.insert("settle_ms".into(), settle_ms.into());
    stats.insert("span_ms".into(), last_ms.into());
    if let Some(footprints) = &footprints
        && let Value::Object(fields) = footprint_stats(footprints, tail_start)
    {
        stats.extend(fields);
    }
    let cpu_seconds = |ticks: u64| ticks as f64 / 1e7;
    let (first, last) = (samples[0].counters, samples[samples.len() - 1].counters);
    let cpu_s = match (first.cpu_user, last.cpu_user) {
        (Some(first_user), Some(last_user)) => {
            let kernel_ticks = |cpu: u64, user: u64| cpu.saturating_sub(user);
            let user = cpu_seconds(last_user.saturating_sub(first_user));
            let kernel = cpu_seconds(
                kernel_ticks(last.cpu, last_user)
                    .saturating_sub(kernel_ticks(first.cpu, first_user)),
            );
            stats.insert("cpu_user_s".into(), user.into());
            stats.insert("cpu_kernel_s".into(), kernel.into());
            user + kernel
        }
        _ => cpu_seconds(last.cpu.saturating_sub(first.cpu)),
    };
    stats.insert("cpu_s".into(), cpu_s.into());
    stats.insert("cpu_threads".into(), threads.into());
    stats.insert(
        "cpu_machine_percent".into(),
        average_machine_cpu_percent(samples, tail_start, threads).into(),
    );
    stats.insert(
        "cpu_machine_peak_percent".into(),
        peak_machine_cpu_percent(samples, CPU_PEAK_WINDOW_MS, threads).into(),
    );
    // Cumulative process CPU per sample, so a peak can be placed in the
    // operation; stripped from the ledger row like `series`.
    let cpu_series = samples
        .iter()
        .map(|sample| json!([sample.elapsed_ms, sample.counters.cpu / 10_000]))
        .collect();
    stats.insert("cpu_series".into(), Value::Array(cpu_series));
    Value::Object(stats)
}

/// Commit, residency, fault and read figures over a non-empty series.
fn footprint_stats(footprints: &[(u64, Footprint)], tail_start: u64) -> Value {
    let first = footprints[0].1;
    let last = footprints[footprints.len() - 1].1;
    let tail: Vec<&Footprint> = footprints
        .iter()
        .filter(|(elapsed_ms, _)| *elapsed_ms >= tail_start)
        .map(|(_, footprint)| footprint)
        .collect();
    let retained = tail
        .iter()
        .map(|footprint| footprint.private)
        .min()
        .unwrap_or(last.private);
    let start = first.private;
    let peak = footprints
        .iter()
        .map(|(_, footprint)| footprint.private)
        .max()
        .unwrap_or(start);
    let mut sorted: Vec<u64> = footprints
        .iter()
        .map(|(_, footprint)| footprint.private)
        .collect();
    sorted.sort_unstable();
    json!({
        "start_private_bytes": start,
        "peak_private_bytes": peak,
        "median_private_bytes": sorted[sorted.len() / 2],
        "retained_private_bytes": retained,
        // Growth over the operation, floored at zero: a negative number would
        // only mean the operation started while an earlier one was still
        // releasing, and a signed metric there is noise, not a saving.
        "growth_private_bytes": retained.saturating_sub(start),
        "transient_private_bytes": peak.saturating_sub(retained),
        "peak_working_set_bytes": footprints.iter().map(|(_, f)| f.working_set).max(),
        "retained_working_set_bytes": tail.iter().map(|f| f.working_set).min(),
        "process_peak_working_set_bytes": footprints
            .iter()
            .map(|(_, f)| f.peak_working_set)
            .max(),
        "page_faults": last.page_faults.saturating_sub(first.page_faults),
        "read_transfer_bytes": last.read_transfer.saturating_sub(first.read_transfer),
    })
}

/// Width of the window the CPU peak is read over: the app's CPU usage limit is
/// an average, so one 100 ms sample over it is not a breach.
const CPU_PEAK_WINDOW_MS: u64 = 1_000;

fn cpu_ticks(sample: &Sample) -> u64 {
    sample.counters.cpu
}

/// CPU use between two samples as a share of every logical processor, the
/// scale Task Manager and the app's CPU usage limit use.
fn machine_cpu_percent(first: &Sample, last: &Sample, threads: usize) -> Option<f64> {
    let wall_s = last.elapsed_ms.saturating_sub(first.elapsed_ms) as f64 / 1000.0;
    if wall_s <= 0.0 {
        return None;
    }
    let cpu_s = cpu_ticks(last).saturating_sub(cpu_ticks(first)) as f64 / 1e7;
    Some(cpu_s / wall_s / threads.max(1) as f64 * 100.0)
}

/// Average over the operation itself, before the settle tail dilutes it.
fn average_machine_cpu_percent(samples: &[Sample], tail_start: u64, threads: usize) -> Option<f64> {
    let first = samples.first()?;
    let last = samples
        .iter()
        .rev()
        .find(|sample| sample.elapsed_ms <= tail_start)
        .filter(|sample| sample.elapsed_ms > first.elapsed_ms)
        .or_else(|| samples.last())?;
    machine_cpu_percent(first, last, threads)
}

/// Highest CPU use over any span of at least `window_ms`.
fn peak_machine_cpu_percent(samples: &[Sample], window_ms: u64, threads: usize) -> Option<f64> {
    let mut peak: Option<f64> = None;
    let mut end = 0;
    for (start, first) in samples.iter().enumerate() {
        end = end.max(start + 1);
        while end < samples.len()
            && samples[end].elapsed_ms.saturating_sub(first.elapsed_ms) < window_ms
        {
            end += 1;
        }
        let Some(last) = samples.get(end) else {
            break;
        };
        if let Some(percent) = machine_cpu_percent(first, last, threads) {
            peak = Some(peak.map_or(percent, |best: f64| best.max(percent)));
        }
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(elapsed_ms: u64, private: u64) -> Sample {
        Sample {
            elapsed_ms,
            counters: Counters {
                footprint: Some(Footprint {
                    working_set: private / 2,
                    private,
                    peak_working_set: private,
                    page_faults: elapsed_ms,
                    read_transfer: elapsed_ms * 10,
                }),
                cpu: elapsed_ms * 25_000,
                cpu_user: Some(elapsed_ms * 20_000),
            },
        }
    }

    #[test]
    fn empty_series_still_reports_shape() {
        let stats = summarize(&[], INTERVAL, 1_500, 8);
        assert_eq!(stats["samples"], 0);
        assert!(stats["peak_private_bytes"].is_null());
    }

    #[test]
    fn a_platform_with_only_total_cpu_reports_cpu_and_no_footprint() {
        let samples: Vec<Sample> = (0..=20)
            .map(|i| Sample {
                elapsed_ms: i * 100,
                counters: Counters {
                    footprint: None,
                    cpu: i * 100 * 25_000,
                    cpu_user: None,
                },
            })
            .collect();
        let stats = summarize(&samples, INTERVAL, 0, 10);
        assert_eq!(stats["samples"], 21);
        assert_eq!(stats["cpu_s"], 5.0);
        assert_eq!(stats["cpu_machine_percent"], 25.0);
        assert_eq!(stats["cpu_machine_peak_percent"], 25.0);
        for absent in [
            "series",
            "peak_private_bytes",
            "retained_working_set_bytes",
            "read_transfer_bytes",
            "cpu_user_s",
            "cpu_kernel_s",
        ] {
            assert!(stats.get(absent).is_none(), "{absent}");
        }
    }

    #[test]
    fn the_running_process_can_be_read() {
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(30) {
            std::hint::black_box(0u64);
        }
        let counters = read(std::process::id()).unwrap();
        assert_eq!(counters.footprint.is_some(), cfg!(windows));
        assert!(counters.cpu > 0);
    }

    #[test]
    fn cpu_is_reported_as_a_share_of_every_thread() {
        // The helper burns 2.5 processor seconds per second.
        let samples: Vec<Sample> = (0..=20).map(|i| sample(i * 100, 100)).collect();
        let stats = summarize(&samples, INTERVAL, 0, 10);
        assert_eq!(stats["cpu_threads"], 10);
        assert_eq!(stats["cpu_machine_percent"], 25.0);
        assert_eq!(stats["cpu_machine_peak_percent"], 25.0);
        assert_eq!(stats["cpu_series"][2], json!([200, 500]));
    }

    #[test]
    fn the_cpu_peak_is_the_busiest_full_window() {
        let mut samples: Vec<Sample> = (0..=30).map(|i| sample(i * 100, 100)).collect();
        // Idle for the first second, then twice the helper's rate.
        for sample in &mut samples {
            let busy_ms = sample.elapsed_ms.saturating_sub(1_000);
            sample.counters.cpu = busy_ms * 50_000;
            sample.counters.cpu_user = Some(busy_ms * 40_000);
        }
        assert_eq!(
            peak_machine_cpu_percent(&samples, CPU_PEAK_WINDOW_MS, 10),
            Some(50.0)
        );
        assert_eq!(
            peak_machine_cpu_percent(&samples[..5], CPU_PEAK_WINDOW_MS, 10),
            None
        );
    }

    #[test]
    fn retained_reads_the_settle_tail_not_the_peak() {
        let samples = [
            sample(0, 100),
            sample(500, 900),
            sample(1_000, 800),
            sample(1_100, 210),
            sample(1_600, 200),
        ];
        let stats = summarize(&samples, INTERVAL, 1_000, 8);
        assert_eq!(stats["peak_private_bytes"], 900);
        assert_eq!(stats["retained_private_bytes"], 200);
        assert_eq!(stats["growth_private_bytes"], 100);
        assert_eq!(stats["transient_private_bytes"], 700);
        assert_eq!(stats["read_transfer_bytes"], 16_000);
        assert_eq!(stats["cpu_user_s"], 3.2);
        assert_eq!(stats["cpu_kernel_s"], 0.8);
        assert_eq!(stats["cpu_s"], 4.0);
    }

    #[test]
    fn growth_never_goes_negative() {
        let samples = [sample(0, 900), sample(1_000, 100)];
        let stats = summarize(&samples, INTERVAL, 1_000, 8);
        assert_eq!(stats["growth_private_bytes"], 0);
    }
}
