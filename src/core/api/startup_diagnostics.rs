use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::core::utils::format::sanitize_log_path;
use log::info;
use sysinfo::{Disks, Networks, System};

#[derive(Clone, Debug)]
pub struct StartupStoragePath {
    pub role: String,
    pub path: PathBuf,
}

impl StartupStoragePath {
    pub fn new(role: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            role: role.into(),
            path: path.into(),
        }
    }
}

/// Free-space floor for the drives Foxy writes its own state through.
///
/// A heuristic, and deliberately generous: Foxy stages multi-gigabyte addon
/// downloads and backups through these paths while the database and its WAL grow
/// on the same volume. A drive that runs out mid-sync does not fail cleanly -
/// writes start erroring part-way through, which lands the local state in the
/// same silently-wrong shape a stale schema does.
pub const CRITICAL_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Roles whose drive backs Foxy's own writes rather than the game install.
const SPACE_CRITICAL_ROLES: &[&str] = &[
    "database",
    "app_data",
    "game_space",
    "logs",
    "temp",
    "backups",
];

/// Whether `role` sits on a drive too full for Foxy to work through safely.
pub fn role_space_is_critical(role: &str, available: u64) -> bool {
    SPACE_CRITICAL_ROLES.contains(&role) && available < CRITICAL_FREE_BYTES
}

pub fn log_startup_system_diagnostics(storage_paths: &[StartupStoragePath]) {
    for line in startup_system_diagnostics_lines(storage_paths) {
        info!("{line}");
    }
    for line in low_space_warning_lines(storage_paths) {
        log::warn!("{line}");
    }
}

/// One `low_space:` line per role whose drive is below [`CRITICAL_FREE_BYTES`].
/// Empty when every Foxy-owned path has headroom.
pub fn low_space_warning_lines(storage_paths: &[StartupStoragePath]) -> Vec<String> {
    let disks = Disks::new_with_refreshed_list();
    let mut lines = Vec::new();
    let mut seen = BTreeSet::new();
    for storage_path in storage_paths {
        let path = normalized_path(&storage_path.path);
        let Some(disk) = disk_for_path(&path, &disks) else {
            continue;
        };
        let available = disk.available_space();
        if !role_space_is_critical(&storage_path.role, available) {
            continue;
        }
        if !seen.insert(disk.mount_point().to_path_buf()) {
            continue;
        }
        lines.push(format!(
            "low_space: role={} drive=\"{}\" available={} minimum={} - Foxy writes its database, temporary copies and backups here; downloads and database writes can fail part-way through",
            storage_path.role,
            sanitize_log_path(disk.mount_point()),
            format_bytes(available),
            format_bytes(CRITICAL_FREE_BYTES)
        ));
    }
    lines
}

pub fn startup_system_diagnostics_lines(storage_paths: &[StartupStoragePath]) -> Vec<String> {
    let mut system = System::new_all();
    system.refresh_all();

    let mut lines = Vec::new();
    lines.push("-- STARTUP SYSTEM SUMMARY --".to_string());
    lines.push(format!(
        "app: name={} version={} arch={} profile={} build={} commit={}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        crate::build_info::build_kind(),
        crate::build_info::GIT_HASH
    ));
    lines.push(format!(
        "os: name={} long_version={} version={} kernel={}",
        optional_value(System::name()),
        optional_value(System::long_os_version()),
        optional_value(System::os_version()),
        optional_value(System::kernel_version())
    ));
    lines.push(locale_summary_line());
    lines.push(cpu_summary_line(&system));
    lines.push(format!(
        "memory: total={} available={} used={} swap_total={} swap_used={}",
        format_bytes(system.total_memory()),
        format_bytes(system.available_memory()),
        format_bytes(system.used_memory()),
        format_bytes(system.total_swap()),
        format_bytes(system.used_swap())
    ));
    lines.push(uptime_summary_line());
    lines.push(power_summary_line());
    lines.push(privilege_summary_line());
    lines.extend(gpu_summary_lines());
    lines.extend(display_scale_summary_lines());
    lines.extend(network_summary_lines());
    lines.extend(antivirus_summary_lines());
    lines.extend(used_drive_summary_lines(storage_paths));
    lines.extend(path_space_summary_lines(storage_paths));
    lines.push(process_summary_line(&system));
    lines.push("-- END STARTUP SYSTEM SUMMARY --".to_string());
    lines
}

