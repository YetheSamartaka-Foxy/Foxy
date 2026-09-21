use super::app::Foxy;
use super::app::agent_driver::AgentGuiLaunchConfig;
use super::app::debug_modals::DebugModal;
use crate::core::utils::renderer_fallback::{
    GraphicsLaunchStage, clear_launch_attempt, forget_graphics_backend, next_launch_stage,
    previous_launch_attempt, record_launch_attempt, remembered_graphics_backend,
    renderer_fallback_notice_path, wgpu_crash_marker_path,
};
use crate::ui::types::{SettingsViewState, UiRendererPreference};
use eframe::NativeOptions;
use eframe::egui::{self, IconData};
use egui::Vec2;
use egui::ViewportBuilder;

pub(crate) const USE_DECORATIONS: bool = false;
const DEFAULT_WINDOW_SIZE: [f32; 2] = [1024.0, 768.0];
const MIN_WINDOW_SIZE: [f32; 2] = [800.0, 600.0];
const MAX_RESTORED_WINDOW_SIZE: [f32; 2] = [3840.0, 2160.0];
/// Largest absolute window position (logical px) we will restore. Multi-monitor
/// layouts can use negative coordinates, but anything beyond this is corrupt
/// (e.g. geometry distorted by a tiny UI scale) and would place the window
/// off-screen, so we ignore it and let the OS position the window.
const MAX_RESTORED_WINDOW_POSITION: f32 = 32000.0;

pub(crate) fn main(
    debug_mode: bool,
    agent_gui: AgentGuiLaunchConfig,
    debug_modals: Vec<DebugModal>,
) {
    let icon = include_bytes!("icons/foxy_256.png");
    let image = match image::load_from_memory(icon) {
        Ok(img) => img.to_rgba8(),
        Err(err) => {
            log::error!("Failed to load application icon: {}", err);
            eprintln!("FATAL: Failed to load application icon: {}", err);
            std::process::exit(1);
        }
    };
    let (icon_width, icon_height) = image.dimensions();
    let viewport = build_root_viewport(image.into_raw(), icon_width, icon_height);

    // Whether eframe got as far as constructing the app. A narrowed launch that
    // fails does so while creating the instance, adapter or device - before this
    // is set - so it is what separates "never started" from "ran and then
    // returned an error", and it is why a retry can never open a second window
    // over a session the user already used.
    let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stage = launch_stage_after_previous_attempt();
    let run = |viewport: ViewportBuilder, pin_backend: bool| {
        let (options, pinned) = build_native_options(viewport, pin_backend, stage);
        let debug_modals = debug_modals.clone();
        let agent_gui = agent_gui.clone();
        let started = started.clone();
        record_launch_attempt(stage);
        let result = eframe::run_native(
            "Foxy",
            options,
            Box::new(move |cc| {
                started.store(true, std::sync::atomic::Ordering::SeqCst);
                clear_launch_attempt();
                Ok(Box::new(Foxy::new(
                    cc,
                    debug_mode,
                    agent_gui.clone(),
                    debug_modals.clone(),
                )))
            }),
        );
        (result, pinned)
    };

    let (result, pinned) = run(viewport.clone(), true);
    let result = match result {
        Err(err) if pinned && !started.load(std::sync::atomic::Ordering::SeqCst) => {
            // The remembered backend no longer resolves to an adapter - a driver
            // change, a swapped GPU, a remote session. Re-enumerating every
            // backend is exactly what the record is a shortcut for, so drop it
            // and take the slow path rather than failing a launch that would
            // have worked.
            log::warn!(
                "Failed to start Foxy UI on the remembered graphics backend ({}); retrying with every supported backend",
                err
            );
            forget_graphics_backend();
            run(viewport, false).0
        }
        other => other,
    };

    if let Err(err) = result {
        log::error!("Failed to start Foxy UI: {}", err);
        eprintln!("FATAL: Failed to start Foxy UI: {}", err);
        eprintln!("This may be caused by graphics driver issues.");
        std::process::exit(1);
    }
}

