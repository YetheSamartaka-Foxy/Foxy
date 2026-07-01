#[cfg(target_os = "linux")]
use std::fs;

#[derive(Clone, Debug, Default)]
pub struct ProcessMemoryStats {
    pub page_fault_count: Option<u64>,
    pub private_working_set_bytes: Option<u64>,
    pub working_set_bytes: Option<u64>,
    pub peak_working_set_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub virtual_bytes: Option<u64>,
    pub shared_commit_bytes: Option<u64>,
    pub paged_pool_bytes: Option<u64>,
    pub non_paged_pool_bytes: Option<u64>,
    pub total_system_bytes: Option<u64>,
    pub available_system_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessVirtualMemoryBucket {
    pub label: String,
    pub committed_bytes: u64,
    pub reserved_bytes: u64,
    pub region_count: usize,
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessVirtualMemoryRegion {
    pub base_address: usize,
    pub size_bytes: u64,
    pub protection: String,
    pub usage: String,
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessVirtualMemoryMap {
    pub buckets: Vec<ProcessVirtualMemoryBucket>,
    pub top_private_regions: Vec<ProcessVirtualMemoryRegion>,
}

impl ProcessMemoryStats {
    pub fn task_manager_memory_bytes(&self) -> Option<u64> {
        self.private_working_set_bytes.or(self.working_set_bytes)
    }

    pub fn baseline_bytes(&self) -> Option<u64> {
        self.task_manager_memory_bytes()
            .or(self.private_bytes)
            .or(self.working_set_bytes)
    }

    pub fn shared_working_set_bytes(&self) -> Option<u64> {
        match (self.working_set_bytes, self.private_working_set_bytes) {
            (Some(working), Some(private)) => Some(working.saturating_sub(private)),
            _ => None,
        }
    }
}

#[cfg(target_os = "windows")]
pub fn sample_process_memory() -> ProcessMemoryStats {
    use std::mem::{size_of, zeroed};
    use winapi::um::processthreadsapi::GetCurrentProcess;
    use winapi::um::psapi::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use winapi::um::sysinfoapi::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut stats = ProcessMemoryStats::default();

    #[repr(C)]
    struct ProcessMemoryCountersEx2 {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
        private_working_set_size: usize,
        shared_commit_usage: u64,
    }

    unsafe {
        let mut counters_ex2: ProcessMemoryCountersEx2 = zeroed();
        counters_ex2.cb = size_of::<ProcessMemoryCountersEx2>() as u32;
        let have_ex2 = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters_ex2 as *mut _ as *mut _,
            size_of::<ProcessMemoryCountersEx2>() as u32,
        ) != 0;

        if have_ex2 {
            stats.page_fault_count = Some(counters_ex2.page_fault_count as u64);
            stats.private_working_set_bytes = Some(counters_ex2.private_working_set_size as u64);
            stats.working_set_bytes = Some(counters_ex2.working_set_size as u64);
            stats.peak_working_set_bytes = Some(counters_ex2.peak_working_set_size as u64);
            stats.private_bytes = Some(counters_ex2.private_usage as u64);
            stats.virtual_bytes = Some(counters_ex2.pagefile_usage as u64);
            stats.shared_commit_bytes = Some(counters_ex2.shared_commit_usage);
            stats.paged_pool_bytes = Some(counters_ex2.quota_paged_pool_usage as u64);
            stats.non_paged_pool_bytes = Some(counters_ex2.quota_non_paged_pool_usage as u64);
        } else {
            let mut counters: PROCESS_MEMORY_COUNTERS_EX = zeroed();
            if GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters as *mut _ as *mut _,
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ) != 0
            {
                stats.page_fault_count = Some(counters.PageFaultCount as u64);
                stats.working_set_bytes = Some(counters.WorkingSetSize as u64);
                stats.peak_working_set_bytes = Some(counters.PeakWorkingSetSize as u64);
                stats.private_bytes = Some(counters.PrivateUsage as u64);
                stats.virtual_bytes = Some(counters.PagefileUsage as u64);
                stats.paged_pool_bytes = Some(counters.QuotaPagedPoolUsage as u64);
                stats.non_paged_pool_bytes = Some(counters.QuotaNonPagedPoolUsage as u64);
            }
        }

        let mut memory_status: MEMORYSTATUSEX = zeroed();
        memory_status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut memory_status) != 0 {
            stats.total_system_bytes = Some(memory_status.ullTotalPhys);
            stats.available_system_bytes = Some(memory_status.ullAvailPhys);
        }
    }

