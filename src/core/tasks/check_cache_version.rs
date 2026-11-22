//! Saved repository check results and pending-update plans belong to the
//! Foxy version that produced them.
//!
//! A newer build can check differently from the build that wrote the cache.
//! When the Application setting is on, startup deletes that cache if the
//! recorded version differs, then records the running version as the owner
//! of the cache the new check writes.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::build_info;
use crate::core::game::spaces::{self, APP_SETTINGS_FILE};
use crate::core::utils::app_paths;
use crate::core::utils::fs_safety::atomic_write;

pub const CHECK_CACHE_APP_VERSION_FILE: &str = "check_cache_app_version.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckCacheVersionDecision {
    Keep,
    Invalidate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckCacheAlign {
    Unchanged,
    Invalidated,
}

#[derive(Serialize, Deserialize)]
struct CheckCacheVersionFile {
    app_version: String,
}

/// Whether a stored check-cache version may be reused by `current`.
///
/// An empty or missing record is not the current version: it was written by
/// a build that did not record one. Disabling the setting keeps whatever is
/// stored, including a mismatch.
pub fn decide_check_cache_version(
    stored: Option<&str>,
    current: &str,
    enabled: bool,
) -> CheckCacheVersionDecision {
    if !enabled {
        return CheckCacheVersionDecision::Keep;
    }
    match stored.map(str::trim).filter(|version| !version.is_empty()) {
        Some(stored) if stored == current.trim() => CheckCacheVersionDecision::Keep,
        _ => CheckCacheVersionDecision::Invalidate,
    }
}

pub fn recheck_repositories_after_app_update_enabled() -> bool {
    let path = app_paths::foxy_data_dir().join(APP_SETTINGS_FILE);
    let Ok(raw) = fs::read_to_string(path) else {
        return true;
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| {
            value
                .get("recheck_repositories_after_app_update")
                .and_then(Value::as_bool)
        })
        .unwrap_or(true)
}

pub fn check_cache_app_version_path() -> PathBuf {
    spaces::active_game_space_dir().join(CHECK_CACHE_APP_VERSION_FILE)
}

pub(crate) fn read_check_cache_app_version_at(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<CheckCacheVersionFile>(&raw) {
        Ok(file) => {
            let version = file.app_version.trim();
            (!version.is_empty()).then(|| version.to_string())
        }
        Err(err) => {
            log::warn!(
                "Repository check-cache version file could not be read ({err}); treating it as an older app version"
            );
            None
        }
    }
}

pub(crate) fn write_check_cache_app_version_at(path: &Path, version: &str) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(&CheckCacheVersionFile {
        app_version: version.to_string(),
    })
    .map_err(|_| "could not serialize the check-cache version".to_string())?;
    atomic_write(path, &payload).map_err(|_| "could not write the check-cache version".to_string())
}

/// Clear saved check results in `repositories.json` and record `version` as the
/// cache owner when the database itself has just been wiped.
pub(crate) fn clear_repository_check_results_and_record_owner_at(
    repositories_path: &Path,
    stamp_path: &Path,
    version: &str,
) -> Result<usize, String> {
    let cleared = clear_repository_check_results_file(repositories_path)?;
    write_check_cache_app_version_at(stamp_path, version)?;
    Ok(cleared)
}

/// The database no longer holds check results, so clear stale repository check
/// results too before recording the running version as the owner of the empty cache.
pub fn record_running_app_as_check_cache_owner() {
    let repositories_path = spaces::active_game_space_dir().join("repositories.json");
    if let Err(err) = clear_repository_check_results_and_record_owner_at(
        &repositories_path,
        &check_cache_app_version_path(),
        build_info::VERSION,
    ) {
        log::warn!("Could not record the app version for the repository check cache: {err}");
    }
}

