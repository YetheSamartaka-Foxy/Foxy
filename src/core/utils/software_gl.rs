//! Windows CPU OpenGL fallback via bundled Mesa llvmpipe when only virtual display adapters exist.

use std::sync::atomic::{AtomicBool, Ordering};

/// Floor between software-drawn frames so CPU rendering does not saturate servers without a GPU driver.
pub const SOFTWARE_UI_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

static SOFTWARE_UI: AtomicBool = AtomicBool::new(false);

pub fn software_ui_active() -> bool {
    SOFTWARE_UI.load(Ordering::Relaxed)
}

pub fn mark_software_ui() {
    SOFTWARE_UI.store(true, Ordering::Relaxed);
}

/// Cap llvmpipe rasterizer threads; Mesa otherwise spawns one per logical CPU.
pub fn llvmpipe_thread_count(cpus: usize) -> usize {
    cpus.clamp(1, 4)
}

pub fn is_software_renderer(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "llvmpipe",
        "softpipe",
        "swrast",
        "swiftshader",
        "basic render",
        "basic display",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Rejects Microsoft virtual adapters (Basic Display/Render, Remote Display, Hyper-V Video, `VEN_1414`).
pub fn is_hardware_display(device_string: &str, device_id: &str) -> bool {
    let name = device_string.trim();
    if name.is_empty() {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if lower.contains("basic display")
        || lower.contains("basic render")
        || lower.contains("remote display")
        || (lower.contains("hyper-v") && lower.contains("video"))
    {
        return false;
    }
    let id = device_id.to_ascii_lowercase();
    if id.contains("ven_1414") || id.contains(r"root\basicdisplay") {
        return false;
    }
    true
}

pub fn mesa_libraries_beside_exe() -> bool {
    #[cfg(windows)]
    {
        let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        else {
            return false;
        };
        dir.join("libgallium_wgl.dll").is_file() && dir.join("opengl32.dll").is_file()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Direct bundled Mesa to llvmpipe before window creation (Mesa's default D3D driver needs unshipped DLLs);
/// user-set `GALLIUM_DRIVER` and `LP_NUM_THREADS` win.
pub fn configure_bundled_mesa() -> bool {
    if !mesa_libraries_beside_exe() {
        return false;
    }
    if std::env::var_os("GALLIUM_DRIVER").is_none() {
        set_process_env("GALLIUM_DRIVER", "llvmpipe");
    }
    if std::env::var_os("LP_NUM_THREADS").is_none() {
        let cpus = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        set_process_env("LP_NUM_THREADS", &llvmpipe_thread_count(cpus).to_string());
    }
    let driver = std::env::var("GALLIUM_DRIVER").unwrap_or_default();
    if is_software_renderer(&driver) {
        mark_software_ui();
    }
    true
}

fn set_process_env(name: &str, value: &str) {
    #[cfg(windows)]
    {
        let wide = |text: &str| {
            use std::os::windows::ffi::OsStrExt;
            std::ffi::OsStr::new(text)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let key = wide(name);
        let val = wide(value);
        // SAFETY: both strings are NUL-terminated and live for the call.
        // SetEnvironmentVariableW updates the process environment Mesa reads
        // without mutating Rust's environment lock.
        unsafe {
            if SetEnvironmentVariableW(key.as_ptr(), val.as_ptr()) == 0 {
                log::warn!("Could not set {name} for CPU drawing");
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (name, value);
    }
}

pub fn note_software_renderer(name: &str) {
    if is_software_renderer(name) {
        mark_software_ui();
        log::info!(
            "The window is drawn on the CPU ({name}). Progress updates are limited to 4 per second. Delete opengl32.dll and libgallium_wgl.dll next to Foxy.exe to use a graphics driver instead."
        );
    }
}

pub fn graphics_startup_hint(error: &str) -> Option<String> {
    let lower = error.to_ascii_lowercase();
    let graphics = [
        "opengl",
        "glutin",
        "wgl",
        "wgpu",
        "adapter",
        "egui_glow",
        "pixel format",
        "gl context",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if !graphics {
        return None;
    }
    let mut hint = String::from(
        "Foxy draws its window with OpenGL 2.0 or newer, or with WGPU. Without a graphics driver (a server without a graphics card, or a virtual machine whose display is the Microsoft Basic Display Adapter), Windows offers only OpenGL 1.1.",
    );
    if mesa_libraries_beside_exe() {
        hint.push_str(
            " CPU drawing is already next to Foxy.exe. Install the graphics driver, or turn on GPU acceleration for the virtual machine, and start Foxy again.",
        );
    } else {
        hint.push_str(
            "\n\nRun this in PowerShell. It places CPU drawing next to Foxy when Windows has no graphics driver, then starts Foxy again:\n\nirm https://yethesamartaka-foxy.github.io/Foxy/install.ps1 | iex\n\nOr install the graphics driver, or turn on GPU acceleration for the virtual machine.",
        );
    }
    Some(hint)
}

/// Treats enumeration failure as hardware present so normal PCs keep their default renderer.
#[cfg(windows)]
pub fn hardware_display_present() -> bool {
    let found = enumerated_displays();
    if found.is_empty() {
        return true;
    }
    found.iter().any(|(name, id)| is_hardware_display(name, id))
}

#[cfg(windows)]
fn enumerated_displays() -> Vec<(String, String)> {
    let mut found = Vec::new();
    unsafe {
        let mut index = 0u32;
        loop {
            // SAFETY: DisplayDeviceW is a plain C struct. `cb` is set to its size
            // before EnumDisplayDevicesW writes the rest.
            let mut device: DisplayDeviceW = std::mem::zeroed();
            device.cb = std::mem::size_of::<DisplayDeviceW>() as u32;
            if EnumDisplayDevicesW(std::ptr::null(), index, &mut device, 0) == 0 {
                break;
            }
            index = index.saturating_add(1);
            if index > 32 {
                break;
            }
            found.push((wide_z(&device.device_string), wide_z(&device.device_id)));
        }
    }
    found
}

#[cfg(windows)]
#[repr(C)]
struct DisplayDeviceW {
    cb: u32,
    device_name: [u16; 32],
    device_string: [u16; 128],
    state_flags: u32,
    device_id: [u16; 128],
    device_key: [u16; 128],
}

#[cfg(windows)]
unsafe extern "system" {
    fn EnumDisplayDevicesW(
        device: *const u16,
        index: u32,
        display_device: *mut DisplayDeviceW,
        flags: u32,
    ) -> i32;
    fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> i32;
}

#[cfg(windows)]
fn wide_z(buf: &[u16]) -> String {
    let end = buf.iter().position(|unit| *unit == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llvmpipe_uses_at_most_four_threads() {
        assert_eq!(llvmpipe_thread_count(0), 1);
        assert_eq!(llvmpipe_thread_count(1), 1);
        assert_eq!(llvmpipe_thread_count(2), 2);
        assert_eq!(llvmpipe_thread_count(4), 4);
        assert_eq!(llvmpipe_thread_count(32), 4);
    }

    #[test]
    fn software_renderers_are_recognised() {
        assert!(is_software_renderer("llvmpipe (LLVM 21.1.0, 256 bits)"));
        assert!(is_software_renderer("softpipe"));
        assert!(is_software_renderer("Software Rasterizer (swrast)"));
        assert!(is_software_renderer("Google SwiftShader"));
        assert!(is_software_renderer(
            "D3D12 (Microsoft Basic Render Driver)"
        ));
        assert!(is_software_renderer("Microsoft Basic Display Adapter"));
        assert!(!is_software_renderer("NVIDIA GeForce RTX 4080"));
        assert!(!is_software_renderer("AMD Radeon(TM) Graphics"));
        assert!(!is_software_renderer(""));
    }

    #[test]
    fn virtual_adapters_are_not_hardware_displays() {
        assert!(!is_hardware_display(
            "Microsoft Basic Display Adapter",
            r"ROOT\BASICDISPLAY\0000"
        ));
        assert!(!is_hardware_display(
            "Microsoft Basic Render Driver",
            r"PCI\VEN_1414&DEV_008C"
        ));
        assert!(!is_hardware_display(
            "Microsoft Hyper-V Video",
            r"PCI\VEN_1414&DEV_5353"
        ));
        assert!(!is_hardware_display("Microsoft Remote Display Adapter", ""));
        assert!(is_hardware_display(
            "NVIDIA GeForce RTX 4080",
            r"PCI\VEN_10DE&DEV_2704"
        ));
        assert!(is_hardware_display("AMD Radeon Graphics", ""));
        assert!(!is_hardware_display("  ", ""));
    }

    #[cfg(windows)]
    #[test]
    fn display_enumeration_returns_readable_names() {
        for (name, id) in enumerated_displays() {
            assert!(
                name.chars().all(|c| !c.is_control()),
                "unreadable display name {name:?} {id:?}"
            );
            assert!(name.len() < 200 && id.len() < 400, "{name:?} {id:?}");
        }
    }

    #[test]
    fn a_non_graphics_startup_error_has_no_display_hint() {
        assert!(graphics_startup_hint("Settings folder is not writable").is_none());
        let hint = graphics_startup_hint("egui_glow requires opengl 2.0+").unwrap();
        assert!(hint.contains("OpenGL 1.1"));
        assert!(hint.contains("Basic Display Adapter"));
    }
}
