use std::path::PathBuf;

use super::app_paths;

pub const WGPU_CRASH_MARKER_FILE: &str = "wgpu_crash.flag";
pub const RENDERER_FALLBACK_NOTICE_FILE: &str = "renderer_fallback_notice.flag";

pub fn wgpu_crash_marker_path() -> PathBuf {
    app_paths::foxy_data_dir().join(WGPU_CRASH_MARKER_FILE)
}

pub fn renderer_fallback_notice_path() -> PathBuf {
    app_paths::foxy_data_dir().join(RENDERER_FALLBACK_NOTICE_FILE)
}

pub const GRAPHICS_BACKEND_FILE: &str = "graphics_backend.txt";

pub fn graphics_backend_path() -> PathBuf {
    app_paths::foxy_data_dir().join(GRAPHICS_BACKEND_FILE)
}

/// The graphics backend a previous launch reached a window with.
///
/// wgpu creates an instance per backend it is allowed to consider, and each one
/// loads its driver stack, so enumerating three backends costs a launcher that
/// draws a font atlas and a few images the memory of three. Remembering the one
/// that worked lets the next launch ask for that one alone; the full list is
/// still what a first launch, an unknown answer, or a failed narrowed launch
/// uses, so no machine loses a backend it needs.
pub fn remembered_graphics_backend() -> Option<String> {
    parse_graphics_backend_record(&std::fs::read_to_string(graphics_backend_path()).ok()?)
}

/// Read a backend name out of the record file's contents.
///
/// A leading BOM is stripped: an editor or a shell redirect can add one, and a
/// record that silently fails to parse costs a launch the narrowing it earned
/// without saying so. Anything that is not a bare alphanumeric token is
/// rejected, which is what keeps a corrupt file from reaching the backend map.
fn parse_graphics_backend_record(raw: &str) -> Option<String> {
    let value = raw
        .trim_start_matches('\u{feff}')
        .trim()
        .to_ascii_lowercase();
    (!value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric())).then_some(value)
}

pub fn remember_graphics_backend(backend: &str) {
    let backend = backend.trim().to_ascii_lowercase();
    if backend.is_empty() || remembered_graphics_backend().as_deref() == Some(backend.as_str()) {
        return;
    }
    let path = graphics_backend_path();
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        log::warn!(
            "Failed to create graphics backend record directory: {}",
            err
        );
        return;
    }
    if let Err(err) = std::fs::write(&path, format!("{backend}\n")) {
        log::warn!("Failed to record the graphics backend: {}", err);
    }
}

/// Drop the record so the next launch enumerates every backend again.
pub fn forget_graphics_backend() {
    let path = graphics_backend_path();
    if path.exists()
        && let Err(err) = std::fs::remove_file(&path)
    {
        log::warn!("Failed to clear the graphics backend record: {}", err);
    }
}

pub const UI_LAUNCH_ATTEMPT_FILE: &str = "ui_launch_attempt.flag";

pub fn ui_launch_attempt_path() -> PathBuf {
    app_paths::foxy_data_dir().join(UI_LAUNCH_ATTEMPT_FILE)
}

/// How far down the graphics fallback ladder a UI launch starts.
///
/// A launch that dies inside the driver stack (an access violation in a Vulkan
/// ICD or an injected overlay layer) takes the whole process with it: no panic
/// hook runs and nothing reaches the log. The only evidence is the attempt
/// marker written just before eframe starts and cleared once the app is
/// constructed, so each launch that leaves it behind moves one rung down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsLaunchStage {
    /// wgpu on every supported backend, or the remembered one.
    Full,
    /// wgpu narrowed to the platform's most conservative single backend.
    SafeBackend,
    /// The Glow (OpenGL) renderer, persisted into the renderer setting.
    Glow,
}

impl GraphicsLaunchStage {
    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SafeBackend => "safe-backend",
            Self::Glow => "glow",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Self::Full),
            "safe-backend" => Some(Self::SafeBackend),
            "glow" => Some(Self::Glow),
            _ => None,
        }
    }
}

