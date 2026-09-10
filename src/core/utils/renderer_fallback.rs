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

#[cfg(test)]
mod tests {
    use super::parse_graphics_backend_record;

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
}
