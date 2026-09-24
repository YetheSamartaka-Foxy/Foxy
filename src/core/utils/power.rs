//! The machine's power state, recorded with benchmarks and hash runs: on
//! battery or a power-saving plan, disk and CPU power management can slow a
//! run by more than any code change being measured.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerStatus {
    /// `ac`, `battery` or empty when unknown.
    pub source: String,
    pub battery_percent: Option<u8>,
    pub battery_saver: bool,
    /// Friendly name of the active power plan, in the system language.
    pub plan: String,
    /// The Windows 10/11 power mode slider (`best-efficiency`, `balanced`,
    /// `best-performance`, ...), empty when the system has none.
    pub mode: String,
}

impl PowerStatus {
    pub fn summary(&self) -> String {
        let source = if self.source.is_empty() {
            "unknown"
        } else {
            self.source.as_str()
        };
        let mut text = format!("source={source}");
        if let Some(percent) = self.battery_percent {
            text.push_str(&format!(" battery={percent}%"));
        }
        if self.battery_saver {
            text.push_str(" battery_saver=on");
        }
        if !self.plan.is_empty() {
            text.push_str(&format!(" plan=\"{}\"", self.plan));
        }
        if !self.mode.is_empty() {
            text.push_str(&format!(" mode={}", self.mode));
        }
        text
    }
}

#[cfg(windows)]
pub fn sample() -> PowerStatus {
    use winapi::um::winbase::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = PowerStatus::default();
    // SAFETY: all-zero is a valid SYSTEM_POWER_STATUS, and the call only
    // writes into the struct it is given.
    let mut raw: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut raw) } != 0 {
        status.source = match raw.ACLineStatus {
            0 => "battery".to_owned(),
            1 => "ac".to_owned(),
            _ => String::new(),
        };
        // 128 means no system battery; 255 means the status is unknown.
        if raw.BatteryFlag & 128 == 0 && raw.BatteryLifePercent <= 100 {
            status.battery_percent = Some(raw.BatteryLifePercent);
        }
        status.battery_saver = raw.Reserved1 == 1;
    }
    status.plan = active_plan_name().unwrap_or_default();
    status.mode = overlay_mode().unwrap_or_default();
    status
}

#[cfg(not(windows))]
pub fn sample() -> PowerStatus {
    PowerStatus::default()
}

/// A [`sample`] at most 30 s old. Reading the power plan takes about 0.1 s on
/// Windows, which callers that run once per hash batch cannot pay.
pub fn recent_sample() -> PowerStatus {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);
    static RECENT: std::sync::Mutex<Option<(std::time::Instant, PowerStatus)>> =
        std::sync::Mutex::new(None);
    reuse_or_sample(&RECENT, MAX_AGE, std::time::Instant::now(), sample)
}

fn reuse_or_sample(
    cache: &std::sync::Mutex<Option<(std::time::Instant, PowerStatus)>>,
    max_age: std::time::Duration,
    now: std::time::Instant,
    sample: impl FnOnce() -> PowerStatus,
) -> PowerStatus {
    let mut cached = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((taken, status)) = cached.as_ref()
        && now.saturating_duration_since(*taken) <= max_age
    {
        return status.clone();
    }
    let status = sample();
    *cached = Some((now, status.clone()));
    status
}