/// Drop saved check-result fields on an in-memory repository so live UI state
/// matches a cleared `repositories.json` and never writes stale results back.
pub fn clear_in_memory_repository_check_results(
    repository: &mut crate::ui::types::Repository,
) -> bool {
    let mut changed = false;
    if !repository.preset_check_evidence.is_empty() {
        repository.preset_check_evidence.clear();
        changed = true;
    }
    if repository.last_check_at.take().is_some() {
        changed = true;
    }
    if repository.last_remote_check_at.take().is_some() {
        changed = true;
    }
    if repository.last_full_verification_at.take().is_some() {
        changed = true;
    }
    if repository.last_check_full {
        repository.last_check_full = false;
        changed = true;
    }
    if !repository.last_check_remote_fresh {
        repository.last_check_remote_fresh = true;
        changed = true;
    }
    changed
}

/// Drop saved check-result fields on repository objects.
///
/// Returns how many repositories changed. Timestamps that were never set stay
/// absent so a file with no check history is left untouched.
pub fn clear_repository_check_results(repositories: &mut Value) -> Result<usize, String> {
    let Some(repos) = repositories.as_array_mut() else {
        return Err("repositories are not a list".to_string());
    };
    let mut cleared = 0usize;
    for repo in repos.iter_mut() {
        let Some(object) = repo.as_object_mut() else {
            continue;
        };
        let mut changed = false;
        if object
            .get("preset_check_evidence")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
        {
            object.insert(
                "preset_check_evidence".to_string(),
                Value::Array(Vec::new()),
            );
            changed = true;
        }
        for key in [
            "last_check_at",
            "last_remote_check_at",
            "last_full_verification_at",
        ] {
            if object.get(key).is_some_and(|value| !value.is_null()) {
                object.insert(key.to_string(), Value::Null);
                changed = true;
            }
        }
        if object.get("last_check_full") == Some(&Value::Bool(true)) {
            object.insert("last_check_full".to_string(), Value::Bool(false));
            changed = true;
        }
        if object.get("last_check_remote_fresh") == Some(&Value::Bool(false)) {
            object.insert("last_check_remote_fresh".to_string(), Value::Bool(true));
            changed = true;
        }
        if changed {
            cleared += 1;
        }
    }
    Ok(cleared)
}

fn clear_repository_check_results_file(path: &Path) -> Result<usize, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(0),
        Err(_) => return Err("could not read repositories".to_string()),
    };
    let mut value = serde_json::from_str::<Value>(&raw)
        .map_err(|_| "could not parse repositories".to_string())?;
    let cleared = clear_repository_check_results(&mut value)?;
    if cleared == 0 {
        return Ok(0);
    }
    let payload = serde_json::to_vec_pretty(&value)
        .map_err(|_| "could not serialize repositories".to_string())?;
    atomic_write(path, &payload).map_err(|_| "could not write repositories".to_string())?;
    Ok(cleared)
}

/// Delete the previous version's check cache when this build should not reuse it.
///
/// The version file is written only after both the repository results and the
/// pending-update rows are gone, so a failed delete is tried again next launch.
pub async fn align_check_cache_with_app_version(enabled: bool) -> Result<CheckCacheAlign, String> {
    let current = build_info::VERSION;
    let stamp_path = check_cache_app_version_path();
    let stored = read_check_cache_app_version_at(&stamp_path);
    if decide_check_cache_version(stored.as_deref(), current, enabled)
        == CheckCacheVersionDecision::Keep
    {
        return Ok(CheckCacheAlign::Unchanged);
    }
    if crate::core::tasks::db_schema_version::current_wipe_requirement().is_some() {
        log::info!(
            "Repository check cache left in place until the database can be used (app version {current})"
        );
        return Ok(CheckCacheAlign::Unchanged);
    }
    crate::core::tasks::db_schema_version::ensure_database_ready_for_updates().await?;

    let repositories_path = spaces::active_game_space_dir().join("repositories.json");
    let cleared_repositories = clear_repository_check_results_file(&repositories_path)?;
    let cleared_pending = crate::core::models::pending_update::clear_all_pending_updates()
        .await
        .map_err(|_| "could not delete saved update plans".to_string())?;
    write_check_cache_app_version_at(&stamp_path, current)?;
    let previous = stored.unwrap_or_else(|| "none".to_string());
    log::info!(
        "Discarded repository check cache after an app update: previous={previous} current={current} pending_rows={cleared_pending} repositories={cleared_repositories}"
    );
    Ok(CheckCacheAlign::Invalidated)
}