    stats
}

#[cfg(target_os = "windows")]
pub fn sample_process_virtual_memory_map() -> Option<ProcessVirtualMemoryMap> {
    use std::mem::{size_of, zeroed};
    use winapi::um::memoryapi::VirtualQuery;
    use winapi::um::sysinfoapi::{GetSystemInfo, SYSTEM_INFO};
    use winapi::um::winnt::{
        MEM_COMMIT, MEM_FREE, MEM_IMAGE, MEM_MAPPED, MEM_PRIVATE, MEM_RESERVE,
        MEMORY_BASIC_INFORMATION, PAGE_EXECUTE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
        PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS, PAGE_NOCACHE, PAGE_READONLY,
        PAGE_READWRITE, PAGE_TARGETS_INVALID, PAGE_WRITECOMBINE, PAGE_WRITECOPY,
    };

    #[derive(Default)]
    struct BucketAccumulator {
        committed_bytes: u64,
        reserved_bytes: u64,
        region_count: usize,
    }

    fn add_region(bucket: &mut BucketAccumulator, state: u32, region_size: u64) {
        match state {
            MEM_COMMIT => {
                bucket.committed_bytes = bucket.committed_bytes.saturating_add(region_size)
            }
            MEM_RESERVE => {
                bucket.reserved_bytes = bucket.reserved_bytes.saturating_add(region_size)
            }
            _ => {}
        }
        if state != MEM_FREE {
            bucket.region_count += 1;
        }
    }

    fn protection_label(protect: u32) -> String {
        let mut labels = Vec::new();
        match protect & 0xff {
            PAGE_NOACCESS => labels.push("no access"),
            PAGE_READONLY => labels.push("read only"),
            PAGE_READWRITE => labels.push("read/write"),
            PAGE_WRITECOPY => labels.push("write copy"),
            PAGE_EXECUTE => labels.push("execute"),
            PAGE_EXECUTE_READ => labels.push("execute/read"),
            PAGE_EXECUTE_READWRITE => labels.push("execute/read/write"),
            PAGE_EXECUTE_WRITECOPY => labels.push("execute/write copy"),
            _ => {}
        }
        if protect & PAGE_GUARD != 0 {
            labels.push("guard");
        }
        if protect & PAGE_NOCACHE != 0 {
            labels.push("no cache");
        }
        if protect & PAGE_WRITECOMBINE != 0 {
            labels.push("write combine");
        }
        if protect & PAGE_TARGETS_INVALID != 0 {
            labels.push("invalid targets");
        }
        if labels.is_empty() {
            "unknown".to_string()
        } else {
            labels.join(", ")
        }
    }

    fn effective_protect(info: &MEMORY_BASIC_INFORMATION) -> u32 {
        if info.Protect != 0 {
            info.Protect
        } else {
            info.AllocationProtect
        }
    }

    fn bucket_usage(kind: u32, protect: u32) -> (&'static str, &'static str) {
        match kind {
            MEM_PRIVATE => {
                if protect & PAGE_GUARD != 0 {
                    (
                        "Guarded private pages",
                        "Usually stack growth guards or reserved growth boundaries",
                    )
                } else if matches!(
                    protect & 0xff,
                    PAGE_EXECUTE
                        | PAGE_EXECUTE_READ
                        | PAGE_EXECUTE_READWRITE
                        | PAGE_EXECUTE_WRITECOPY
                ) {
                    (
                        "Executable private memory",
                        "Private executable pages, uncommon for this app unless a library allocates them",
                    )
                } else {
                    (
                        "Heap, stacks, temp buffers",
                        "Anonymous writable memory usually used by allocator heaps, thread stacks, SQLite temp pages, decompression, or network buffers",
                    )
                }
            }
            MEM_MAPPED => (
                "Mapped files or shared sections",
                "Backed by a file mapping or shared section rather than anonymous heap memory",
            ),
            MEM_IMAGE => (
                "Executable image or DLL",
                "Mapped code and read-only data from the app binary or loaded libraries",
            ),
            _ => (
                "Other mapping type",
                "Windows reported a region type outside the usual private, mapped, or image buckets",
            ),
        }
    }

    let mut map = ProcessVirtualMemoryMap::default();
    let mut private_bucket = BucketAccumulator::default();
    let mut mapped_bucket = BucketAccumulator::default();
    let mut image_bucket = BucketAccumulator::default();
    let mut other_bucket = BucketAccumulator::default();

    unsafe {
        let mut system_info: SYSTEM_INFO = zeroed();
        GetSystemInfo(&mut system_info);

        let max_address = system_info.lpMaximumApplicationAddress as usize;
        let mut address = system_info.lpMinimumApplicationAddress as usize;

        while address < max_address {
            let mut info: MEMORY_BASIC_INFORMATION = zeroed();
            let query_len = VirtualQuery(
                address as *const _,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if query_len == 0 {
                break;
            }

            let region_size = info.RegionSize as u64;
            let kind = info.Type;
            let state = info.State;

            match kind {
                MEM_PRIVATE => add_region(&mut private_bucket, state, region_size),
                MEM_MAPPED => add_region(&mut mapped_bucket, state, region_size),
                MEM_IMAGE => add_region(&mut image_bucket, state, region_size),
                _ => add_region(&mut other_bucket, state, region_size),
            }

            if state == MEM_COMMIT && kind == MEM_PRIVATE {
                let protect = effective_protect(&info);
                let (usage, note) = bucket_usage(kind, protect);
                map.top_private_regions.push(ProcessVirtualMemoryRegion {
                    base_address: info.BaseAddress as usize,
                    size_bytes: region_size,
                    protection: protection_label(protect),
                    usage: usage.to_string(),
                    note: note.to_string(),
                });
            }

            let next_address = (info.BaseAddress as usize).saturating_add(info.RegionSize);
            if next_address <= address {
                break;
            }
            address = next_address;
        }
    }

    map.top_private_regions
        .sort_by_key(|region| std::cmp::Reverse(region.size_bytes));
    map.top_private_regions.truncate(8);

    map.buckets = vec![
        ProcessVirtualMemoryBucket {
            label: "Private anonymous".to_string(),
            committed_bytes: private_bucket.committed_bytes,
            reserved_bytes: private_bucket.reserved_bytes,
            region_count: private_bucket.region_count,
            note: "Allocator heaps, thread stacks, temp buffers, and other process-private pages"
                .to_string(),
        },
        ProcessVirtualMemoryBucket {
            label: "Mapped sections".to_string(),
            committed_bytes: mapped_bucket.committed_bytes,
            reserved_bytes: mapped_bucket.reserved_bytes,
            region_count: mapped_bucket.region_count,
            note: "Memory-mapped files or shared sections".to_string(),
        },
        ProcessVirtualMemoryBucket {
            label: "Image / DLL".to_string(),
            committed_bytes: image_bucket.committed_bytes,
            reserved_bytes: image_bucket.reserved_bytes,
            region_count: image_bucket.region_count,
            note: "Executable image code and read-only data from the app or loaded libraries"
                .to_string(),
        },
        ProcessVirtualMemoryBucket {
            label: "Other / unknown".to_string(),
            committed_bytes: other_bucket.committed_bytes,
            reserved_bytes: other_bucket.reserved_bytes,
            region_count: other_bucket.region_count,
            note: "Regions that Windows did not classify as private, mapped, or image".to_string(),
        },
    ];
    map.buckets
        .sort_by_key(|bucket| std::cmp::Reverse(bucket.committed_bytes));

    Some(map)
}

#[cfg(target_os = "linux")]
pub fn sample_process_memory() -> ProcessMemoryStats {
    let mut stats = ProcessMemoryStats::default();
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(value) = parse_proc_kib_line(line, "VmRSS:") {
                stats.working_set_bytes = Some(value);
            } else if let Some(value) = parse_proc_kib_line(line, "VmHWM:") {
                stats.peak_working_set_bytes = Some(value);
            } else if let Some(value) = parse_proc_kib_line(line, "VmSize:") {
                stats.virtual_bytes = Some(value);
            } else if let Some(value) = parse_proc_kib_line(line, "RssAnon:") {
                stats.private_working_set_bytes = Some(value);
            } else if let Some(value) = parse_proc_kib_line(line, "RssFile:") {
                stats.paged_pool_bytes = Some(value);
            } else if let Some(value) = parse_proc_kib_line(line, "RssShmem:") {
                stats.non_paged_pool_bytes = Some(value);
            }
        }
    }

    // Parse page faults from /proc/self/stat (field 10 = minor faults)
    if let Ok(stat) = fs::read_to_string("/proc/self/stat") {
        // The comm field (field 2) may contain spaces and is enclosed in parens.
        // Find the closing paren and parse fields after it.
        if let Some(close_paren) = stat.rfind(')') {
            let fields_after_comm: Vec<&str> = stat[close_paren + 1..].split_whitespace().collect();
            // Field index 0 after ')' is state (field 3 in proc(5)), minor faults is field 10 = index 7
            if fields_after_comm.len() > 7 {
                if let Ok(faults) = fields_after_comm[7].parse::<u64>() {
                    stats.page_fault_count = Some(faults);
                }
            }
        }
    }

    if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            if let Some(value) = parse_proc_kib_line(line, "MemTotal:") {
                stats.total_system_bytes = Some(value);
            } else if let Some(value) = parse_proc_kib_line(line, "MemAvailable:") {
                stats.available_system_bytes = Some(value);
            }
        }
    }

    stats
}

