use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/build_info/changelog.rs"]
mod changelog;

fn main() {
    emit_app_version();
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

fn emit_app_version() {
    println!("cargo:rerun-if-changed=CHANGELOG.md");
    let changelog = std::fs::read_to_string("CHANGELOG.md").expect("Failed to read CHANGELOG.md");
    let version = changelog::latest_version(&changelog).expect("Invalid Foxy changelog version");
    println!("cargo:rustc-env=FOXY_VERSION={version}");
}

/// MSVC delay-loading lets Foxy start without Steamworks; its first call still faults if the DLL is absent.
/// Only the guarded steam-helper subprocess may resolve those imports.
fn delay_load_steamworks() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rustc-link-arg=/DELAYLOAD:steam_api64.dll");
    println!("cargo:rustc-link-arg=delayimp.lib");
}

/// ELF/Mach-O resolve Steamworks before main; the packaged library needs a relative runtime search path.
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

/// steamworks-sys stages its load-time dependency only in OUT_DIR; packaging needs it beside the executable.
fn copy_steamworks_redistributable() {
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };

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

fn emit_build_metadata() {
    println!("cargo:rustc-env=FOXY_GIT_HASH={}", git_short_hash());

    for path in [".git/HEAD", ".git/index"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(ref_path) = head_ref_path() {
        println!("cargo:rerun-if-changed={ref_path}");
    }
}

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

fn head_ref_path() -> Option<String> {
    let head = std::fs::read_to_string(".git/HEAD").ok()?;
    let reference = head.strip_prefix("ref:")?.trim();
    Some(format!(".git/{reference}"))
}
