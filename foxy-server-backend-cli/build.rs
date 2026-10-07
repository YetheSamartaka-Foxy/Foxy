use std::process::Command;

/// Bakes the source commit hash consumed by `src/build_info.rs`, mirroring the
/// desktop app's `build.rs` so both binaries report the checkout they were
/// built from.
fn main() {
    println!("cargo:rustc-env=FOXY_GIT_HASH={}", git_short_hash());

    // The build script runs in the package directory, so the git metadata that
    // should retrigger a rebuild lives in the workspace root.
    if let Some(git_dir) = git_dir() {
        for name in ["HEAD", "index"] {
            let path = format!("{git_dir}/{name}");
            if std::path::Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
        if let Some(ref_path) = head_ref_path(&git_dir) {
            println!("cargo:rerun-if-changed={ref_path}");
        }
    }
}

fn git_stdout(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Short commit hash of `HEAD`, with a `-dirty` suffix when the working tree
/// has uncommitted changes. Returns `"unknown"` when git is unavailable or the
/// source is not a git checkout.
fn git_short_hash() -> String {
    let Some(rev) = git_stdout(&["rev-parse", "--short", "HEAD"]) else {
        return "unknown".to_string();
    };
    let dirty = git_stdout(&["status", "--porcelain"]).is_some();
    if dirty { format!("{rev}-dirty") } else { rev }
}

fn git_dir() -> Option<String> {
    git_stdout(&["rev-parse", "--git-dir"])
}

/// Path to the ref file `HEAD` points at, so a commit on that branch retriggers
/// the build script.
fn head_ref_path(git_dir: &str) -> Option<String> {
    let head = std::fs::read_to_string(format!("{git_dir}/HEAD")).ok()?;
    let reference = head.strip_prefix("ref:")?.trim();
    Some(format!("{git_dir}/{reference}"))
}