#[cfg(test)]
mod tests {
    use super::{
        CheckCacheVersionDecision, clear_repository_check_results, decide_check_cache_version,
        read_check_cache_app_version_at, write_check_cache_app_version_at,
    };
    use serde_json::json;

    #[test]
    fn matching_version_keeps_the_cache_and_a_difference_discards_it() {
        assert_eq!(
            decide_check_cache_version(Some("1.2.3"), "1.2.3", true),
            CheckCacheVersionDecision::Keep
        );
        assert_eq!(
            decide_check_cache_version(Some(" 1.2.3 "), "1.2.3", true),
            CheckCacheVersionDecision::Keep
        );
        assert_eq!(
            decide_check_cache_version(Some("1.2.2"), "1.2.3", true),
            CheckCacheVersionDecision::Invalidate
        );
        assert_eq!(
            decide_check_cache_version(None, "1.2.3", true),
            CheckCacheVersionDecision::Invalidate
        );
        assert_eq!(
            decide_check_cache_version(Some("  "), "1.2.3", true),
            CheckCacheVersionDecision::Invalidate
        );
    }

    #[test]
    fn disabling_the_setting_keeps_a_cache_from_another_version() {
        assert_eq!(
            decide_check_cache_version(Some("1.2.2"), "1.2.3", false),
            CheckCacheVersionDecision::Keep
        );
        assert_eq!(
            decide_check_cache_version(None, "1.2.3", false),
            CheckCacheVersionDecision::Keep
        );
    }

    #[test]
    fn clearing_check_results_drops_verdicts_and_leaves_unrelated_fields() {
        let mut repositories = json!([
            {
                "name": "Main",
                "address": "https://repo.example.invalid/main/",
                "preset_check_evidence": [{
                    "profile": null,
                    "selected_addons": ["@mod_01"],
                    "outcome": "Synced",
                    "checked_at": 10
                }],
                "last_check_at": 10,
                "last_remote_check_at": 9,
                "last_full_verification_at": 8,
                "last_check_full": true,
                "last_check_remote_fresh": false,
                "last_updated_at": 7,
                "extra": "kept"
            },
            {
                "name": "Quiet",
                "address": "https://repo.example.invalid/quiet/"
            }
        ]);

        assert_eq!(
            clear_repository_check_results(&mut repositories).unwrap(),
            1
        );
        assert_eq!(
            clear_repository_check_results(&mut repositories).unwrap(),
            0
        );
        let checked = &repositories[0];
        assert_eq!(checked["preset_check_evidence"], json!([]));
        assert!(checked["last_check_at"].is_null());
        assert!(checked["last_remote_check_at"].is_null());
        assert!(checked["last_full_verification_at"].is_null());
        assert_eq!(checked["last_check_full"], json!(false));
        assert_eq!(checked["last_check_remote_fresh"], json!(true));
        assert_eq!(checked["last_updated_at"], json!(7));
        assert_eq!(checked["extra"], json!("kept"));
        assert!(repositories[1].get("last_check_at").is_none());
    }

    #[test]
    fn cleared_check_results_still_load_as_repositories() {
        use crate::ui::types::{
            Repository, RepositoryPresetCheckEvidence, RepositoryPresetCheckOutcome,
        };

        let repository = Repository {
            name: "Main".to_string(),
            address: "https://repo.example.invalid/main/".to_string(),
            path: "R:/Mods/Main".to_string(),
            preset_check_evidence: vec![RepositoryPresetCheckEvidence {
                profile: None,
                selected_addons: vec!["@mod_01".to_string()],
                outcome: RepositoryPresetCheckOutcome::Synced,
                checked_at: 10,
            }],
            last_check_at: Some(10),
            last_remote_check_at: Some(9),
            last_full_verification_at: Some(8),
            last_check_full: true,
            last_check_remote_fresh: false,
            last_updated_at: Some(7),
            ..Repository::default()
        };

        let mut value = serde_json::to_value(vec![repository]).unwrap();
        assert_eq!(clear_repository_check_results(&mut value).unwrap(), 1);
        let restored: Vec<Repository> = serde_json::from_value(value).unwrap();
        assert!(restored[0].preset_check_evidence.is_empty());
        assert!(restored[0].last_check_at.is_none());
        assert!(restored[0].last_remote_check_at.is_none());
        assert!(restored[0].last_full_verification_at.is_none());
        assert!(!restored[0].last_check_full);
        assert!(restored[0].last_check_remote_fresh);
        assert_eq!(restored[0].last_updated_at, Some(7));
    }