fn cpu_summary_line(system: &System) -> String {
    let cpu_count = system.cpus().len();
    let brand = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim())
        .filter(|brand| !brand.is_empty())
        .unwrap_or("<unknown>");
    let vendor = system
        .cpus()
        .first()
        .map(|cpu| cpu.vendor_id().trim())
        .filter(|vendor| !vendor.is_empty())
        .unwrap_or("<unknown>");
    let frequency_mhz = system
        .cpus()
        .first()
        .map(|cpu| cpu.frequency())
        .unwrap_or_default();

    let physical_cores = System::physical_core_count();
    let physical_text = physical_cores
        .map(|count| count.to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let smt = match physical_cores {
        Some(physical) if physical > 0 => {
            if cpu_count > physical {
                "yes"
            } else {
                "no"
            }
        }
        _ => "<unknown>",
    };

    format!(
        "cpu: brand=\"{}\" vendor=\"{}\" logical_cores={} physical_cores={} smt={} frequency_mhz={} usage_percent={:.1}",
        brand,
        vendor,
        cpu_count,
        physical_text,
        smt,
        frequency_mhz,
        system.global_cpu_usage()
    )
}

// ── #1 uptime & boot time ──────────────────────────────────────────────────
fn uptime_summary_line() -> String {
    let uptime = System::uptime();
    let boot = System::boot_time();
    let boot_text = chrono::DateTime::from_timestamp(boot as i64, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).to_rfc3339())
        .unwrap_or_else(|| "<unknown>".to_string());
    format!(
        "uptime: seconds={} human={} boot_time={}",
        uptime,
        format_duration_secs(uptime),
        boot_text
    )
}

fn format_duration_secs(total: u64) -> String {
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 || days > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 || hours > 0 || days > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

// ── #4 locale, language & timezone ──────────────────────────────────────────
fn locale_summary_line() -> String {
    let env_lang = ["LC_ALL", "LANG", "LANGUAGE", "LC_MESSAGES"]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "<unknown>".to_string());
    let utc_offset = chrono::Local::now().offset().to_string();
    format!(
        "locale: ui={} env_lang={} utc_offset={}",
        os_ui_locale(),
        env_lang,
        utc_offset
    )
}

#[cfg(target_os = "windows")]
fn os_ui_locale() -> String {
    use winapi::um::winnls::GetUserDefaultLocaleName;

    // LOCALE_NAME_MAX_LENGTH is 85 wide chars.
    let mut buffer = [0u16; 85];
    let written = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if written <= 0 {
        return "<unknown>".to_string();
    }
    // `written` includes the trailing null terminator.
    let len = (written as usize).saturating_sub(1);
    String::from_utf16_lossy(&buffer[..len])
}

#[cfg(not(target_os = "windows"))]
fn os_ui_locale() -> String {
    "<unknown>".to_string()
}

// ── #10 power / battery status ──────────────────────────────────────────────
#[cfg(target_os = "windows")]
fn power_summary_line() -> String {
    use winapi::um::winbase::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetSystemPowerStatus(&mut status) };
    if ok == 0 {
        return "power: status unavailable".to_string();
    }

    let ac = match status.ACLineStatus {
        0 => "battery",
        1 => "ac",
        _ => "unknown",
    };
    let battery_percent = match status.BatteryLifePercent {
        255 => "<unknown>".to_string(),
        percent => format!("{percent}%"),
    };
    let has_battery = status.BatteryFlag & 128 == 0;
    format!(
        "power: source={} has_battery={} battery_percent={}",
        ac, has_battery, battery_percent
    )
}

#[cfg(not(target_os = "windows"))]
fn power_summary_line() -> String {
    "power: unavailable on this platform".to_string()
}

