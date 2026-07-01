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
