use super::app::Foxy;
use super::app::agent_driver::AgentGuiLaunchConfig;
use super::app::debug_modals::DebugModal;
use crate::core::utils::renderer_fallback::{
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

    let options = build_native_options(viewport);

    if let Err(err) = eframe::run_native(
        "Foxy",
        options,
        Box::new(move |cc| {
            Ok(Box::new(Foxy::new(
                cc,
                debug_mode,
                agent_gui.clone(),
                debug_modals.clone(),
            )))
        }),
    ) {
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

fn build_native_options(viewport: ViewportBuilder) -> NativeOptions {
    let mut options = NativeOptions {
        viewport,
        ..Default::default()
    };
    configure_renderer_fallback(&mut options);
    configure_native_graphics(&mut options);
    options
}

fn configure_renderer_fallback(options: &mut NativeOptions) {
    match preferred_renderer() {
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

fn preferred_renderer() -> PreferredRenderer {
    let had_wgpu_crash_marker = consume_wgpu_crash_marker();

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

    switch_renderer_setting_to_glow();

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

    if let Err(err) = std::fs::remove_file(&marker_path) {
        log::warn!(
            "Failed to remove consumed WGPU crash marker {}: {}",
            marker_path.display(),
            err
        );
    }

    true
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

#[cfg(target_os = "windows")]
fn configure_native_graphics(options: &mut NativeOptions) {
    use eframe::egui_wgpu::WgpuSetup;
    use eframe::wgpu;

    let WgpuSetup::CreateNew(create_new) = &mut options.wgpu_options.wgpu_setup else {
        return;
    };

    if wgpu::Backends::from_env().is_none() {
        create_new.instance_descriptor.backends =
            wgpu::Backends::DX12 | wgpu::Backends::VULKAN | wgpu::Backends::GL;
        log::info!(
            "Configured Windows graphics backends: DirectX 12 > Vulkan > OpenGL. Set WGPU_BACKEND to override."
        );
    }
}

#[cfg(target_os = "linux")]
fn configure_native_graphics(options: &mut NativeOptions) {
    use eframe::egui_wgpu::WgpuSetup;
    use eframe::wgpu;

    let WgpuSetup::CreateNew(create_new) = &mut options.wgpu_options.wgpu_setup else {
        return;
    };

    if wgpu::Backends::from_env().is_none() {
        create_new.instance_descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        log::info!(
            "Configured Linux graphics backends: Vulkan > OpenGL. Set WGPU_BACKEND to override."
        );
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn configure_native_graphics(_options: &mut NativeOptions) {}
