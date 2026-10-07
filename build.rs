use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    emit_build_metadata();
    copy_steamworks_redistributable();
    steamworks_runtime_search_path();

    println!("cargo:rerun-if-env-changed=BUILD_ICON");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    delay_load_steamworks();

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

/// Delay-load the Steamworks import on MSVC so a missing `steam_api64.dll` does
/// not stop the process from starting. Without this the whole app is
/// unlaunchable when the redistributable is not packaged.
///
/// Delay-loading only moves the failure: resolving the import still raises
/// `0xC06D007E` at the first Steamworks call. Nothing outside the
/// `foxy steam-helper` subprocess makes such a call, and `workshop::` checks the
/// library is present before spawning it, so the reachable failure is a clear
/// error rather than a crash.
fn delay_load_steamworks() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rustc-link-arg=/DELAYLOAD:steam_api64.dll");
    println!("cargo:rustc-link-arg=delayimp.lib");
}

/// Look for the Steamworks library next to the executable at runtime.
///
/// `steamworks-sys` emits a link search path into its own `OUT_DIR` and no
/// rpath, so an installed ELF/Mach-O binary would only find `libsteam_api.so`
/// via `LD_LIBRARY_PATH` or a system directory. Unlike Windows there is no
/// delay-load equivalent - the loader resolves `DT_NEEDED` before `main`, so
/// without this the installed binary does not start at all.
fn steamworks_runtime_search_path() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        }
        Ok("macos") => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
        }
        _ => {}
    }
}

/// Place the Steamworks redistributable next to the built executable.
///
/// `steamworks-sys` links the import library, so the shared library is a load-
/// time dependency of the whole binary: without it beside the exe the process
/// does not start at all (`STATUS_DLL_NOT_FOUND` on Windows), not just the
/// Workshop commands. `steamworks-sys` only unpacks it into its own `OUT_DIR`,
/// so copy it into the target profile directory that CI and the installer
/// package from.
fn copy_steamworks_redistributable() {
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    // OUT_DIR is `<target>/<profile>/build/<pkg>-<hash>/out`; the profile
    // directory holding the executable is four levels up.
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };

    let names: &[&str] = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => &["steam_api64.dll", "steam_api.dll"],
        Ok("macos") => &["libsteam_api.dylib"],
        _ => &["libsteam_api.so"],
    };

    for name in names {
        let Some(source) = find_steamworks_redistributable(profile_dir, name) else {
            continue;
        };
        let destination = profile_dir.join(name);
        if destination.exists() {
            continue;
        }
        if let Err(err) = std::fs::copy(&source, &destination) {
            println!("cargo:warning=Failed to stage {name} beside the executable: {err}");
        }
    }
}

/// Locate a redistributable inside any `build/steamworks-sys-*/out` directory
/// under the current target profile.
fn find_steamworks_redistributable(profile_dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(profile_dir.join("build")).ok()?;
    entries.flatten().find_map(|entry| {
        let file_name = entry.file_name();
        if !file_name.to_string_lossy().starts_with("steamworks-sys-") {
            return None;
        }
        let candidate = entry.path().join("out").join(name);
        candidate.is_file().then_some(candidate)
    })
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
