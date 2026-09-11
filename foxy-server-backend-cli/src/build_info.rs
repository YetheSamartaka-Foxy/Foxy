//! Build metadata baked in at compile time by `build.rs`.
//!
//! Mirrors the desktop app's `build_info`: the version always carries the
//! source commit so a binary built from a local checkout can be told apart
//! from the published artifact of the same version.

use std::sync::LazyLock;

/// Package version from `Cargo.toml`, e.g. `1.0.0`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git commit the binary was built from, with a `-dirty` suffix when the
/// working tree had uncommitted changes, or `"unknown"` when unavailable.
pub const GIT_HASH: &str = env!("FOXY_GIT_HASH");

/// `1.0.0 (a1b2c3d)` for release builds, `1.0.0-dev (a1b2c3d-dirty)` for debug
/// builds.
pub fn version_label() -> String {
    if cfg!(debug_assertions) {
        format!("{VERSION}-dev ({GIT_HASH})")
    } else {
        format!("{VERSION} ({GIT_HASH})")
    }
}

/// `version_label()` as a static string for clap's `--version`.
pub fn clap_version() -> &'static str {
    static LABEL: LazyLock<String> = LazyLock::new(version_label);
    LABEL.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_label_carries_version_and_commit() {
        let label = version_label();
        assert!(label.starts_with(VERSION));
        assert!(label.ends_with(&format!("({GIT_HASH})")));
        assert_eq!(clap_version(), label);
    }
}
