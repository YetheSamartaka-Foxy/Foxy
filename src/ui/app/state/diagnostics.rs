use std::time::Instant;

use crate::ui::memory::{ProcessMemoryStats, ProcessVirtualMemoryMap};

#[derive(Clone, Debug)]
pub struct MemoryBreakdownBucket {
    pub label: String,
    pub bytes: usize,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct MemoryDiagnosticsReport {
    pub process: ProcessMemoryStats,
    pub os_virtual_memory_map: Option<ProcessVirtualMemoryMap>,
    pub buckets: Vec<MemoryBreakdownBucket>,
    pub tracked_total_bytes: usize,
    pub untracked_bytes: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct MemoryDiagnosticsSample {
    pub captured_at: Instant,
    pub label: String,
    pub process: ProcessMemoryStats,
    pub task_manager_memory_bytes: Option<u64>,
    pub working_set_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub tracked_total_bytes: usize,
    pub untracked_bytes: Option<i64>,
    pub buckets: Vec<MemoryBreakdownBucket>,
}
