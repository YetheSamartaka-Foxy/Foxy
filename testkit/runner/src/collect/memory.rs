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

use serde_json::{Value, json};
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
    working_set: u64,
    private: u64,
    peak_working_set: u64,
    page_faults: u64,
    read_transfer: u64,
    /// Process user and kernel CPU time, in 100 ns units.
    cpu_user: u64,
    cpu_kernel: u64,
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
        (ok != 0).then_some(Counters {
            working_set: counters.WorkingSetSize as u64,
            private: counters.PrivateUsage as u64,
            peak_working_set: counters.PeakWorkingSetSize as u64,
            page_faults: u64::from(counters.PageFaultCount),
            read_transfer: if io_ok != 0 { io.ReadTransferCount } else { 0 },
            cpu_user: if times_ok != 0 { ticks(&times[3]) } else { 0 },
            cpu_kernel: if times_ok != 0 { ticks(&times[2]) } else { 0 },
        })
    }
}

#[cfg(not(windows))]
fn read(_pid: u32) -> Option<Counters> {
    None
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
        summarize(&samples, self.interval, settle_ms)
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

fn summarize(samples: &[Sample], interval: Duration, settle_ms: u64) -> Value {
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
    let tail: Vec<&Sample> = samples
        .iter()
        .filter(|sample| sample.elapsed_ms >= tail_start)
        .collect();
    let retained = tail
        .iter()
        .map(|sample| sample.counters.private)
        .min()
        .unwrap_or(samples[samples.len() - 1].counters.private);
    let start = samples[0].counters.private;
    let peak = samples
        .iter()
        .map(|sample| sample.counters.private)
        .max()
        .unwrap_or(start);
    let mut sorted: Vec<u64> = samples
        .iter()
        .map(|sample| sample.counters.private)
        .collect();
    sorted.sort_unstable();
    // The reduced numbers say how much; the series says where, which is the
    // difference between a one-time cache fill and a leak.
    let series: Vec<Value> = samples
        .iter()
        .map(|sample| {
            json!([
                sample.elapsed_ms,
                sample.counters.private,
                sample.counters.working_set
            ])
        })
        .collect();
    let first = samples[0].counters;
    let last = samples[samples.len() - 1].counters;
    let cpu_seconds = |ticks: u64| ticks as f64 / 1e7;
    let cpu_user = cpu_seconds(last.cpu_user.saturating_sub(first.cpu_user));
    let cpu_kernel = cpu_seconds(last.cpu_kernel.saturating_sub(first.cpu_kernel));
    json!({
        "series": series,
        "samples": samples.len(),
        "interval_ms": interval.as_millis() as u64,
        "settle_ms": settle_ms,
        "span_ms": last_ms,
        "start_private_bytes": start,
        "peak_private_bytes": peak,
        "median_private_bytes": sorted[sorted.len() / 2],
        "retained_private_bytes": retained,
        // Growth over the operation, floored at zero: a negative number would
        // only mean the operation started while an earlier one was still
        // releasing, and a signed metric there is noise, not a saving.
        "growth_private_bytes": retained.saturating_sub(start),
        "transient_private_bytes": peak.saturating_sub(retained),
        "peak_working_set_bytes": samples.iter().map(|s| s.counters.working_set).max(),
        "retained_working_set_bytes": tail.iter().map(|s| s.counters.working_set).min(),
        "process_peak_working_set_bytes": samples
            .iter()
            .map(|s| s.counters.peak_working_set)
            .max(),
        "page_faults": samples[samples.len() - 1]
            .counters
            .page_faults
            .saturating_sub(samples[0].counters.page_faults),
        "read_transfer_bytes": samples[samples.len() - 1]
            .counters
            .read_transfer
            .saturating_sub(samples[0].counters.read_transfer),
        "cpu_user_s": cpu_user,
        "cpu_kernel_s": cpu_kernel,
        "cpu_s": cpu_user + cpu_kernel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(elapsed_ms: u64, private: u64) -> Sample {
        Sample {
            elapsed_ms,
            counters: Counters {
                working_set: private / 2,
                private,
                peak_working_set: private,
                page_faults: elapsed_ms,
                read_transfer: elapsed_ms * 10,
                cpu_user: elapsed_ms * 20_000,
                cpu_kernel: elapsed_ms * 5_000,
            },
        }
    }

    #[test]
    fn empty_series_still_reports_shape() {
        let stats = summarize(&[], INTERVAL, 1_500);
        assert_eq!(stats["samples"], 0);
        assert!(stats["peak_private_bytes"].is_null());
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
        let stats = summarize(&samples, INTERVAL, 1_000);
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
        let stats = summarize(&samples, INTERVAL, 1_000);
        assert_eq!(stats["growth_private_bytes"], 0);
    }
}