#[cfg(target_os = "linux")]
pub fn sample_process_virtual_memory_map() -> Option<ProcessVirtualMemoryMap> {
    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    let mut code_bytes: u64 = 0;
    let mut heap_bytes: u64 = 0;
    let mut mapped_bytes: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut top_regions = Vec::new();

    for line in maps.lines() {
        let parts: Vec<&str> = line.splitn(6, ' ').collect();
        if parts.is_empty() {
            continue;
        }

        let addresses: Vec<&str> = parts[0].split('-').collect();
        if addresses.len() != 2 {
            continue;
        }

        let start = match u64::from_str_radix(addresses[0], 16) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let end = match u64::from_str_radix(addresses[1], 16) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let size = end.saturating_sub(start);
        let perms = if parts.len() > 1 { parts[1] } else { "" };
        let mapping_name = if parts.len() >= 6 {
            parts[5].trim()
        } else {
            ""
        };

        if perms.contains('x') {
            code_bytes = code_bytes.saturating_add(size);
        } else if perms.contains('w') {
            heap_bytes = heap_bytes.saturating_add(size);
        } else {
            mapped_bytes = mapped_bytes.saturating_add(size);
        }
        total_bytes = total_bytes.saturating_add(size);

        // Collect large private writable regions for the top-regions view
        if perms.contains('w') && size >= 1024 * 1024 {
            let usage = if mapping_name.is_empty() || mapping_name == "[heap]" {
                "Heap / anonymous".to_string()
            } else if mapping_name == "[stack]" {
                "Stack".to_string()
            } else {
                format!("Mapped: {}", mapping_name)
            };
            top_regions.push(ProcessVirtualMemoryRegion {
                base_address: start as usize,
                size_bytes: size,
                protection: perms.to_string(),
                usage,
                note: mapping_name.to_string(),
            });
        }
    }

    top_regions.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    top_regions.truncate(8);

    Some(ProcessVirtualMemoryMap {
        buckets: vec![
            ProcessVirtualMemoryBucket {
                label: "Executable code".to_string(),
                committed_bytes: code_bytes,
                reserved_bytes: 0,
                region_count: 0,
                note: "Executable pages (r-xp) from the binary and shared libraries".to_string(),
            },
            ProcessVirtualMemoryBucket {
                label: "Writable memory".to_string(),
                committed_bytes: heap_bytes,
                reserved_bytes: 0,
                region_count: 0,
                note: "Heap, stacks, and anonymous writable mappings (rw-p)".to_string(),
            },
            ProcessVirtualMemoryBucket {
                label: "Read-only mappings".to_string(),
                committed_bytes: mapped_bytes,
                reserved_bytes: 0,
                region_count: 0,
                note: "Read-only file-backed and shared mappings (r--p, r--s)".to_string(),
            },
        ],
        top_private_regions: top_regions,
    })
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn sample_process_virtual_memory_map() -> Option<ProcessVirtualMemoryMap> {
    None
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn sample_process_memory() -> ProcessMemoryStats {
    ProcessMemoryStats::default()
}

#[cfg(target_os = "linux")]
fn parse_proc_kib_line(line: &str, prefix: &str) -> Option<u64> {
    let value = line.strip_prefix(prefix)?.trim();
    let kib = value.strip_suffix("kB").unwrap_or(value).trim();
    kib.parse::<u64>().ok().map(|parsed| parsed * 1024)
}
