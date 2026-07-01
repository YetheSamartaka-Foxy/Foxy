use crate::core::models::repository::FoxyRepository;
use crate::core::tasks::init_database::{DB_WRITE_PERMITS, sqlite_perf_snapshot};
use log::warn;
use std::collections::HashSet;
use std::path::Path;

pub(super) fn mod_task_limit() -> usize {
    let cpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // Keep remote fan-out bounded to SQLite write capacity.
    let lock_retries = sqlite_perf_snapshot().lock_retries;
    let pressure_divisor = if lock_retries >= 64 {
        4
    } else if lock_retries >= 24 {
        2
    } else {
        1
    };
    let suggested = cpu.saturating_mul(2) / pressure_divisor;
    let ceiling = (*DB_WRITE_PERMITS).saturating_mul(6).clamp(6, 16);
    suggested.clamp(4, ceiling)
}

pub(super) fn join_path(base: &str, child: &str) -> String {
    if base.ends_with('/') || base.ends_with('\\') {
        format!("{}{}", base, child)
    } else {
        format!("{}/{}", base, child)
    }
}

pub(crate) fn resolve_mod_local_path(
    repository_local_path: &str,
    repository_space_shared_path: Option<&str>,
    mod_name: &str,
) -> String {
    if let Some(shared_path) = repository_space_shared_path.filter(|path| !path.trim().is_empty()) {
        let shared_mod_path = join_path(shared_path, mod_name);
        if Path::new(&shared_mod_path).is_dir() {
            return shared_mod_path;
        }
    }

    join_path(repository_local_path, mod_name)
}

/// Validate that a mod name is safe to use as a path component.
/// Rejects directory traversal, absolute paths, and dangerous characters.
pub(super) fn validate_mod_name(mod_name: &str) -> bool {
    crate::core::utils::fs_safety::is_safe_child_path(mod_name)
}

pub(super) fn collect_desired_mod_pairs(
    repository_parent: &FoxyRepository,
    repository_space_shared_path: Option<&str>,
    mods_data: &serde_json::Value,
    dedupe: &mut HashSet<String>,
    out: &mut Vec<(String, String)>,
) {
    let Some(mods) = mods_data.as_array() else {
        return;
    };

    for mod_data in mods {
        let mod_name = mod_data
            .get("modName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if mod_name.is_empty() {
            continue;
        }

        if !validate_mod_name(&mod_name) {
            warn!(
                "Skipping mod with unsafe name '{}' from repository {}",
                mod_name, repository_parent.remote_url
            );
            continue;
        }

        let remote_path = join_path(&repository_parent.remote_url, &mod_name);
        let local_path = resolve_mod_local_path(
            &repository_parent.local_path,
            repository_space_shared_path,
            &mod_name,
        );
        let dedupe_key = format!("{}|{}", remote_path, local_path);
        if dedupe.insert(dedupe_key) {
            out.push((remote_path, local_path));
        }
    }
}