    #[test]
    fn version_file_round_trips_and_blank_versions_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("check_cache_app_version.json");
        assert!(read_check_cache_app_version_at(&path).is_none());
        write_check_cache_app_version_at(&path, "1.2.3").unwrap();
        assert_eq!(
            read_check_cache_app_version_at(&path).as_deref(),
            Some("1.2.3")
        );
        std::fs::write(&path, r#"{"app_version":"   "}"#).unwrap();
        assert!(read_check_cache_app_version_at(&path).is_none());
    }

    #[test]
    fn recording_owner_after_wipe_clears_stale_repository_check_results_first() {
        let dir = tempfile::tempdir().unwrap();
        let repos_path = dir.path().join("repositories.json");
        let stamp_path = dir.path().join("check_cache_app_version.json");
        std::fs::write(
            &repos_path,
            serde_json::to_string_pretty(&json!([{
                "name": "Main",
                "address": "https://repo.example.invalid/main/",
                "last_check_at": 10,
                "last_remote_check_at": 9,
                "last_check_full": true,
                "last_check_remote_fresh": false,
                "preset_check_evidence": [{
                    "profile": null,
                    "selected_addons": ["@mod_01"],
                    "outcome": "Synced",
                    "checked_at": 10
                }]
            }]))
            .unwrap(),
        )
        .unwrap();

        let cleared = super::clear_repository_check_results_and_record_owner_at(
            &repos_path,
            &stamp_path,
            "1.2.3",
        )
        .unwrap();
        assert_eq!(cleared, 1);
        assert_eq!(
            read_check_cache_app_version_at(&stamp_path).as_deref(),
            Some("1.2.3")
        );
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&repos_path).unwrap()).unwrap();
        assert_eq!(saved[0]["preset_check_evidence"], json!([]));
        assert!(saved[0]["last_check_at"].is_null());
        assert!(saved[0]["last_remote_check_at"].is_null());
    }

    #[test]
    fn in_memory_clear_matches_persisted_clear() {
        use crate::ui::types::{
            Repository, RepositoryPresetCheckEvidence, RepositoryPresetCheckOutcome,
        };

        let mut repository = Repository {
            name: "Main".to_string(),
            address: "https://repo.example.invalid/main/".to_string(),
            path: "R:/Mods/Main".to_string(),
            preset_check_evidence: vec![RepositoryPresetCheckEvidence {
                profile: None,
                selected_addons: vec!["@mod_01".to_string()],
                outcome: RepositoryPresetCheckOutcome::Synced,
                checked_at: 10,
            }],
            last_check_at: Some(10),
            last_remote_check_at: Some(9),
            last_full_verification_at: Some(8),
            last_check_full: true,
            last_check_remote_fresh: false,
            last_updated_at: Some(7),
            ..Repository::default()
        };

        assert!(super::clear_in_memory_repository_check_results(
            &mut repository
        ));
        assert!(!super::clear_in_memory_repository_check_results(
            &mut repository
        ));
        assert!(repository.preset_check_evidence.is_empty());
        assert!(repository.last_check_at.is_none());
        assert!(repository.last_remote_check_at.is_none());
        assert!(repository.last_full_verification_at.is_none());
        assert!(!repository.last_check_full);
        assert!(repository.last_check_remote_fresh);
        assert_eq!(repository.last_updated_at, Some(7));
    }
}
