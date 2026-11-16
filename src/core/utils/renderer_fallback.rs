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

/// Backend used by the last launch that reached a window. Reuse it to avoid
/// loading every driver stack; unknown or failed records use the full list.
pub fn remembered_graphics_backend() -> Option<String> {
    parse_graphics_backend_record(&std::fs::read_to_string(graphics_backend_path()).ok()?)
}

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

/// Select the fallback rung from the prior launch marker; driver crashes leave
/// no log, so an uncleared marker advances the next launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsLaunchStage {
    Full,

    SafeBackend,

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

/// Stay on Glow after a Glow startup failure so diagnostics persist instead of cycling back into failed wgpu drivers.
pub fn next_launch_stage(previous: Option<GraphicsLaunchStage>) -> GraphicsLaunchStage {
    match previous {
        None => GraphicsLaunchStage::Full,
        Some(GraphicsLaunchStage::Full) => GraphicsLaunchStage::SafeBackend,
        Some(GraphicsLaunchStage::SafeBackend | GraphicsLaunchStage::Glow) => {
            GraphicsLaunchStage::Glow
        }
    }
}

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