// ── #6 elevation / privilege level ──────────────────────────────────────────
#[cfg(target_os = "windows")]
fn privilege_summary_line() -> String {
    match is_process_elevated() {
        Some(true) => "elevation: elevated=yes".to_string(),
        Some(false) => "elevation: elevated=no".to_string(),
        None => "elevation: elevated=<unknown>".to_string(),
    }
}

#[cfg(target_os = "windows")]
fn is_process_elevated() -> Option<bool> {
    use winapi::shared::minwindef::DWORD;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    use winapi::um::securitybaseapi::GetTokenInformation;
    use winapi::um::winnt::{TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};

    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }

        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut returned: DWORD = 0;
        let size = std::mem::size_of::<TOKEN_ELEVATION>() as DWORD;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            size,
            &mut returned,
        );
        CloseHandle(token);

        if ok == 0 {
            None
        } else {
            Some(elevation.TokenIsElevated != 0)
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn privilege_summary_line() -> String {
    "elevation: unavailable on this platform".to_string()
}

// ── #5 display scaling / DPI ────────────────────────────────────────────────
#[cfg(target_os = "windows")]
fn display_scale_summary_lines() -> Vec<String> {
    use winapi::um::winuser::GetDpiForSystem;

    let dpi = unsafe { GetDpiForSystem() };
    if dpi == 0 {
        return vec!["display_scale: system DPI unavailable".to_string()];
    }
    let scale_percent = (dpi as f64 / 96.0 * 100.0).round() as i64;
    vec![format!(
        "display_scale: system_dpi={dpi} scale_percent={scale_percent} (per-monitor scaling is logged by the UI layer)"
    )]
}

#[cfg(not(target_os = "windows"))]
fn display_scale_summary_lines() -> Vec<String> {
    vec!["display_scale: per-monitor scaling is logged by the UI layer".to_string()]
}

// ── #7 network interface summary ────────────────────────────────────────────
fn network_summary_lines() -> Vec<String> {
    let networks = Networks::new_with_refreshed_list();
    if networks.is_empty() {
        return vec!["network: no interfaces reported".to_string()];
    }

    let mut names: Vec<&String> = networks.keys().collect();
    names.sort();
    let mut lines = Vec::new();
    // MAC and IP addresses are intentionally omitted to avoid logging PII.
    for name in names {
        let data = &networks[name];
        lines.push(format!(
            "network: interface=\"{}\" mtu={} ip_count={} total_received={} total_transmitted={}",
            name,
            data.mtu(),
            data.ip_networks().len(),
            format_bytes(data.total_received()),
            format_bytes(data.total_transmitted())
        ));
    }
    lines
}

// ── #8 antivirus / Defender state (Windows Security Center) ─────────────────
#[cfg(target_os = "windows")]
fn antivirus_summary_lines() -> Vec<String> {
    match query_windows_antivirus() {
        Ok(products) if products.is_empty() => {
            vec!["antivirus: none registered with Security Center".to_string()]
        }
        Ok(products) => products
            .iter()
            .map(|product| {
                let state = product.product_state;
                // Bit layout of productState (undocumented but stable):
                //   0x1000 in the middle byte => real-time protection enabled
                //   0x10 in the low byte      => definitions out of date
                let realtime = state & 0x1000 != 0;
                let up_to_date = state & 0x10 == 0;
                format!(
                    "antivirus: name=\"{}\" realtime_protection={} up_to_date={} product_state=0x{:06X}",
                    product.display_name.as_deref().unwrap_or("<unknown>"),
                    realtime,
                    up_to_date,
                    state
                )
            })
            .collect(),
        Err(err) => vec![format!(
            "antivirus: failed to query Windows Security Center: {err}"
        )],
    }
}

#[cfg(not(target_os = "windows"))]
fn antivirus_summary_lines() -> Vec<String> {
    vec!["antivirus: unavailable on this platform".to_string()]
}

#[cfg(target_os = "windows")]
#[allow(non_snake_case)]
#[derive(Debug, serde::Deserialize)]
struct AntiVirusProduct {
    displayName: Option<String>,
    productState: Option<u32>,
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct AntivirusInfo {
    display_name: Option<String>,
    product_state: u32,
}

#[cfg(target_os = "windows")]
fn query_windows_antivirus() -> Result<Vec<AntivirusInfo>, wmi::WMIError> {
    let connection = wmi::WMIConnection::with_namespace_path("root\\SecurityCenter2")?;
    let products: Vec<AntiVirusProduct> =
        connection.raw_query("SELECT displayName, productState FROM AntiVirusProduct")?;
    Ok(products
        .into_iter()
        .map(|product| AntivirusInfo {
            display_name: product.displayName,
            product_state: product.productState.unwrap_or_default(),
        })
        .collect())
}

// ── #2 free space for app-relevant paths ────────────────────────────────────
fn path_space_summary_lines(storage_paths: &[StartupStoragePath]) -> Vec<String> {
    let disks = Disks::new_with_refreshed_list();

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        entries.push(("install".to_string(), dir.to_path_buf()));
    }
    entries.push(("temp".to_string(), std::env::temp_dir()));
    for storage_path in storage_paths {
        if storage_path.role.trim().is_empty() {
            continue;
        }
        entries.push((storage_path.role.clone(), storage_path.path.clone()));
    }

    let mut lines = Vec::new();
    let mut seen = BTreeSet::new();
    for (role, raw_path) in entries {
        let path = normalized_path(&raw_path);
        if !seen.insert((role.clone(), path.to_string_lossy().to_string())) {
            continue;
        }

        match disk_for_path(&path, &disks) {
            Some(disk) => {
                let total = disk.total_space();
                let available = disk.available_space();
                let free_percent = if total > 0 {
                    available as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                lines.push(format!(
                    "path_space: role={} path=\"{}\" total={} available={} free_percent={:.1}",
                    role,
                    sanitize_log_path(&path),
                    format_bytes(total),
                    format_bytes(available),
                    free_percent
                ));
            }
            None => lines.push(format!(
                "path_space: role={} path=\"{}\" drive=<unresolved>",
                role,
                sanitize_log_path(&path)
            )),
        }
    }
    lines
}

fn disk_for_path<'a>(path: &Path, disks: &'a Disks) -> Option<&'a sysinfo::Disk> {
    disks
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
}

