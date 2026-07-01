use std::process::Command;

fn main() {
    emit_build_metadata();

    println!("cargo:rerun-if-env-changed=BUILD_ICON");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let build_icon = std::env::var("BUILD_ICON")
        .map(|value| value == "true" || value == "1")
        .unwrap_or(false);

    if build_icon {
        let output = std::process::Command::new("windres")
            .args(["-i", "app_icon.rc", "-o", "app_icon.o"])
            .status()
            .expect("Failed to run windres");

        if !output.success() {
            panic!("Failed to generate icon resource!");
        }
    }

    println!("cargo:rustc-link-arg=app_icon.o");
}

/// Bakes the source commit hash consumed by `src/build_info.rs`.
///
/// The build kind (dev / pre-release / release) is derived from the compile
/// profile in `build_info.rs`, not here; this only captures the commit so a
/// running binary can be matched back to the exact checkout it came from.
fn emit_build_metadata() {
    println!("cargo:rustc-env=FOXY_GIT_HASH={}", git_short_hash());

    // Rebuild when the checked-out commit or working tree changes so the baked
    // dev hash stays accurate.
    for path in [".git/HEAD", ".git/index"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(ref_path) = head_ref_path() {
        println!("cargo:rerun-if-changed={ref_path}");
    }
}

/// Short commit hash of `HEAD`, with a `-dirty` suffix when the working tree
/// has uncommitted changes. Returns `"unknown"` when git is unavailable or the
/// source is not a git checkout.
fn git_short_hash() -> String {
    let rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty());

    let Some(rev) = rev else {
        return "unknown".to_string();
    };

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| !out.stdout.is_empty())
        .unwrap_or(false);

    if dirty { format!("{rev}-dirty") } else { rev }
}

/// Path to the ref file `HEAD` points at, so a commit on that branch retriggers
/// the build script.
fn head_ref_path() -> Option<String> {
    let head = std::fs::read_to_string(".git/HEAD").ok()?;
    let reference = head.strip_prefix("ref:")?.trim();
    Some(format!(".git/{reference}"))
}