fn build_root_viewport(icon_rgba: Vec<u8>, icon_width: u32, icon_height: u32) -> ViewportBuilder {
    let mut viewport = ViewportBuilder::default()
        .with_icon(IconData {
            rgba: icon_rgba,
            width: icon_width,
            height: icon_height,
        })
        .with_inner_size(DEFAULT_WINDOW_SIZE)
        .with_min_inner_size(Vec2::new(MIN_WINDOW_SIZE[0], MIN_WINDOW_SIZE[1]))
        .with_resizable(true)
        .with_decorations(USE_DECORATIONS);

    #[cfg(target_os = "windows")]
    {
        viewport = viewport.with_transparent(false);
    }

    if let Some(window_state) = Foxy::load_window_state() {
        if let Some(size) = window_state.size {
            if size[0].is_finite()
                && size[1].is_finite()
                && size[0] >= MIN_WINDOW_SIZE[0]
                && size[1] >= MIN_WINDOW_SIZE[1]
                && size[0] <= MAX_RESTORED_WINDOW_SIZE[0]
                && size[1] <= MAX_RESTORED_WINDOW_SIZE[1]
            {
                log::info!(
                    "Setting app resolution from saved window state: {}x{}",
                    size[0].round() as i32,
                    size[1].round() as i32
                );
                viewport = viewport.with_inner_size(size);
            } else {
                log::warn!(
                    "Ignoring out-of-range saved app resolution {}x{}; using default {}x{}",
                    size[0],
                    size[1],
                    DEFAULT_WINDOW_SIZE[0] as i32,
                    DEFAULT_WINDOW_SIZE[1] as i32
                );
            }
        }

        if let Some(position) = window_state.position {
            if position[0].is_finite()
                && position[1].is_finite()
                && position[0].abs() <= MAX_RESTORED_WINDOW_POSITION
                && position[1].abs() <= MAX_RESTORED_WINDOW_POSITION
            {
                viewport = viewport.with_position(position);
            } else {
                log::warn!(
                    "Ignoring out-of-range saved window position {}x{}; letting the OS place the window",
                    position[0],
                    position[1]
                );
            }
        }

        if window_state.maximized {
            viewport = viewport.with_maximized(true);
        }
    } else {
        log::info!(
            "Setting default app resolution: {}x{}",
            DEFAULT_WINDOW_SIZE[0] as i32,
            DEFAULT_WINDOW_SIZE[1] as i32
        );
    }

    viewport
}

/// Pick the fallback rung for this launch from the marker a previous launch
/// left behind when it died before constructing the app.
///
/// A returned error is logged and exits normally, so the marker mostly records
/// native crashes in the graphics stack that no panic hook ever sees. Reaching
/// the app clears it, so a working machine never pays for this.
fn launch_stage_after_previous_attempt() -> GraphicsLaunchStage {
    let previous = previous_launch_attempt();
    let stage = next_launch_stage(previous);
    if let Some(previous) = previous {
        log::warn!(
            "The previous UI launch (graphics stage {:?}) ended before the app window was constructed; starting this launch at graphics stage {:?}",
            previous,
            stage
        );
    }
    stage
}

/// Build the eframe options, optionally narrowing wgpu to the backend a previous
/// launch proved. Returns whether the narrowing was actually applied, which is
/// what makes a failed launch retryable rather than fatal.
fn build_native_options(
    viewport: ViewportBuilder,
    pin_backend: bool,
    stage: GraphicsLaunchStage,
) -> (NativeOptions, bool) {
    let mut options = NativeOptions {
        viewport,
        ..Default::default()
    };
    configure_renderer_fallback(&mut options, stage);
    let pinned = configure_native_graphics(&mut options, pin_backend, stage);
    configure_graphics_memory_hints(&mut options);
    (options, pinned)
}

/// Ask wgpu's allocators to size for footprint rather than for throughput.
///
/// wgpu defaults to [`wgpu::MemoryHints::Performance`], which sizes its
/// suballocation blocks for a renderer streaming large resources. Foxy uploads
/// a font atlas and a handful of repository images, so those blocks are commit
/// that never holds a Foxy texture.
fn configure_graphics_memory_hints(options: &mut NativeOptions) {
    use eframe::egui_wgpu::WgpuSetup;
    use eframe::wgpu;

    let WgpuSetup::CreateNew(create_new) = &mut options.wgpu_options.wgpu_setup else {
        return;
    };
    let base = create_new.device_descriptor.clone();
    create_new.device_descriptor = std::sync::Arc::new(move |adapter| wgpu::DeviceDescriptor {
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..base(adapter)
    });
}