// ── #9 process & environment basics ─────────────────────────────────────────
fn process_summary_line(system: &System) -> String {
    let pid = std::process::id();
    let parent = sysinfo::get_current_pid()
        .ok()
        .and_then(|current| system.process(current))
        .and_then(|process| process.parent())
        .and_then(|parent_pid| system.process(parent_pid))
        .map(|parent| parent.name().to_string_lossy().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let cwd = std::env::current_dir()
        .map(|dir| sanitize_log_path(&dir))
        .unwrap_or_else(|_| "<unknown>".to_string());
    let foxy_renderer = env_presence("FOXY_RENDERER");
    let wgpu_backend = env_presence("WGPU_BACKEND");

    format!(
        "process: pid={} parent=\"{}\" cwd=\"{}\" FOXY_RENDERER={} WGPU_BACKEND={}",
        pid, parent, cwd, foxy_renderer, wgpu_backend
    )
}

fn env_presence(key: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => format!("set(\"{}\")", value.trim()),
        _ => "unset".to_string(),
    }
}

#[cfg(target_os = "windows")]
fn gpu_summary_lines() -> Vec<String> {
    match query_windows_gpus() {
        Ok(gpus) if gpus.is_empty() => vec!["gpu: none reported by WMI".to_string()],
        Ok(gpus) => gpus
            .iter()
            .enumerate()
            .map(|(index, gpu)| {
                format!(
                    "gpu: index={} name=\"{}\" driver_version={} adapter_ram={}",
                    index,
                    gpu.name.as_deref().unwrap_or("<unknown>"),
                    gpu.driver_version.as_deref().unwrap_or("<unknown>"),
                    gpu.adapter_ram
                        .map(format_bytes)
                        .unwrap_or_else(|| "<unknown>".to_string())
                )
            })
            .collect(),
        Err(err) => vec![format!(
            "gpu: failed to query Windows WMI video controllers: {}",
            err
        )],
    }
}

