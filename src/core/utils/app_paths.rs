use std::fs;
use std::path::PathBuf;

const APP_DIR_NAME: &str = "Foxy";

fn platform_config_base_dir() -> PathBuf {
    if let Some(dir) = dirs::config_dir() {
        return dir;
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.trim().is_empty()
        {
            return PathBuf::from(appdata);
        }

        if let Some(home) = dirs::home_dir() {
            return home.join("AppData").join("Roaming");
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg_config_home.trim().is_empty() {
                return PathBuf::from(xdg_config_home);
            }
        }

        if let Some(home) = dirs::home_dir() {
            return home.join(".config");
        }
    }

    PathBuf::from(".")
}

fn override_data_dir() -> Option<PathBuf> {
    let override_dir = std::env::var("FOXY_CONFIG_DIR").ok()?;
    let trimmed = override_dir.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn ensure_directory(path: &PathBuf, label: &str) -> bool {
    match fs::create_dir_all(path) {
        Ok(_) => true,
        Err(err) => {
            log::error!(
                "Failed to create {} directory {}: {}",
                label,
                path.display(),
                err
            );
            false
        }
    }
}

pub fn foxy_data_dir() -> PathBuf {
    if let Some(override_dir) = override_data_dir()
        && ensure_directory(&override_dir, "override app data")
    {
        return override_dir;
    }

    let mut preferred = platform_config_base_dir();
    preferred.push(APP_DIR_NAME);
    if ensure_directory(&preferred, "app data") {
        return preferred;
    }

    let fallback = PathBuf::from(APP_DIR_NAME);
    if ensure_directory(&fallback, "fallback app data") {
        return fallback;
    }

    PathBuf::from(".")
}

pub fn foxy_logs_dir() -> PathBuf {
    let mut logs_dir = foxy_data_dir();
    logs_dir.push("logs");
    if ensure_directory(&logs_dir, "logs") {
        return logs_dir;
    }

    let fallback = PathBuf::from("logs");
    if ensure_directory(&fallback, "fallback logs") {
        return fallback;
    }

    PathBuf::from(".")
}

pub fn foxy_backups_dir() -> PathBuf {
    let mut backups_dir = foxy_data_dir();
    backups_dir.push("backups");
    if ensure_directory(&backups_dir, "backups") {
        return backups_dir;
    }

    let fallback = PathBuf::from("backups");
    if ensure_directory(&fallback, "fallback backups") {
        return fallback;
    }

    PathBuf::from(".")
}

#[allow(dead_code)]
pub fn foxy_patches_dir() -> PathBuf {
    // On Linux, prefer XDG_RUNTIME_DIR (per-user, mode 0700, backed by tmpfs)
    // to avoid the world-writable /tmp directory.
    #[cfg(target_os = "linux")]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let patches_dir = PathBuf::from(runtime_dir).join("foxy").join("patches");
            if ensure_directory(&patches_dir, "patches (XDG_RUNTIME_DIR)") {
                ensure_directory_permissions(&patches_dir);
                return patches_dir;
            }
        }

        // Fallback to user cache directory
        let mut cache_dir = foxy_data_dir();
        cache_dir.push("cache");
        cache_dir.push("patches");
        if ensure_directory(&cache_dir, "patches (cache)") {
            return cache_dir;
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut patches_dir = std::env::temp_dir();
        patches_dir.push(APP_DIR_NAME);
        patches_dir.push("patches");
        if ensure_directory(&patches_dir, "patches") {
            return patches_dir;
        }
    }

    // Final fallback
    let mut fallback = foxy_data_dir();
    fallback.push("patches");
    if ensure_directory(&fallback, "fallback patches") {
        return fallback;
    }

    std::env::temp_dir()
}

pub fn foxy_large_payload_dir() -> PathBuf {
    if let Some(cache_dir) = dirs::cache_dir() {
        let patches_dir = cache_dir.join(APP_DIR_NAME).join("patches");
        if ensure_directory(&patches_dir, "large payload patches") {
            return patches_dir;
        }
    }

    let mut fallback = foxy_data_dir();
    fallback.push("cache");
    fallback.push("patches");
    if ensure_directory(&fallback, "fallback large payload patches") {
        return fallback;
    }

    std::env::temp_dir()
}

/// On Unix, set directory permissions to 0700 (owner-only access).
#[cfg(target_os = "linux")]
fn ensure_directory_permissions(path: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o700);
    if let Err(err) = std::fs::set_permissions(path, perms) {
        log::warn!(
            "Failed to set restrictive permissions on {}: {}",
            path.display(),
            err
        );
    }
}
