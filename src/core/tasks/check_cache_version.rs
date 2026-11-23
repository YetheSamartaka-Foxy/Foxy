//! Per-space check-cache ownership keyed by running build identity (version, commit, build kind, binary BLAKE3).

use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckCacheAppIdentity {
    pub app_version: String,
    #[serde(default)]
    pub git_hash: String,
    #[serde(default)]
    pub build_kind: String,
    #[serde(default)]
    pub exe_blake3: String,
}

impl CheckCacheAppIdentity {
    pub fn summary(&self) -> String {
        let version = self.app_version.trim();
        let git_hash = self.git_hash.trim();
        let build_kind = self.build_kind.trim();
        let exe_hash = self.exe_blake3.trim();
        if git_hash.is_empty() && build_kind.is_empty() && exe_hash.is_empty() {
            return format!("{version} (version-only)");
        }
        let short_hash = if exe_hash.len() > 12 {
            &exe_hash[..12]
        } else if exe_hash.is_empty() {
            "unavailable"
        } else {
            exe_hash
        };
        format!(
            "{version} ({}, {}, blake3={short_hash})",
            if git_hash.is_empty() { "?" } else { git_hash },
            if build_kind.is_empty() {
                "?"
            } else {
                build_kind
            },
        )
    }
}

fn hash_file_blake3(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|err| format!("could not open executable ({err})"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| format!("could not read executable ({err})"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn current_exe_blake3() -> &'static str {
    // Process-wide binary fingerprint (never space-derived).
    static EXE_BLAKE3: OnceLock<String> = OnceLock::new();
    EXE_BLAKE3.get_or_init(|| match std::env::current_exe() {
        Ok(path) => match hash_file_blake3(&path) {
            Ok(hash) => hash,
            Err(err) => {
                log::warn!(
                    "Could not hash the running executable for the repository check cache: {err}"
                );
                String::new()
            }
        },
        Err(err) => {
            log::warn!(
                "Could not locate the running executable for the repository check cache: {err}"
            );
            String::new()
        }
    })
}

pub fn running_app_identity() -> CheckCacheAppIdentity {
    CheckCacheAppIdentity {
        app_version: build_info::VERSION.to_string(),
        git_hash: build_info::GIT_HASH.to_string(),
        build_kind: build_info::build_kind().to_string(),
        exe_blake3: current_exe_blake3().to_string(),
    }
}

async fn running_app_identity_async() -> CheckCacheAppIdentity {
    tokio::task::spawn_blocking(running_app_identity)
        .await
        .unwrap_or_else(|_| running_app_identity())
}

fn non_empty_trimmed_eq(left: &str, right: &str) -> bool {
    let left = left.trim();
    let right = right.trim();
    !left.is_empty() && left == right
}

/// Missing, empty, or legacy version-only stamps invalidate when enabled; disabling keeps any stamp.
pub fn decide_check_cache_version(
    stored: Option<&CheckCacheAppIdentity>,
    current: &CheckCacheAppIdentity,
    enabled: bool,
) -> CheckCacheVersionDecision {
    if !enabled {
        return CheckCacheVersionDecision::Keep;
    }
    let Some(stored) = stored else {
        return CheckCacheVersionDecision::Invalidate;
    };
    let metadata_matches = non_empty_trimmed_eq(&stored.app_version, &current.app_version)
        && non_empty_trimmed_eq(&stored.git_hash, &current.git_hash)
        && non_empty_trimmed_eq(&stored.build_kind, &current.build_kind);
    if !metadata_matches {
        return CheckCacheVersionDecision::Invalidate;
    }
    let current_hash = current.exe_blake3.trim();
    if current_hash.is_empty() {
        return CheckCacheVersionDecision::Keep;
    }
    let stored_hash = stored.exe_blake3.trim();
    if !stored_hash.is_empty() && stored_hash.eq_ignore_ascii_case(current_hash) {
        CheckCacheVersionDecision::Keep
    } else {
        CheckCacheVersionDecision::Invalidate
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

pub(crate) fn read_check_cache_app_version_at(path: &Path) -> Option<CheckCacheAppIdentity> {
    let raw = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<CheckCacheAppIdentity>(&raw) {
        Ok(file) => {
            let version = file.app_version.trim();
            if version.is_empty() {
                return None;
            }
            Some(CheckCacheAppIdentity {
                app_version: version.to_string(),
                git_hash: file.git_hash.trim().to_string(),
                build_kind: file.build_kind.trim().to_string(),
                exe_blake3: file.exe_blake3.trim().to_string(),
            })
        }
        Err(err) => {
            log::warn!(
                "Repository check-cache version file could not be read ({err}); treating it as an older app build"
            );
            None
        }
    }
}

pub(crate) fn write_check_cache_app_version_at(
    path: &Path,
    identity: &CheckCacheAppIdentity,
) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(identity)
        .map_err(|_| "could not serialize the check-cache version".to_string())?;
    atomic_write(path, &payload).map_err(|_| "could not write the check-cache version".to_string())
}

pub(crate) fn clear_repository_check_results_and_record_owner_at(
    repositories_path: &Path,
    stamp_path: &Path,
    identity: &CheckCacheAppIdentity,
) -> Result<usize, String> {
    let cleared = clear_repository_check_results_file(repositories_path)?;
    write_check_cache_app_version_at(stamp_path, identity)?;
    Ok(cleared)
}

pub fn record_running_app_as_check_cache_owner() {
    let repositories_path = spaces::active_game_space_dir().join("repositories.json");
    let identity = running_app_identity();
    if let Err(err) = clear_repository_check_results_and_record_owner_at(
        &repositories_path,
        &check_cache_app_version_path(),
        &identity,
    ) {
        log::warn!("Could not record the app build for the repository check cache: {err}");
    }
}

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

/// Returns how many repositories changed, leaving unset timestamps absent so clean files are untouched.
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

/// Writes the build stamp only after both repository results and pending updates are cleared so failed deletes retry.
pub async fn align_check_cache_with_app_version(enabled: bool) -> Result<CheckCacheAlign, String> {
    let current = running_app_identity_async().await;
    let stamp_path = check_cache_app_version_path();
    let stored = read_check_cache_app_version_at(&stamp_path);
    if decide_check_cache_version(stored.as_ref(), &current, enabled)
        == CheckCacheVersionDecision::Keep
    {
        return Ok(CheckCacheAlign::Unchanged);
    }
    if crate::core::tasks::db_schema_version::current_wipe_requirement().is_some() {
        log::info!(
            "Repository check cache left in place until the database can be used (app build {})",
            current.summary()
        );
        return Ok(CheckCacheAlign::Unchanged);
    }
    crate::core::tasks::db_schema_version::ensure_database_ready_for_updates().await?;

    let repositories_path = spaces::active_game_space_dir().join("repositories.json");
    let cleared_repositories = clear_repository_check_results_file(&repositories_path)?;
    let cleared_pending = crate::core::models::pending_update::clear_all_pending_updates()
        .await
        .map_err(|_| "could not delete saved update plans".to_string())?;
    write_check_cache_app_version_at(&stamp_path, &current)?;
    let previous = stored
        .as_ref()
        .map(CheckCacheAppIdentity::summary)
        .unwrap_or_else(|| "none".to_string());
    log::info!(
        "Discarded repository check cache after an app update: previous={} current={} pending_rows={} repositories={}",
        previous,
        current.summary(),
        cleared_pending,
        cleared_repositories
    );
    Ok(CheckCacheAlign::Invalidated)
}

#[cfg(test)]
mod tests {
    use super::{
        CheckCacheAppIdentity, CheckCacheVersionDecision, clear_repository_check_results,
        decide_check_cache_version, hash_file_blake3, read_check_cache_app_version_at,
        running_app_identity, write_check_cache_app_version_at,
    };
    use serde_json::json;

    fn identity(
        app_version: &str,
        git_hash: &str,
        build_kind: &str,
        exe_blake3: &str,
    ) -> CheckCacheAppIdentity {
        CheckCacheAppIdentity {
            app_version: app_version.to_string(),
            git_hash: git_hash.to_string(),
            build_kind: build_kind.to_string(),
            exe_blake3: exe_blake3.to_string(),
        }
    }

    #[test]
    fn matching_identity_keeps_the_cache_and_any_field_difference_discards_it() {
        let current = identity("1.2.3", "bb5d144", "release", "abcdef0123456789");
        assert_eq!(
            decide_check_cache_version(Some(&current), &current, true),
            CheckCacheVersionDecision::Keep
        );
        assert_eq!(
            decide_check_cache_version(
                Some(&identity(
                    " 1.2.3 ",
                    " bb5d144 ",
                    " release ",
                    " ABCDEF0123456789 "
                )),
                &current,
                true
            ),
            CheckCacheVersionDecision::Keep
        );
        assert_eq!(
            decide_check_cache_version(
                Some(&identity("1.2.2", "bb5d144", "release", "abcdef0123456789")),
                &current,
                true
            ),
            CheckCacheVersionDecision::Invalidate
        );
        assert_eq!(
            decide_check_cache_version(
                Some(&identity("1.2.3", "c9f31b5", "release", "abcdef0123456789")),
                &current,
                true
            ),
            CheckCacheVersionDecision::Invalidate
        );
        assert_eq!(
            decide_check_cache_version(
                Some(&identity(
                    "1.2.3",
                    "bb5d144",
                    "prerelease",
                    "abcdef0123456789"
                )),
                &current,
                true
            ),
            CheckCacheVersionDecision::Invalidate
        );
        assert_eq!(
            decide_check_cache_version(
                Some(&identity("1.2.3", "bb5d144", "release", "0000000000000000")),
                &current,
                true
            ),
            CheckCacheVersionDecision::Invalidate
        );
        // Legacy version-only stamp from an earlier build invalidates once.
        assert_eq!(
            decide_check_cache_version(Some(&identity("1.2.3", "", "", "")), &current, true),
            CheckCacheVersionDecision::Invalidate
        );
        assert_eq!(
            decide_check_cache_version(None, &current, true),
            CheckCacheVersionDecision::Invalidate
        );
        assert_eq!(
            decide_check_cache_version(
                Some(&identity("  ", "bb5d144", "release", "abcdef0123456789")),
                &current,
                true
            ),
            CheckCacheVersionDecision::Invalidate
        );
    }

    #[test]
    fn unavailable_executable_hash_falls_back_to_build_metadata() {
        let current = identity("1.2.3", "bb5d144", "release", "");
        assert_eq!(
            decide_check_cache_version(
                Some(&identity("1.2.3", "bb5d144", "release", "")),
                &current,
                true
            ),
            CheckCacheVersionDecision::Keep
        );
        assert_eq!(
            decide_check_cache_version(
                Some(&identity("1.2.3", "c9f31b5", "release", "")),
                &current,
                true
            ),
            CheckCacheVersionDecision::Invalidate
        );
    }

    #[test]
    fn disabling_the_setting_keeps_a_cache_from_another_build() {
        let current = identity("1.2.3", "bb5d144", "release", "abcdef0123456789");
        assert_eq!(
            decide_check_cache_version(
                Some(&identity(
                    "1.2.2",
                    "c9f31b5",
                    "prerelease",
                    "1111111111111111"
                )),
                &current,
                false
            ),
            CheckCacheVersionDecision::Keep
        );
        assert_eq!(
            decide_check_cache_version(None, &current, false),
            CheckCacheVersionDecision::Keep
        );
    }

    #[test]
    fn running_app_identity_includes_all_four_fields() {
        let current = running_app_identity();
        assert!(!current.app_version.trim().is_empty());
        assert!(!current.git_hash.trim().is_empty());
        assert!(!current.build_kind.trim().is_empty());
        assert_eq!(current.exe_blake3.len(), 64);
    }

    #[test]
    fn hash_file_blake3_matches_blake3_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.bin");
        let bytes = b"foxy-binary-sample";
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            hash_file_blake3(&path).unwrap(),
            blake3::hash(bytes).to_hex().to_string()
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
    fn version_file_round_trips_and_reads_legacy_version_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("check_cache_app_version.json");
        assert!(read_check_cache_app_version_at(&path).is_none());
        let expected = identity("1.2.3", "bb5d144", "release", "abcdef0123456789");
        write_check_cache_app_version_at(&path, &expected).unwrap();
        assert_eq!(read_check_cache_app_version_at(&path), Some(expected));

        std::fs::write(&path, r#"{"app_version":"1.2.3"}"#).unwrap();
        assert_eq!(
            read_check_cache_app_version_at(&path),
            Some(identity("1.2.3", "", "", ""))
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

        let expected = identity("1.2.3", "bb5d144", "release", "abcdef0123456789");
        let cleared = super::clear_repository_check_results_and_record_owner_at(
            &repos_path,
            &stamp_path,
            &expected,
        )
        .unwrap();
        assert_eq!(cleared, 1);
        assert_eq!(read_check_cache_app_version_at(&stamp_path), Some(expected));
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