#[cfg(not(target_os = "windows"))]
fn gpu_summary_lines() -> Vec<String> {
    vec!["gpu: unavailable on this platform without graphics API specific probing".to_string()]
}

#[cfg(target_os = "windows")]
#[allow(non_snake_case)]
#[derive(Debug, serde::Deserialize)]
struct Win32VideoController {
    Name: Option<String>,
    DriverVersion: Option<String>,
    AdapterRAM: Option<u64>,
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct GpuInfo {
    name: Option<String>,
    driver_version: Option<String>,
    adapter_ram: Option<u64>,
}

#[cfg(target_os = "windows")]
fn query_windows_gpus() -> Result<Vec<GpuInfo>, wmi::WMIError> {
    let connection = wmi::WMIConnection::new()?;
    let controllers: Vec<Win32VideoController> = connection
        .raw_query("SELECT Name, DriverVersion, AdapterRAM FROM Win32_VideoController")?;
    Ok(controllers
        .into_iter()
        .map(|controller| GpuInfo {
            name: controller.Name,
            driver_version: controller.DriverVersion,
            adapter_ram: controller.AdapterRAM,
        })
        .collect())
}

/// Enumerate every storage device the OS reports, regardless of whether Foxy
/// uses it. Intended for the diagnostics export so support can see the full
/// drive inventory (e.g. a slow external disk holding a repo). The startup log
/// keeps using [`used_drive_summary_lines`] to stay focused on app-relevant
/// drives.
pub fn all_storage_devices_lines() -> Vec<String> {
    let disks = Disks::new_with_refreshed_list();
    if disks.is_empty() {
        return vec!["storage_device: none reported by the operating system".to_string()];
    }

    let mut lines = Vec::new();
    for disk in &disks {
        let total = disk.total_space();
        let available = disk.available_space();
        let used_percent = if total > 0 {
            (total - available) as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        lines.push(format!(
            "storage_device: name=\"{}\" mount=\"{}\" kind={:?} fs=\"{}\" removable={} read_only={} total={} available={} used_percent={:.1}",
            disk.name().to_string_lossy(),
            sanitize_log_path(disk.mount_point()),
            disk.kind(),
            disk.file_system().to_string_lossy(),
            disk.is_removable(),
            disk.is_read_only(),
            format_bytes(total),
            format_bytes(available),
            used_percent
        ));
    }
    lines
}

fn used_drive_summary_lines(storage_paths: &[StartupStoragePath]) -> Vec<String> {
    let disks = Disks::new_with_refreshed_list();
    let used_roles = used_drive_roles(storage_paths, &disks);

    if used_roles.is_empty() {
        return vec!["drives: no configured storage paths resolved to a drive".to_string()];
    }

    let mut lines = Vec::new();
    for disk in &disks {
        let Some(key) = drive_key_for_mount(disk.mount_point()) else {
            continue;
        };
        let Some(roles) = used_roles.get(&key) else {
            continue;
        };
        let roles = roles.iter().cloned().collect::<Vec<_>>().join(",");
        lines.push(format!(
            "drive: id={} name=\"{}\" kind={:?} fs=\"{}\" total={} available={} used_by={}",
            key,
            disk.name().to_string_lossy(),
            disk.kind(),
            disk.file_system().to_string_lossy(),
            format_bytes(disk.total_space()),
            format_bytes(disk.available_space()),
            roles
        ));
    }

    for (key, roles) in used_roles {
        if disks
            .iter()
            .any(|disk| drive_key_for_mount(disk.mount_point()).as_deref() == Some(&key))
        {
            continue;
        }
        let roles = roles.iter().cloned().collect::<Vec<_>>().join(",");
        lines.push(format!(
            "drive: id={} name=\"<unmatched>\" kind=<unknown> fs=\"<unknown>\" total=<unknown> available=<unknown> used_by={}",
            key, roles
        ));
    }
    lines
}

fn used_drive_roles(
    storage_paths: &[StartupStoragePath],
    disks: &Disks,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_drive: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut seen_paths = BTreeSet::new();

    for storage_path in storage_paths {
        if storage_path.role.trim().is_empty() {
            continue;
        }
        let path = normalized_path(&storage_path.path);
        let path_key = path.to_string_lossy().to_string();
        if !seen_paths.insert((storage_path.role.clone(), path_key)) {
            continue;
        }
        if let Some(drive_key) = drive_key_for_path(&path, disks) {
            by_drive
                .entry(drive_key)
                .or_default()
                .insert(storage_path.role.clone());
        }
    }

    by_drive
}

fn normalized_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn drive_key_for_path(path: &Path, disks: &Disks) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        windows_drive_key_for_path(path).or_else(|| longest_mount_match(path, disks))
    }