fn configure_renderer_fallback(options: &mut NativeOptions, stage: GraphicsLaunchStage) {
    match preferred_renderer(stage) {
        PreferredRenderer::Wgpu => {
            options.renderer = eframe::Renderer::Wgpu;
            log::info!("Configured UI renderer: wgpu");
        }
        PreferredRenderer::Glow { reason } => {
            options.renderer = eframe::Renderer::Glow;
            log::warn!(
                "Configured UI renderer: glow ({reason}). Set FOXY_RENDERER=wgpu to force WGPU."
            );
        }
    }
}

enum PreferredRenderer {
    Wgpu,
    Glow { reason: String },
}

fn preferred_renderer(stage: GraphicsLaunchStage) -> PreferredRenderer {
    let had_wgpu_crash_marker = consume_wgpu_crash_marker();
    let died_in_wgpu_startup = stage == GraphicsLaunchStage::Glow;
    if died_in_wgpu_startup {
        persist_glow_fallback();
    }

    if let Ok(renderer) = std::env::var("FOXY_RENDERER") {
        let renderer = renderer.trim().to_ascii_lowercase();
        if renderer == "glow" || renderer == "opengl" || renderer == "gl" {
            return PreferredRenderer::Glow {
                reason: "FOXY_RENDERER override".to_string(),
            };
        }
        if renderer == "wgpu" {
            return PreferredRenderer::Wgpu;
        }
        log::warn!(
            "Ignoring unsupported FOXY_RENDERER value {:?}; expected wgpu or glow",
            renderer
        );
    }

    if had_wgpu_crash_marker {
        return PreferredRenderer::Glow {
            reason: "previous egui-wgpu panic; setting switched to Glow".to_string(),
        };
    }

    if died_in_wgpu_startup {
        return PreferredRenderer::Glow {
            reason: "previous launches died while starting wgpu; setting switched to Glow"
                .to_string(),
        };
    }

    match load_renderer_preference() {
        UiRendererPreference::Auto | UiRendererPreference::Wgpu => PreferredRenderer::Wgpu,
        UiRendererPreference::Glow => PreferredRenderer::Glow {
            reason: "configured in settings".to_string(),
        },
    }
}

fn load_renderer_preference() -> UiRendererPreference {
    let merged = match crate::core::game::spaces::read_merged_settings_value(
        &Foxy::get_app_settings_path(),
        &Foxy::get_game_settings_path(),
    ) {
        Ok(Some(merged)) => merged,
        Ok(None) => return UiRendererPreference::default(),
        Err(err) => {
            log::warn!(
                "Failed to load settings while resolving renderer preference: {}",
                err
            );
            return UiRendererPreference::default();
        }
    };
    match serde_json::from_value::<SettingsViewState>(merged) {
        Ok(settings) => settings.ui_renderer,
        Err(err) => {
            log::warn!(
                "Failed to parse settings while resolving renderer preference: {}",
                err
            );
            UiRendererPreference::default()
        }
    }
}

fn consume_wgpu_crash_marker() -> bool {
    let marker_path = wgpu_crash_marker_path();
    if !marker_path.exists() {
        return false;
    }

    persist_glow_fallback();

    if let Err(err) = std::fs::remove_file(&marker_path) {
        log::warn!(
            "Failed to remove consumed WGPU crash marker {}: {}",
            marker_path.display(),
            err
        );
    }

    true
}

/// Switch the renderer setting to Glow and leave the notice the UI shows once.
fn persist_glow_fallback() {
    switch_renderer_setting_to_glow();
    // The remembered backend is the one that just crashed. Clear it so a later
    // return to wgpu re-enumerates instead of pinning the failure.
    forget_graphics_backend();

    let notice_path = renderer_fallback_notice_path();
    let notice_contents =
        "Foxy detected a previous WGPU renderer crash and switched the renderer setting to Glow.\n";
    if let Some(parent) = notice_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        log::warn!(
            "Failed to create renderer fallback notice directory {}: {}",
            parent.display(),
            err
        );
    } else if let Err(err) = std::fs::write(&notice_path, notice_contents) {
        log::warn!(
            "Failed to write renderer fallback notice marker {}: {}",
            notice_path.display(),
            err
        );
    }
}