/// The rung a launch starts on given the stage of a previous launch that never
/// reached the app. Glow is the last rung; a launch that dies there stays on it
/// so the failure keeps being logged instead of cycling back into wgpu.
pub fn next_launch_stage(previous: Option<GraphicsLaunchStage>) -> GraphicsLaunchStage {
    match previous {
        None => GraphicsLaunchStage::Full,
        Some(GraphicsLaunchStage::Full) => GraphicsLaunchStage::SafeBackend,
        Some(GraphicsLaunchStage::SafeBackend | GraphicsLaunchStage::Glow) => {
            GraphicsLaunchStage::Glow
        }
    }
}

/// The stage of a previous UI launch that died before constructing the app.
pub fn previous_launch_attempt() -> Option<GraphicsLaunchStage> {
    parse_launch_attempt_record(&std::fs::read_to_string(ui_launch_attempt_path()).ok()?)
}

fn parse_launch_attempt_record(raw: &str) -> Option<GraphicsLaunchStage> {
    raw.trim_start_matches('\u{feff}')
        .lines()
        .find_map(|line| line.trim().strip_prefix("stage="))
        .and_then(|name| GraphicsLaunchStage::from_name(name.trim()))
}

pub fn record_launch_attempt(stage: GraphicsLaunchStage) {
    let path = ui_launch_attempt_path();
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        log::warn!(
            "Failed to create the UI launch attempt marker directory: {}",
            err
        );
        return;
    }
    let contents = format!(
        "stage={}\nPresent while a Foxy UI launch is in progress. Left behind when the launch died before the window opened.\n",
        stage.name()
    );
    if let Err(err) = std::fs::write(&path, contents) {
        log::warn!("Failed to write the UI launch attempt marker: {}", err);
    }
}

pub fn clear_launch_attempt() {
    if let Err(err) = std::fs::remove_file(ui_launch_attempt_path())
        && err.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("Failed to clear the UI launch attempt marker: {}", err);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GraphicsLaunchStage, next_launch_stage, parse_graphics_backend_record,
        parse_launch_attempt_record,
    };

    #[test]
    fn accepts_a_plain_token_with_any_line_ending_or_bom() {
        for raw in [
            "vulkan",
            "vulkan\n",
            "vulkan\r\n",
            " VULKAN ",
            "\u{feff}vulkan\n",
        ] {
            assert_eq!(
                parse_graphics_backend_record(raw).as_deref(),
                Some("vulkan"),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_and_non_token_contents() {
        for raw in ["", "   ", "\u{feff}", "dx 12", "vulkan;rm -rf", "../../etc"] {
            assert!(
                parse_graphics_backend_record(raw).is_none(),
                "{raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn launch_ladder_steps_down_once_per_dead_launch_and_stops_at_glow() {
        assert_eq!(next_launch_stage(None), GraphicsLaunchStage::Full);
        assert_eq!(
            next_launch_stage(Some(GraphicsLaunchStage::Full)),
            GraphicsLaunchStage::SafeBackend
        );
        assert_eq!(
            next_launch_stage(Some(GraphicsLaunchStage::SafeBackend)),
            GraphicsLaunchStage::Glow
        );
        assert_eq!(
            next_launch_stage(Some(GraphicsLaunchStage::Glow)),
            GraphicsLaunchStage::Glow
        );
    }

    #[test]
    fn launch_attempt_record_round_trips_and_rejects_garbage() {
        for stage in [
            GraphicsLaunchStage::Full,
            GraphicsLaunchStage::SafeBackend,
            GraphicsLaunchStage::Glow,
        ] {
            let raw = format!(
                "\u{feff}stage={}\r\nsome explanatory text\r\n",
                stage.name()
            );
            assert_eq!(parse_launch_attempt_record(&raw), Some(stage), "{raw:?}");
        }
        for raw in ["", "stage=", "stage=vulkan", "full", "note\nstage = full"] {
            assert!(
                parse_launch_attempt_record(raw).is_none(),
                "{raw:?} should be rejected"
            );
        }
    }
}