    #[cfg(not(target_os = "windows"))]
    {
        longest_mount_match(path, disks)
    }
}

#[cfg(target_os = "windows")]
fn windows_drive_key_for_path(path: &Path) -> Option<String> {
    use std::path::{Component, Prefix};

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return None;
    };

    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            Some(format!("{}:", char::from(letter).to_ascii_uppercase()))
        }
        Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => Some("UNC".to_string()),
        _ => None,
    }
}

fn longest_mount_match(path: &Path, disks: &Disks) -> Option<String> {
    disks
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .and_then(|disk| drive_key_for_mount(disk.mount_point()))
}

fn drive_key_for_mount(mount_point: &Path) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        windows_drive_key_for_path(mount_point)
    }

    #[cfg(not(target_os = "windows"))]
    {
        if mount_point == Path::new("/") {
            return Some("root".to_string());
        }

        mount_point
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .map(|name| name.to_string())
            .or_else(|| Some("root".to_string()))
    }
}

fn optional_value(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;

    if bytes as f64 >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else if bytes as f64 >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_as_gib() {
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GiB");
    }

    #[test]
    fn skips_empty_storage_roles() {
        let paths = vec![StartupStoragePath::new("", PathBuf::from("."))];
        let disks = Disks::new();

        assert!(used_drive_roles(&paths, &disks).is_empty());
    }

    #[test]
    fn formats_duration_with_days() {
        assert_eq!(format_duration_secs(0), "0s");
        assert_eq!(format_duration_secs(59), "59s");
        assert_eq!(format_duration_secs(3661), "1h 1m 1s");
        assert_eq!(format_duration_secs(90_061), "1d 1h 1m 1s");
    }

    #[test]
    fn low_space_applies_only_to_foxy_owned_roles() {
        let tight = CRITICAL_FREE_BYTES - 1;
        assert!(role_space_is_critical("database", tight));
        assert!(role_space_is_critical("temp", tight));
        assert!(role_space_is_critical("backups", tight));
        // The game install can legitimately sit on a nearly full drive; Foxy
        // does not write its own state there.
        assert!(!role_space_is_critical("arma3", tight));
        assert!(!role_space_is_critical("repository", tight));
        assert!(!role_space_is_critical("steam", tight));
    }

    #[test]
    fn low_space_threshold_is_a_floor_not_a_ratio() {
        // The August 2026 field report: 5.6 GiB free on a 930 GiB system drive
        // holding the database, logs, temp and backups.
        assert!(role_space_is_critical("database", 5 * 1024 * 1024 * 1024));
        assert!(!role_space_is_critical("database", CRITICAL_FREE_BYTES));
        assert!(!role_space_is_critical(
            "database",
            200 * 1024 * 1024 * 1024
        ));
    }

    #[test]
    fn emits_all_diagnostic_sections() {
        // Exercises every collector end-to-end (including the platform-specific
        // ones) and asserts each section is present without panicking.
        let paths = vec![StartupStoragePath::new("test-repo", std::env::temp_dir())];
        let lines = startup_system_diagnostics_lines(&paths);
        let joined = lines.join("\n");

        for prefix in [
            "app:",
            "os:",
            "locale:",
            "cpu:",
            "memory:",
            "uptime:",
            "power:",
            "elevation:",
            "gpu:",
            "display_scale:",
            "network:",
            "antivirus:",
            "path_space:",
            "process:",
        ] {
            assert!(
                lines.iter().any(|line| line.starts_with(prefix)),
                "missing diagnostic section {prefix:?} in:\n{joined}"
            );
        }
    }

    #[test]
    fn all_storage_devices_lines_never_empty() {
        // Always returns at least one line: a device row, or the explicit
        // "none reported" fallback when the OS lists no disks.
        let lines = all_storage_devices_lines();
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| line.starts_with("storage_device:")));
    }

    // ── format_bytes ────────────────────────────────────────────────────

    #[test]
    fn format_bytes_small_values_use_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        // One byte below a MiB still reports raw bytes.
        assert_eq!(format_bytes(1024 * 1024 - 1), "1048575 B");
    }

    #[test]
    fn format_bytes_mib_range() {
        assert_eq!(format_bytes(1024 * 1024), "1.00 MiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 / 2), "1.50 MiB");
    }

    #[test]
    fn format_bytes_gib_boundary() {
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GiB");
    }

    // ── format_duration_secs ────────────────────────────────────────────

    #[test]
    fn format_duration_minute_boundary() {
        assert_eq!(format_duration_secs(60), "1m 0s");
    }

    #[test]
    fn format_duration_hour_boundary() {
        assert_eq!(format_duration_secs(3600), "1h 0m 0s");
    }

    #[test]
    fn format_duration_exact_day() {
        assert_eq!(format_duration_secs(86_400), "1d 0h 0m 0s");
    }

    #[test]
    fn format_duration_multiple_days() {
        assert_eq!(format_duration_secs(2 * 86_400 + 5), "2d 0h 0m 5s");
    }

    // ── optional_value ──────────────────────────────────────────────────

    #[test]
    fn optional_value_returns_trimmed_present_value() {
        assert_eq!(
            optional_value(Some("  Windows 11  ".to_string())),
            "Windows 11"
        );
    }

    #[test]
    fn optional_value_none_is_unknown() {
        assert_eq!(optional_value(None), "<unknown>");
    }

    #[test]
    fn optional_value_empty_or_whitespace_is_unknown() {
        assert_eq!(optional_value(Some(String::new())), "<unknown>");
        assert_eq!(optional_value(Some("   ".to_string())), "<unknown>");
    }

    // ── env_presence ────────────────────────────────────────────────────

    #[test]
    fn env_presence_unset_variable_reports_unset() {
        // A key that is exceedingly unlikely to exist in any test environment.
        assert_eq!(
            env_presence("FOXY_DIAGNOSTICS_DEFINITELY_UNSET_VAR_XYZ"),
            "unset"
        );
    }

    // ── normalized_path ─────────────────────────────────────────────────

    #[test]
    fn normalized_path_keeps_absolute_paths() {
        #[cfg(windows)]
        let abs = Path::new("C:\\Foxy\\repo");
        #[cfg(not(windows))]
        let abs = Path::new("/foxy/repo");
        assert_eq!(normalized_path(abs), abs.to_path_buf());
    }

    #[test]
    fn normalized_path_makes_relative_paths_absolute() {
        let normalized = normalized_path(Path::new("relative/dir"));
        assert!(normalized.is_absolute());
    }

    // ── drive_key_for_mount ─────────────────────────────────────────────

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn drive_key_for_mount_root_is_root() {
        assert_eq!(
            drive_key_for_mount(Path::new("/")),
            Some("root".to_string())
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn drive_key_for_mount_named_mount_uses_leaf() {
        assert_eq!(
            drive_key_for_mount(Path::new("/mnt/data")),
            Some("data".to_string())
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn drive_key_for_mount_windows_disk_letter() {
        assert_eq!(
            drive_key_for_mount(Path::new("C:\\")),
            Some("C:".to_string())
        );
    }

    // ── StartupStoragePath ──────────────────────────────────────────────

    #[test]
    fn startup_storage_path_stores_role_and_path() {
        let storage = StartupStoragePath::new("config", "/foxy/config");
        assert_eq!(storage.role, "config");
        assert_eq!(storage.path, PathBuf::from("/foxy/config"));
    }
}
