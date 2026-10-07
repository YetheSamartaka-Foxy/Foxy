//! Build metadata baked in at compile time by `build.rs`.
//!
//! Lets a running binary report whether it is an official release build, a
//! pre-release build, or a local/IDE dev build, and which commit it was built
//! from.
//!
//! The kind is derived from the compile profile, not from any environment:
//! - **dev**: debug profile (`cargo run`, `cargo build`, the VS Code "Run"
//!   button).
//! - **pre-release**: release profile built with the `prerelease` feature
//!   (`cargo prerelease`).
//! - **release**: plain release profile (`cargo build --release`, the GitHub
//!   artifacts).
//!
//! Every kind shows the source commit so a release-profile build made from a
//! local checkout can still be told apart from the published artifact of the
//! same version.

use std::sync::LazyLock;

/// Package version from `Cargo.toml`, e.g. `1.0.0`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git commit the binary was built from, with a `-dirty` suffix when the
/// working tree had uncommitted changes, or `"unknown"` when unavailable.
pub const GIT_HASH: &str = env!("FOXY_GIT_HASH");

/// Whether this is a debug/dev build (VS Code "Run", `cargo run`/`build`).
pub fn is_dev_build() -> bool {
    cfg!(debug_assertions)
}

/// Whether this is a release build explicitly marked as a pre-release
/// (`cargo build --release --features prerelease`).
pub fn is_prerelease_build() -> bool {
    !cfg!(debug_assertions) && cfg!(feature = "prerelease")
}

/// Whether this is a plain release build - the official distributed artifact.
pub fn is_official_build() -> bool {
    !cfg!(debug_assertions) && !cfg!(feature = "prerelease")
}

/// `"dev"`, `"prerelease"`, or `"release"`. For logs and diagnostics.
pub fn build_kind() -> &'static str {
    if is_dev_build() {
        "dev"
    } else if is_prerelease_build() {
        "prerelease"
    } else {
        "release"
    }
}

/// Version label for display.
///
/// Always carries the source commit (`v1.0.0 (a1b2c3d)`); dev and pre-release
/// builds also mark the kind (`v1.0.0-dev (a1b2c3d-dirty)` / `v1.0.0-pre (a1b2c3d)`)
/// so the running binary can be matched back to the checkout it was built from.
pub fn version_label() -> String {
    if is_dev_build() {
        format!("v{VERSION}-dev ({GIT_HASH})")
    } else if is_prerelease_build() {
        format!("v{VERSION}-pre ({GIT_HASH})")
    } else {
        format!("v{VERSION} ({GIT_HASH})")
    }
}

/// `version_label()` without the `v` prefix, for clap's `--version` output.
pub fn clap_version() -> &'static str {
    static LABEL: LazyLock<String> = LazyLock::new(|| {
        version_label()
            .strip_prefix('v')
            .map(str::to_string)
            .unwrap_or_else(version_label)
    });
    LABEL.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_label_always_carries_commit() {
        let label = version_label();
        assert!(label.starts_with(&format!("v{VERSION}")));
        assert!(label.ends_with(&format!("({GIT_HASH})")));
    }

    #[test]
    fn clap_version_drops_v_prefix() {
        assert_eq!(format!("v{}", clap_version()), version_label());
    }
}