#[cfg(windows)]
fn active_plan_name() -> Option<String> {
    use winapi::shared::guiddef::GUID;
    use winapi::um::powersetting::PowerGetActiveScheme;
    use winapi::um::powrprof::PowerReadFriendlyName;
    use winapi::um::winbase::LocalFree;

    let mut scheme: *mut GUID = std::ptr::null_mut();
    // SAFETY: on success the call stores a LocalAlloc'd GUID, freed below.
    if unsafe { PowerGetActiveScheme(std::ptr::null_mut(), &mut scheme) } != 0 || scheme.is_null() {
        return None;
    }
    let mut size = 0u32;
    // SAFETY: a null buffer asks only for the size; `scheme` is valid.
    unsafe {
        PowerReadFriendlyName(
            std::ptr::null_mut(),
            scheme,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    let mut name = vec![0u16; (size as usize).div_ceil(2).max(1)];
    let mut bytes = (name.len() * 2) as u32;
    // SAFETY: the buffer holds `bytes` bytes, and `scheme` is still valid.
    let read = unsafe {
        PowerReadFriendlyName(
            std::ptr::null_mut(),
            scheme,
            std::ptr::null(),
            std::ptr::null(),
            name.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    // SAFETY: `scheme` came from PowerGetActiveScheme and is freed once.
    unsafe { LocalFree(scheme.cast()) };
    if read != 0 {
        return None;
    }
    let end = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..end]).trim().to_owned())
}

/// The power mode slider, read through `PowerGetEffectiveOverlayScheme`,
/// which older Windows releases do not export, hence the runtime lookup.
#[cfg(windows)]
fn overlay_mode() -> Option<String> {
    use winapi::shared::guiddef::GUID;
    use winapi::um::libloaderapi::{GetProcAddress, LoadLibraryW};

    type GetOverlay = unsafe extern "system" fn(*mut GUID) -> u32;
    let library: Vec<u16> = "powrprof.dll\0".encode_utf16().collect();
    // SAFETY: the name is NUL-terminated; powrprof stays loaded for the
    // process, so the function pointer never dangles.
    let module = unsafe { LoadLibraryW(library.as_ptr()) };
    if module.is_null() {
        return None;
    }
    // SAFETY: the symbol name is NUL-terminated ASCII.
    let symbol = unsafe { GetProcAddress(module, c"PowerGetEffectiveOverlayScheme".as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    // SAFETY: the export has this signature on every release that has it.
    let get_overlay: GetOverlay = unsafe { std::mem::transmute(symbol) };
    // SAFETY: all-zero is a valid GUID, and the call writes only into it.
    let mut guid: GUID = unsafe { std::mem::zeroed() };
    if unsafe { get_overlay(&mut guid) } != 0 {
        return None;
    }
    Some(overlay_mode_name(guid.Data1, guid.Data2, guid.Data3, guid.Data4).to_owned())
}

fn overlay_mode_name(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> &'static str {
    match (data1, data2, data3, data4) {
        (0, 0, 0, [0, 0, 0, 0, 0, 0, 0, 0]) => "balanced",
        (0x961c_c777, 0x2547, 0x4f9d, [0x81, 0x74, 0x7d, 0x86, 0x18, 0x1b, 0x8a, 0x7a]) => {
            "best-efficiency"
        }
        (0x3af9_b8d9, 0x7c97, 0x431d, [0xad, 0x78, 0x34, 0xa8, 0xbf, 0xea, 0x43, 0x9f]) => {
            "better-battery"
        }
        (0xded5_74b5, 0x45a0, 0x4f42, [0x87, 0x37, 0x46, 0x34, 0x5c, 0x09, 0xc2, 0x38]) => {
            "best-performance"
        }
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_names_only_what_is_known() {
        assert_eq!(PowerStatus::default().summary(), "source=unknown");
        let status = PowerStatus {
            source: "battery".into(),
            battery_percent: Some(41),
            battery_saver: true,
            plan: "Balanced".into(),
            mode: "best-efficiency".into(),
        };
        assert_eq!(
            status.summary(),
            "source=battery battery=41% battery_saver=on plan=\"Balanced\" mode=best-efficiency"
        );
    }

    #[test]
    fn a_recent_sample_is_reused_until_it_is_too_old() {
        let cache = std::sync::Mutex::new(None);
        let max_age = std::time::Duration::from_secs(30);
        let start = std::time::Instant::now();
        let mut samples = 0;
        let mut take = |now| {
            reuse_or_sample(&cache, max_age, now, || {
                samples += 1;
                PowerStatus {
                    source: format!("s{samples}"),
                    ..Default::default()
                }
            })
            .source
        };
        assert_eq!(take(start), "s1");
        assert_eq!(take(start + max_age), "s1");
        assert_eq!(
            take(start + max_age + std::time::Duration::from_millis(1)),
            "s2"
        );
    }

    #[test]
    fn overlay_guids_map_to_the_slider_positions() {
        assert_eq!(overlay_mode_name(0, 0, 0, [0; 8]), "balanced");
        assert_eq!(
            overlay_mode_name(
                0xded5_74b5,
                0x45a0,
                0x4f42,
                [0x87, 0x37, 0x46, 0x34, 0x5c, 0x09, 0xc2, 0x38]
            ),
            "best-performance"
        );
        assert_eq!(overlay_mode_name(1, 2, 3, [4; 8]), "other");
    }

    #[cfg(windows)]
    #[test]
    fn sampling_this_machine_reports_a_plan() {
        assert!(!sample().plan.is_empty());
    }
}
