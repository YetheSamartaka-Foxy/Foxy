//! Build metadata baked in at compile time by `build.rs`.
//!
//! Lets a running binary report whether it is an official release build, a
//! pre-release build, or a local/IDE dev build, and which commit it was built
//! from.
//!
//! The kind is derived from the compile profile, not from any environment:
//! - **dev**: debug profile (`cargo run`, `cargo build`, the VS Code "Run"
//!   button) - shows the source commit.
//! - **pre-release**: release profile built with the `prerelease` feature
//!   (`cargo prerelease`) - release-optimized but still shows the commit.
//! - **release**: plain release profile (`cargo build --release`, the GitHub
//!   artifacts) - shows just the version.

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
/// Official builds show just the version (`v1.0.0`); dev and pre-release builds
/// append the source commit (`v1.0.0-dev (a1b2c3d)` / `v1.0.0-pre (a1b2c3d)`)
/// so the running binary can be matched back to the checkout it was built from.
pub fn version_label() -> String {
    if is_dev_build() {
        format!("v{VERSION}-dev ({GIT_HASH})")
    } else if is_prerelease_build() {
        format!("v{VERSION}-pre ({GIT_HASH})")
    } else {
        format!("v{VERSION}")
    }
}