fn switch_renderer_setting_to_glow() {
    let app_settings_path = Foxy::get_app_settings_path();
    let game_settings_path = Foxy::get_game_settings_path();
    let mut settings = match crate::core::game::spaces::read_merged_settings_value(
        &app_settings_path,
        &game_settings_path,
    ) {
        Ok(Some(merged)) => match serde_json::from_value::<SettingsViewState>(merged) {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!(
                    "Failed to parse settings while applying renderer fallback: {}",
                    err
                );
                return;
            }
        },
        Ok(None) => SettingsViewState::default(),
        Err(err) => {
            log::warn!(
                "Failed to read settings while applying renderer fallback: {}",
                err
            );
            return;
        }
    };

    if settings.ui_renderer == UiRendererPreference::Glow {
        return;
    }

    settings.ui_renderer = UiRendererPreference::Glow;
    let settings_value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(err) => {
            log::warn!("Failed to serialize renderer fallback setting: {}", err);
            return;
        }
    };
    if let Err(err) = crate::core::game::spaces::write_split_settings(
        &settings_value,
        &app_settings_path,
        &game_settings_path,
    ) {
        log::warn!("Failed to persist renderer fallback setting: {}", err);
    }
}

/// Backends this platform is willing to run on, in preference order.
#[cfg(target_os = "windows")]
fn platform_backends() -> eframe::wgpu::Backends {
    use eframe::wgpu::Backends;
    Backends::DX12 | Backends::VULKAN | Backends::GL
}

#[cfg(target_os = "linux")]
fn platform_backends() -> eframe::wgpu::Backends {
    use eframe::wgpu::Backends;
    Backends::VULKAN | Backends::GL
}

/// The single backend to try after a full enumeration killed the process.
///
/// Vulkan instance creation loads every installed ICD and implicit layer
/// (overlays, capture tools, stale drivers), which is where such crashes
/// almost always live. DX12 on Windows and GL on Linux touch none of that.
#[cfg(target_os = "windows")]
fn safe_backend() -> eframe::wgpu::Backends {
    eframe::wgpu::Backends::DX12
}

#[cfg(target_os = "linux")]
fn safe_backend() -> eframe::wgpu::Backends {
    eframe::wgpu::Backends::GL
}

/// Map a recorded backend name onto its single-backend bit.
///
/// The names are wgpu's own (`Backend::to_str`), which is also what
/// `WGPU_BACKEND` accepts, so a record and an override are spelled the same way
/// and an unrecognized name degrades to a full enumeration rather than to a
/// wrong backend.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn named_backend(name: &str) -> Option<eframe::wgpu::Backends> {
    eframe::wgpu::Backend::ALL
        .into_iter()
        .find(|backend| backend.to_str() == name)
        .map(eframe::wgpu::Backends::from)
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn configure_native_graphics(
    options: &mut NativeOptions,
    pin_backend: bool,
    stage: GraphicsLaunchStage,
) -> bool {
    use eframe::egui_wgpu::WgpuSetup;
    use eframe::wgpu;

    let WgpuSetup::CreateNew(create_new) = &mut options.wgpu_options.wgpu_setup else {
        return false;
    };
    if wgpu::Backends::from_env().is_some() {
        return false;
    }

    if stage == GraphicsLaunchStage::SafeBackend {
        // The record names a backend that once worked, but the enumeration it
        // shortcuts just killed the process; the safe backend is chosen
        // outright and gets re-recorded if it reaches a window.
        forget_graphics_backend();
        create_new.instance_descriptor.backends = safe_backend();
        log::warn!(
            "Configured graphics backend {:?} only, because the previous launch died while enumerating every backend. Set WGPU_BACKEND to override.",
            safe_backend()
        );
        return false;
    }

    let supported = platform_backends();
    let pinned = pin_backend
        .then(remembered_graphics_backend)
        .flatten()
        .and_then(|name| named_backend(&name))
        .filter(|backend| supported.contains(*backend));
    create_new.instance_descriptor.backends = pinned.unwrap_or(supported);
    match pinned {
        Some(backend) => log::info!(
            "Configured graphics backend {:?} from the last successful launch. Set WGPU_BACKEND to override.",
            backend
        ),
        None => log::info!(
            "Configured graphics backends: {:?}. Set WGPU_BACKEND to override.",
            supported
        ),
    }
    pinned.is_some()
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn configure_native_graphics(
    _options: &mut NativeOptions,
    _pin_backend: bool,
    _stage: GraphicsLaunchStage,
) -> bool {
    false
}
