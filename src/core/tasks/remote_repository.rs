use crate::core::db::params;
use crate::core::models::context::FoxyContext;
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::models::repository::{
    FoxyMode, FoxyRepository, load_repository_by_remote_url_and_local_path, upsert_repository_entry,
};
use crate::core::tasks::calculate_hashes::{
    finalize_repository_content_hashes_from_mods, finalize_repository_hashes_from_mods,
    pre_propagate_sibling_checksums,
};
use crate::core::tasks::init_database::{DB_WRITE_PERMITS, DB_WRITE_SEMAPHORE};
use crate::core::tasks::remote_mods::{remote_mods_with_data, resolve_mod_local_path};
use crate::core::utils::fetch_json::fetch_json;
use crate::ui::types::HashAlgorithmPreference;
use log::{debug, error, info, warn};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteRepositoryManifest {
    #[serde(default)]
    repo_name: String,
    #[serde(default = "default_repo_image_path")]
    repo_image_path: String,
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    foxy_mode: String,
    #[serde(default)]
    app_update_url: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteRepositoryMetadata {
    pub foxy_mode: FoxyMode,
    pub app_update_url: Option<String>,
    /// `true` when the remote refresh was skipped because checksums already matched.
    pub skipped: bool,
    /// `true` when the repository has linked addons, remote state (mods/files/parts),
    /// and non-empty checksums - meaning the tree is fully initialized and does not
    /// need a bootstrap hash pass.
    pub repository_complete: bool,
    /// `true` when linked addons and remote file/part metadata exist in the DB.
    pub remote_graph_complete: bool,
    /// `true` when remote addon/file/part manifests were fetched during this call.
    pub remote_graph_fetched: bool,
}

fn default_repo_image_path() -> String {
    "repo.png".to_string()
}

fn normalize_optional_url(url: Option<&str>) -> Option<String> {
    url.map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(str::to_string)
}

fn repository_refresh_complete(
    repository: &FoxyRepository,
    has_linked_addons: bool,
    has_remote_state: bool,
) -> bool {
    has_linked_addons
        && has_remote_state
        && !repository.local_checksum.trim().is_empty()
        && !repository.local_content_hash.trim().is_empty()
}

fn repository_remote_graph_complete(
    repository: &FoxyRepository,
    has_linked_addons: bool,
    has_remote_state: bool,
) -> bool {
    has_linked_addons && has_remote_state && !repository.remote_checksum.trim().is_empty()
}

fn checksum_eq(left: &str, right: &str) -> bool {
    let left = left.trim();
    !left.is_empty() && left == right.trim()
}

fn local_path_identity_unchanged(existing: &FoxyRepository, normalized_local_path: &str) -> bool {
    normalized_local_path.is_empty()
        || normalize_local_path_identity(&existing.local_path)
            == normalize_local_path_identity(normalized_local_path)
}

fn normalize_local_path_identity(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path.trim())
}

fn should_skip_clean_remote_refresh(
    repository: &FoxyRepository,
    has_linked_addons: bool,
    has_remote_state: bool,
    recheck_level: RecheckLevel,
    force_refresh: bool,
) -> bool {
    let repository_complete =
        repository_refresh_complete(repository, has_linked_addons, has_remote_state);
    checksum_eq(&repository.remote_checksum, &repository.local_checksum)
        && recheck_level < RecheckLevel::REPOSITORY
        && repository_complete
        && !force_refresh
}

fn should_skip_unchanged_remote_graph(
    remote_repo_checksum: &str,
    previous_remote_checksum: Option<&str>,
    local_path_unchanged: bool,
    graph_complete: bool,
    recheck_level: RecheckLevel,
    force_refresh: bool,
) -> bool {
    previous_remote_checksum.is_some_and(|stored| checksum_eq(remote_repo_checksum, stored))
        && local_path_unchanged
        && graph_complete
        && recheck_level < RecheckLevel::REPOSITORY
        && !force_refresh
}

#[derive(Debug, Clone)]
struct RepositoryAddonRemoteState {
    name: String,
    enabled: bool,
    remote_checksum: String,
    file_count: i64,
    missing_file_remote_checksums: i64,
    part_count: i64,
    missing_part_remote_checksums: i64,
}

fn addon_enabled_for_remote_state(
    addon: &RepositoryAddonRemoteState,
    enabled_overrides: Option<&HashMap<String, bool>>,
) -> bool {
    enabled_overrides
        .and_then(|overrides| overrides.get(&addon.name.to_lowercase()).copied())
        .unwrap_or(addon.enabled)
}

fn remote_state_complete_from_addons(
    addons: &[RepositoryAddonRemoteState],
    enabled_overrides: Option<&HashMap<String, bool>>,
) -> bool {
    if addons.is_empty() {
        return false;
    }

    for addon in addons {
        if !addon_enabled_for_remote_state(addon, enabled_overrides) {
            continue;
        }

        if addon.remote_checksum.trim().is_empty()
            || addon.file_count == 0
            || addon.missing_file_remote_checksums > 0
            || (addon.part_count > 0 && addon.missing_part_remote_checksums > 0)
        {
            return false;
        }
    }

    true
}

async fn repository_has_linked_addons(context: Arc<FoxyContext>, repository_id: i64) -> bool {
    match context
        .db()
        .query_one(
            "SELECT 1 FROM repository_addons WHERE repository_id = ? LIMIT 1",
            params![repository_id],
        )
        .await
    {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(err) => {
            warn!(
                "Failed to check existing repository links for repository_id={}: {}",
                repository_id, err
            );
            false
        }
    }
}

fn collect_remote_addon_names(data: &Value) -> Option<HashSet<String>> {
    let mut names = HashSet::new();
    let mut saw_mod_list = false;

    for key in ["requiredMods", "optionalMods"] {
        let Some(mods) = data.get(key).and_then(Value::as_array) else {
            continue;
        };
        saw_mod_list = true;
        for mod_data in mods {
            let Some(mod_name) = mod_data.get("modName").and_then(Value::as_str) else {
                continue;
            };
            let mod_name = mod_name.trim();
            if mod_name.is_empty() || !crate::core::utils::fs_safety::is_safe_child_path(mod_name) {
                continue;
            }
            names.insert(mod_name.to_lowercase());
        }
    }

    saw_mod_list.then_some(names)
}

async fn repository_linked_addon_names(
    context: Arc<FoxyContext>,
    repository_id: i64,
) -> Option<HashSet<String>> {
    let rows = match context
        .db()
        .query_all(
            r#"SELECT a.name AS name
               FROM repository_addons ra
               JOIN addons a ON a.id = ra.addon_id
               WHERE ra.repository_id = ?"#,
            params![repository_id],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            warn!(
                "Failed to load linked addon names for repository_id={}: {}",
                repository_id, err
            );
            return None;
        }
    };

    Some(
        rows.into_iter()
            .filter_map(|row| row.get_string("name").ok())
            .map(|name| name.trim().to_lowercase())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

async fn repository_addon_paths_match_space_layout(
    context: Arc<FoxyContext>,
    repository: &FoxyRepository,
) -> bool {
    let Some(shared_path) = context.repository_space_shared_path.as_deref() else {
        return true;
    };

    let rows = match context
        .db()
        .query_all(
            r#"SELECT a.name AS name, a.local_path AS local_path
               FROM repository_addons ra
               JOIN addons a ON a.id = ra.addon_id
               WHERE ra.repository_id = ?"#,
            params![repository.id as i64],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            warn!(
                "Failed to validate repository-space addon paths for repository_id={}: {}",
                repository.id, err
            );
            return false;
        }
    };

    rows.into_iter().all(|row| {
        let Ok(name) = row.get_string("name") else {
            return false;
        };
        let Ok(local_path) = row.get_string("local_path") else {
            return false;
        };
        let expected =
            resolve_mod_local_path(&repository.local_path, Some(shared_path), name.trim());
        crate::core::utils::content_hash::normalize_path(&local_path)
            == crate::core::utils::content_hash::normalize_path(&expected)
    })
}

async fn repository_has_remote_state(
    context: Arc<FoxyContext>,
    repository_id: i64,
    enabled_overrides: Option<&HashMap<String, bool>>,
) -> bool {
    // File-level grouped stats only - deliberately NOT joined to `subfiles`. The
    // old query LEFT JOINed subfiles, which multiplied the row set to one per
    // part (tens of thousands) before GROUP BY + COUNT(DISTINCT), and that
    // explosion was the dominant cost of every clean remote recheck on Turso.
    //
    // Part completeness is derived instead of scanned: a file's remote_checksum
    // is rolled up from its parts' remote checksums during the metadata rebuild,
    // so a file with a non-empty remote_checksum implies its parts already carry
    // theirs (mirrors the local rollup ordering in `calculate_hashes`). With all
    // files complete, the parts are complete too, so the part term in
    // `remote_state_complete_from_addons` is satisfied with part_count = 0. If a
    // partial graph ever slipped through, the repository-level checksum equality
    // that gates the skip decision still catches it.
    let rows = match context
        .db()
        .query_all(
            r#"SELECT
                   a.name AS name,
                   a.enabled AS enabled,
                   a.remote_checksum AS remote_checksum,
                   COUNT(DISTINCT f.id) AS file_count,
                   SUM(CASE WHEN f.id IS NOT NULL AND f.remote_checksum = '' THEN 1 ELSE 0 END)
                       AS missing_file_remote_checksums
               FROM repository_addons ra
               JOIN addons a ON a.id = ra.addon_id
               LEFT JOIN addon_files af ON af.addon_id = a.id
               LEFT JOIN files f ON f.id = af.file_id
               WHERE ra.repository_id = ?
               GROUP BY a.id, a.name, a.enabled, a.remote_checksum"#,
            params![repository_id],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            warn!(
                "Failed to check repository remote-state completeness for repository_id={}: {}",
                repository_id, err
            );
            return false;
        }
    };

    let states = rows
        .into_iter()
        .map(|row| RepositoryAddonRemoteState {
            name: row.get_string("name").unwrap_or_default(),
            enabled: row.get_bool("enabled").unwrap_or(false),
            remote_checksum: row.get_string("remote_checksum").unwrap_or_default(),
            file_count: row.get_i64("file_count").unwrap_or(0),
            missing_file_remote_checksums: row
                .get_i64("missing_file_remote_checksums")
                .unwrap_or(0),
            // Derived from file completeness (see query comment) - not scanned.
            part_count: 0,
            missing_part_remote_checksums: 0,
        })
        .collect::<Vec<_>>();

    remote_state_complete_from_addons(&states, enabled_overrides)
}

/// Cheaply fetch the repository's canonical remote checksum without touching the
/// DB or rebuilding any metadata graph. Mirrors how [`remote_repository`] derives
/// the stored `remote_checksum`: it reads `repo.json`, and for FoxyMode repos
/// prefers the deterministic BLAKE3 checksum published in `foxy_addons.json`.
///
/// Used as a fast freshness gate before reusing a confirmation-prepared download
/// queue: if this checksum still equals the stored `remote_checksum`, the remote
/// repository has not changed since the queue was built.
pub(crate) async fn probe_remote_repository_checksum(
    context: Arc<FoxyContext>,
    repository_url: &str,
    hash_algorithm_preference: HashAlgorithmPreference,
) -> Option<String> {
    let normalized_url = if repository_url.ends_with('/') {
        repository_url.to_string()
    } else {
        format!("{}/", repository_url)
    };
    let repo_url = format!("{}repo.json", normalized_url);

    let data = match fetch_json(context.clone(), &repo_url).await {
        Ok(d) => d,
        Err(e) => {
            warn!("Remote checksum probe failed to fetch {} : {}", repo_url, e);
            return None;
        }
    };

    let checksum = data
        .get("checksum")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let remote_foxy_mode = FoxyMode::from_db_str(
        data.get("foxyMode")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let foxy_mode = if hash_algorithm_preference == HashAlgorithmPreference::PreferSwifty
        && remote_foxy_mode.is_foxy()
    {
        FoxyMode::None
    } else {
        remote_foxy_mode
    };

    if foxy_mode.is_foxy() {
        let foxy_addons_url = format!("{}foxy_addons.json", normalized_url);
        if let Ok(foxy_data) = fetch_json(context.clone(), &foxy_addons_url).await
            && let Some(foxy_checksum) = foxy_data
                .get("checksum")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty())
        {
            return Some(foxy_checksum.to_string());
        }
    }

    if checksum.is_empty() {
        None
    } else {
        Some(checksum)
    }
}

/// Acquire repository information from URL, process addons if local and remote repository checksums differ
pub(crate) async fn remote_repository(
    context: Arc<FoxyContext>,
    repository_url: &str,
    local_path_override: Option<&str>,
    enabled_overrides: Option<&std::collections::HashMap<String, bool>>,
    force_refresh: bool,
    hash_algorithm_preference: HashAlgorithmPreference,
) -> Option<RemoteRepositoryMetadata> {
    // Normalize remote URL to always have trailing slash for consistent path joins
    let normalized_url = if repository_url.ends_with('/') {
        repository_url.to_string()
    } else {
        format!("{}/", repository_url)
    };
    let repo_url = format!("{}repo.json", normalized_url);
    info!("Loading repository metadata from: {}", repo_url);

    let data = match fetch_json(context.clone(), &repo_url).await {
        Ok(d) => d,
        Err(e) => {
            error!(
                "Error fetching addons for repository {}: {}",
                repository_url, e
            );
            return None;
        }
    };

    let manifest =
        serde_json::from_value::<RemoteRepositoryManifest>(data.clone()).unwrap_or_else(|err| {
            warn!(
                "Failed to deserialize repo.json metadata for {}: {}",
                repository_url, err
            );
            RemoteRepositoryManifest {
                repo_name: data
                    .get("repoName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                repo_image_path: data
                    .get("repoImagePath")
                    .and_then(Value::as_str)
                    .unwrap_or("repo.png")
                    .to_string(),
                checksum: data
                    .get("checksum")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                foxy_mode: data
                    .get("foxyMode")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                app_update_url: data
                    .get("appUpdateUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }
        });

    let app_update_url = normalize_optional_url(manifest.app_update_url.as_deref());
    let name = manifest.repo_name;
    let image = manifest.repo_image_path;
    let mut remote_checksum = manifest.checksum;
    let remote_foxy_mode = FoxyMode::from_db_str(&manifest.foxy_mode);
    let foxy_mode = if hash_algorithm_preference == HashAlgorithmPreference::PreferSwifty
        && remote_foxy_mode.is_foxy()
    {
        info!(
            "Repository {} supports {:?} but user prefers Swifty (MD5), overriding to legacy mode",
            repository_url, remote_foxy_mode
        );
        FoxyMode::None
    } else {
        remote_foxy_mode
    };
    if foxy_mode.is_foxy() {
        info!("Repository {} uses {:?}", repository_url, foxy_mode);
    }

    // For FoxyMode repos, the deterministic BLAKE3 repository checksum is
    // published in foxy_addons.json. The repo.json `checksum` may be a non-deterministic,
    // algorithm-mismatched SHA-1 (Hybrid/Swifty-generated repos seed it with a timestamp),
    // which can never equal the client's BLAKE3 rollup and causes a perpetual false
    // "remote changed" / redownload loop. Fetch foxy_addons.json once here and prefer its
    // top-level `checksum` so the repository-level comparison is BLAKE3-vs-BLAKE3 and
    // stable. The fetched payload is reused below as the mod metadata source.
    let foxy_addons_data = if foxy_mode.is_foxy() {
        let foxy_addons_url = format!("{}foxy_addons.json", normalized_url);
        info!(
            "FoxyMode detected - loading mod metadata from: {}",
            foxy_addons_url
        );
        match fetch_json(context.clone(), &foxy_addons_url).await {
            Ok(foxy_data) => {
                if let Some(foxy_checksum) = foxy_data
                    .get("checksum")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|candidate| !candidate.is_empty())
                {
                    if !foxy_checksum.eq_ignore_ascii_case(remote_checksum.trim()) {
                        debug!(
                            "Repository {} using foxy_addons.json checksum as remote checksum (repo.json checksum differs)",
                            repository_url
                        );
                    }
                    remote_checksum = foxy_checksum.to_string();
                }
                Some(foxy_data)
            }
            Err(e) => {
                warn!(
                    "Failed to fetch foxy_addons.json for {}, falling back to repo.json: {}",
                    repository_url, e
                );
                None
            }
        }
    } else {
        None
    };
    if let Some(update_url) = &app_update_url {
        debug!(
            "Repository {} provides app update URL: {}",
            repository_url, update_url
        );
    }

    // Save repository
    // Normalize local path (ensure trailing slash, use forward slashes)
    let normalized_local_path = local_path_override
        .map(|p| {
            let mut s = p.replace('\\', "/");
            if !s.ends_with('/') {
                s.push('/');
            }
            s
        })
        .unwrap_or_default();

    let existing_repository = load_repository_by_remote_url_and_local_path(
        context.clone(),
        &normalized_url,
        &normalized_local_path,
    )
    .await
    .ok();
    let previous_remote_checksum = existing_repository
        .as_ref()
        .map(|repo| repo.remote_checksum.clone());
    let local_path_unchanged = existing_repository
        .as_ref()
        .map(|repo| local_path_identity_unchanged(repo, &normalized_local_path))
        .unwrap_or(true);

    let mut repository = match upsert_repository_entry(
        context.clone(),
        &normalized_url,
        &name,
        &image,
        &remote_checksum,
        "",
        "",
        &normalized_local_path,
        &foxy_mode,
    )
    .await
    {
        Ok(repo) => repo,
        Err(err) => {
            error!(
                "Failed to upsert repository metadata for {}: {}",
                normalized_url, err
            );
            return None;
        }
    };

    let has_linked_addons =
        repository_has_linked_addons(context.clone(), repository.id as i64).await;
    let has_remote_state =
        repository_has_remote_state(context.clone(), repository.id as i64, enabled_overrides).await;
    let remote_mods_data = foxy_addons_data.as_ref().unwrap_or(&data);
    let remote_addon_names = collect_remote_addon_names(remote_mods_data);
    let linked_addon_names =
        repository_linked_addon_names(context.clone(), repository.id as i64).await;
    let remote_addon_links_match = remote_addon_names
        .as_ref()
        .zip(linked_addon_names.as_ref())
        .is_none_or(|(remote, linked)| remote == linked);
    let repository_space_paths_match =
        repository_addon_paths_match_space_layout(context.clone(), &repository).await;
    if !remote_addon_links_match
        && let (Some(remote), Some(linked)) = (&remote_addon_names, &linked_addon_names)
    {
        info!(
            "Repository {} linked addon metadata differs from remote manifest (remote_addons={} linked_addons={}); forcing metadata rebuild",
            repository.remote_url,
            remote.len(),
            linked.len()
        );
    }
    if !repository_space_paths_match {
        info!(
            "Repository {} addon paths no longer match repository-space shared-root availability; forcing metadata rebuild",
            repository.remote_url
        );
    }

    // If the repository has linked addons and remote state but its own checksums
    // are incomplete, try to fill them from sibling repositories that share the
    // same addon paths before giving up and forcing a full metadata rebuild.
    if has_linked_addons
        && has_remote_state
        && (repository.local_checksum.trim().is_empty()
            || repository.local_content_hash.trim().is_empty())
    {
        debug!(
            "Repository {} has incomplete checksums; attempting sibling pre-propagation",
            repository.remote_url
        );
        pre_propagate_sibling_checksums(context.clone(), &normalized_url).await;
        // Roll up addon-level checksums to repository level so
        // repository_refresh_complete() can see the propagated state.
        finalize_repository_hashes_from_mods(context.clone(), &normalized_url).await;
        finalize_repository_content_hashes_from_mods(context.clone(), &normalized_url).await;
        // Re-read the repository record to pick up propagated + rolled-up checksums
        if let Ok(refreshed) = load_repository_by_remote_url_and_local_path(
            context.clone(),
            &normalized_url,
            &normalized_local_path,
        )
        .await
        {
            repository = refreshed;
        }
    }

    let repository_complete =
        repository_refresh_complete(&repository, has_linked_addons, has_remote_state);
    let remote_graph_complete =
        repository_remote_graph_complete(&repository, has_linked_addons, has_remote_state);

    if should_skip_clean_remote_refresh(
        &repository,
        has_linked_addons,
        has_remote_state,
        context.recheck_level,
        force_refresh || !remote_addon_links_match || !repository_space_paths_match,
    ) {
        info!("Up-to-date: Repository {}.", repository.remote_url.clone());
        return Some(RemoteRepositoryMetadata {
            foxy_mode,
            app_update_url,
            skipped: true,
            repository_complete: true,
            remote_graph_complete: true,
            remote_graph_fetched: false,
        });
    }
    if should_skip_unchanged_remote_graph(
        &remote_checksum,
        previous_remote_checksum.as_deref(),
        local_path_unchanged,
        remote_graph_complete && remote_addon_links_match,
        context.recheck_level,
        force_refresh || !repository_space_paths_match,
    ) {
        info!(
            "Repository {} remote graph unchanged (repo.json checksum matches stored remote checksum); using existing DB metadata for local verification",
            repository.remote_url
        );
        return Some(RemoteRepositoryMetadata {
            foxy_mode,
            app_update_url,
            skipped: false,
            repository_complete,
            remote_graph_complete: true,
            remote_graph_fetched: false,
        });
    }
    if force_refresh && repository_complete {
        info!(
            "Repository {} metadata is unchanged, but forcing refresh to rebuild download targets",
            repository.remote_url
        );
    }
    if !repository_complete {
        info!(
            "Repository {} has incomplete local remote-state metadata; forcing metadata rebuild",
            repository.remote_url
        );
    }
    info!(
        "Recheck needed: Repository {} (local_path={})",
        repository.remote_url.clone(),
        repository.local_path
    );

    // TODO: Hack remove
    if repository.local_path.is_empty() {
        if !normalized_local_path.is_empty() {
            repository.local_path = normalized_local_path.clone();
        } else {
            repository.local_path = "./data/".to_string();
        }
    }

    if repository.local_path.is_empty() {
        warn!(
            "Repository {}: Local path is not set, please set the value and run recheck again",
            repository.name
        );
        return None;
    }

    // Sync - for FoxyMode repos, use the foxy_addons.json payload fetched above for its
    // mod lists (repo.json mod lists are empty in pure FoxyMode). Falls back to repo.json
    // data when the foxy_addons.json fetch failed or this is not a FoxyMode repo.
    let mods_data = foxy_addons_data.unwrap_or(data);

    let repository = Arc::new(repository);

    // Suppress WAL autocheckpoint during the metadata rebuild to avoid
    // frequent fsyncs from mid-write checkpoints. The WAL will grow temporarily
    // but self-corrects when autocheckpoint is restored.
    let db = context.db();
    let suppressed_wal = db
        .execute("PRAGMA wal_autocheckpoint = 0", params![])
        .await
        .is_ok();

    // Temporarily increase write concurrency for the rebuild. During first-run
    // metadata rebuild there are no competing writers (no downloads, no hash
    // persist, no UI writes), so extra permits are safe. We add permits before
    // and reclaim them after the rebuild completes.
    let extra_permits = (*DB_WRITE_PERMITS * 2).min(4);
    DB_WRITE_SEMAPHORE.add_permits(extra_permits);
    info!(
        "Metadata rebuild: temporarily raised write permits by {} (total ~{})",
        extra_permits,
        *DB_WRITE_PERMITS + extra_permits
    );

    remote_mods_with_data(
        context.clone(),
        repository.clone(),
        mods_data,
        enabled_overrides.cloned(),
    )
    .await;

    // Reclaim extra permits: acquire them and forget (permanently removes).
    // All rebuild work is done, so all permits from within are already released.
    if extra_permits > 0 {
        if let Ok(permit) = DB_WRITE_SEMAPHORE.acquire_many(extra_permits as u32).await {
            permit.forget();
        }
        info!(
            "Metadata rebuild: restored write permits to {}",
            *DB_WRITE_PERMITS
        );
    }

    if suppressed_wal {
        let _ = db
            .execute("PRAGMA wal_autocheckpoint = 256", params![])
            .await;
    }

    let final_has_linked_addons =
        repository_has_linked_addons(context.clone(), repository.id as i64).await;
    let final_has_remote_state =
        repository_has_remote_state(context.clone(), repository.id as i64, enabled_overrides).await;
    let final_repository = load_repository_by_remote_url_and_local_path(
        context.clone(),
        &normalized_url,
        &normalized_local_path,
    )
    .await
    .unwrap_or_else(|_| repository.as_ref().clone());
    let final_repository_complete = repository_refresh_complete(
        &final_repository,
        final_has_linked_addons,
        final_has_remote_state,
    );
    let final_remote_graph_complete = repository_remote_graph_complete(
        &final_repository,
        final_has_linked_addons,
        final_has_remote_state,
    );

    Some(RemoteRepositoryMetadata {
        foxy_mode,
        app_update_url,
        skipped: false,
        repository_complete: final_repository_complete,
        remote_graph_complete: final_remote_graph_complete,
        remote_graph_fetched: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{FoxyDb, params};
    use crate::core::models::repository::FoxyMode;
    use serde_json::json;

    // ── default_repo_image_path ─────────────────────────────────────────

    #[test]
    fn default_repo_image_path_is_repo_png() {
        assert_eq!(default_repo_image_path(), "repo.png");
    }

    // ── normalize_optional_url ──────────────────────────────────────────

    #[test]
    fn normalize_optional_url_some_valid() {
        assert_eq!(
            normalize_optional_url(Some("https://example.com/update")),
            Some("https://example.com/update".to_string())
        );
    }

    #[test]
    fn normalize_optional_url_trims_whitespace() {
        assert_eq!(
            normalize_optional_url(Some("  https://example.com  ")),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn normalize_optional_url_empty_string_returns_none() {
        assert_eq!(normalize_optional_url(Some("")), None);
    }

    #[test]
    fn normalize_optional_url_whitespace_only_returns_none() {
        assert_eq!(normalize_optional_url(Some("   ")), None);
    }

    #[test]
    fn normalize_optional_url_none_returns_none() {
        assert_eq!(normalize_optional_url(None), None);
    }

    // ── RemoteRepositoryManifest deserialization ────────────────────────

    #[test]
    fn deserialize_full_manifest() {
        let json = json!({
            "repoName": "Test Repo",
            "repoImagePath": "banner.png",
            "checksum": "ABC123",
            "foxyMode": "FoxyModeV1",
            "appUpdateUrl": "https://example.com/update.json"
        });

        let manifest: RemoteRepositoryManifest = serde_json::from_value(json).unwrap();
        assert_eq!(manifest.repo_name, "Test Repo");
        assert_eq!(manifest.repo_image_path, "banner.png");
        assert_eq!(manifest.checksum, "ABC123");
        assert_eq!(manifest.foxy_mode, "FoxyModeV1");
        assert_eq!(
            manifest.app_update_url,
            Some("https://example.com/update.json".to_string())
        );
    }

    #[test]
    fn deserialize_minimal_manifest_uses_defaults() {
        let json = json!({});

        let manifest: RemoteRepositoryManifest = serde_json::from_value(json).unwrap();
        assert_eq!(manifest.repo_name, "");
        assert_eq!(manifest.repo_image_path, "repo.png");
        assert_eq!(manifest.checksum, "");
        assert_eq!(manifest.foxy_mode, "");
        assert!(manifest.app_update_url.is_none());
    }

    #[test]
    fn deserialize_manifest_with_extra_fields() {
        let json = json!({
            "repoName": "Repo",
            "unknownField": "should be ignored",
            "anotherExtra": 42
        });

        let manifest: RemoteRepositoryManifest = serde_json::from_value(json).unwrap();
        assert_eq!(manifest.repo_name, "Repo");
    }

    // ── repository_refresh_complete ─────────────────────────────────────

    fn make_repo(local_checksum: &str, local_content_hash: &str) -> FoxyRepository {
        FoxyRepository {
            id: 1,
            name: "test".to_string(),
            remote_url: "https://example.com/".to_string(),
            local_path: "/mods/".to_string(),
            image: "repo.png".to_string(),
            local_checksum: local_checksum.to_string(),
            local_content_hash: local_content_hash.to_string(),
            remote_checksum: "REMOTE123".to_string(),
            foxy_mode: FoxyMode::None,
        }
    }

    #[test]
    fn refresh_complete_all_conditions_met() {
        let repo = make_repo("ABC", "DEF");
        assert!(repository_refresh_complete(&repo, true, true));
    }

    #[test]
    fn refresh_complete_no_linked_addons() {
        let repo = make_repo("ABC", "DEF");
        assert!(!repository_refresh_complete(&repo, false, true));
    }

    #[test]
    fn refresh_complete_no_remote_state() {
        let repo = make_repo("ABC", "DEF");
        assert!(!repository_refresh_complete(&repo, true, false));
    }

    #[test]
    fn refresh_complete_empty_local_checksum() {
        let repo = make_repo("", "DEF");
        assert!(!repository_refresh_complete(&repo, true, true));
    }

    #[test]
    fn refresh_complete_empty_content_hash() {
        let repo = make_repo("ABC", "");
        assert!(!repository_refresh_complete(&repo, true, true));
    }

    #[test]
    fn refresh_complete_whitespace_checksum() {
        let repo = make_repo("  ", "DEF");
        assert!(!repository_refresh_complete(&repo, true, true));
    }

    #[tokio::test]
    async fn repository_space_layout_detects_addon_still_bound_to_override_folder() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());

        let unique = format!(
            "foxy-space-layout-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let shared = root.join("shared");
        let target = root.join("target");
        std::fs::create_dir_all(shared.join("@common")).expect("create shared addon");
        std::fs::create_dir_all(&target).expect("create target root");

        // A parent repository row is required for the repository_addons FK.
        fdb.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (1, 'r', 'https://example.com/', ?)",
            params![target.to_string_lossy().to_string()],
        )
        .await
        .expect("seed repository");
        fdb.execute(
            "INSERT INTO addons (id, name, local_path, required) VALUES (1, '@common', ?, 1)",
            params![target.join("@common").to_string_lossy().to_string()],
        )
        .await
        .expect("insert linked addon");
        fdb.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 1)",
            params![],
        )
        .await
        .expect("link addon to repository");

        let mut repo = make_repo("LOCAL", "CONTENT");
        repo.local_path = target.to_string_lossy().to_string();
        let context = Arc::new(
            FoxyContext::new(db, reqwest::Client::new())
                .with_repository_space_shared_path(Some(shared.to_string_lossy().to_string())),
        );

        assert!(!repository_addon_paths_match_space_layout(context, &repo).await);
        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    // ── should_skip_remote_refresh ──────────────────────────────────────

    #[test]
    fn skip_refresh_when_checksums_match_and_complete() {
        let mut repo = make_repo("MATCH", "HASH");
        repo.remote_checksum = "MATCH".to_string();
        assert!(should_skip_clean_remote_refresh(
            &repo,
            true,
            true,
            RecheckLevel::DEFAULT,
            false
        ));
    }

    #[test]
    fn no_skip_when_checksums_differ() {
        let mut repo = make_repo("LOCAL", "HASH");
        repo.remote_checksum = "REMOTE".to_string();
        assert!(!should_skip_clean_remote_refresh(
            &repo,
            true,
            true,
            RecheckLevel::DEFAULT,
            false
        ));
    }

    #[test]
    fn no_skip_when_force_refresh() {
        let mut repo = make_repo("MATCH", "HASH");
        repo.remote_checksum = "MATCH".to_string();
        assert!(!should_skip_clean_remote_refresh(
            &repo,
            true,
            true,
            RecheckLevel::DEFAULT,
            true
        ));
    }

    #[test]
    fn no_skip_when_recheck_level_repository() {
        let mut repo = make_repo("MATCH", "HASH");
        repo.remote_checksum = "MATCH".to_string();
        assert!(!should_skip_clean_remote_refresh(
            &repo,
            true,
            true,
            RecheckLevel::REPOSITORY,
            false
        ));
    }

    #[test]
    fn no_skip_when_not_complete() {
        let mut repo = make_repo("MATCH", "");
        repo.remote_checksum = "MATCH".to_string();
        assert!(!should_skip_clean_remote_refresh(
            &repo,
            true,
            true,
            RecheckLevel::DEFAULT,
            false
        ));
    }

    // ── FoxyMode ────────────────────────────────────────────────────────

    #[test]
    fn skip_unchanged_remote_graph_when_remote_matches_stored_remote() {
        assert!(should_skip_unchanged_remote_graph(
            "REMOTE",
            Some("REMOTE"),
            true,
            true,
            RecheckLevel::DEFAULT,
            false
        ));
    }

    #[test]
    fn no_skip_unchanged_remote_graph_when_local_path_changed() {
        assert!(!should_skip_unchanged_remote_graph(
            "REMOTE",
            Some("REMOTE"),
            false,
            true,
            RecheckLevel::DEFAULT,
            false
        ));
    }

    #[test]
    fn no_skip_unchanged_remote_graph_when_graph_incomplete() {
        assert!(!should_skip_unchanged_remote_graph(
            "REMOTE",
            Some("REMOTE"),
            true,
            false,
            RecheckLevel::DEFAULT,
            false
        ));
    }

    #[test]
    fn no_skip_unchanged_remote_graph_when_remote_changed() {
        assert!(!should_skip_unchanged_remote_graph(
            "NEW",
            Some("OLD"),
            true,
            true,
            RecheckLevel::DEFAULT,
            false
        ));
    }

    #[test]
    fn local_path_identity_allows_empty_override() {
        let repo = make_repo("ABC", "DEF");
        assert!(local_path_identity_unchanged(&repo, ""));
    }

    #[test]
    fn local_path_identity_ignores_trailing_separator_variants() {
        let mut repo = make_repo("ABC", "DEF");
        repo.local_path = "C:/mods/tfr/".to_string();

        assert!(local_path_identity_unchanged(&repo, "C:/mods/tfr"));
    }

    #[cfg(windows)]
    #[test]
    fn local_path_identity_uses_windows_case_rules() {
        let mut repo = make_repo("ABC", "DEF");
        repo.local_path = "C:/Mods/TFR/".to_string();

        assert!(local_path_identity_unchanged(&repo, "c:\\mods\\tfr\\"));
    }

    fn addon_state(name: &str) -> RepositoryAddonRemoteState {
        RepositoryAddonRemoteState {
            name: name.to_string(),
            enabled: true,
            remote_checksum: "REMOTE".to_string(),
            file_count: 1,
            missing_file_remote_checksums: 0,
            part_count: 1,
            missing_part_remote_checksums: 0,
        }
    }

    #[test]
    fn remote_state_complete_requires_linked_addons() {
        assert!(!remote_state_complete_from_addons(&[], None));
    }

    #[test]
    fn remote_state_complete_accepts_ready_enabled_addon() {
        assert!(remote_state_complete_from_addons(
            &[addon_state("ace")],
            None
        ));
    }

    #[test]
    fn remote_state_complete_rejects_missing_addon_checksum() {
        let mut addon = addon_state("ace");
        addon.remote_checksum.clear();
        assert!(!remote_state_complete_from_addons(&[addon], None));
    }

    #[test]
    fn remote_state_complete_rejects_enabled_addon_without_files() {
        let mut addon = addon_state("ace");
        addon.file_count = 0;
        assert!(!remote_state_complete_from_addons(&[addon], None));
    }

    #[test]
    fn remote_state_complete_rejects_missing_file_checksum() {
        let mut addon = addon_state("ace");
        addon.missing_file_remote_checksums = 1;
        assert!(!remote_state_complete_from_addons(&[addon], None));
    }

    #[test]
    fn remote_state_complete_accepts_whole_file_manifest_without_parts() {
        let mut addon = addon_state("ace");
        addon.part_count = 0;
        assert!(remote_state_complete_from_addons(&[addon], None));
    }

    #[test]
    fn remote_state_complete_rejects_missing_part_checksum_when_parts_exist() {
        let mut addon = addon_state("ace");
        addon.missing_part_remote_checksums = 1;
        assert!(!remote_state_complete_from_addons(&[addon], None));
    }

    #[test]
    fn remote_state_complete_ignores_disabled_addon_missing_files() {
        let mut addon = addon_state("ace");
        addon.enabled = false;
        addon.file_count = 0;
        assert!(remote_state_complete_from_addons(&[addon], None));
    }

    #[test]
    fn remote_state_complete_respects_enabled_override() {
        let mut addon = addon_state("ace");
        addon.enabled = false;
        addon.file_count = 0;
        let overrides = HashMap::from([("ace".to_string(), true)]);
        assert!(!remote_state_complete_from_addons(
            &[addon],
            Some(&overrides)
        ));
    }

    #[test]
    fn collect_remote_addon_names_deduplicates_required_and_optional() {
        let data = serde_json::json!({
            "requiredMods": [
                { "modName": "@ace" },
                { "modName": "@cba_a3" }
            ],
            "optionalMods": [
                { "modName": "@ACE" },
                { "modName": "" },
                { "modName": "../unsafe" }
            ]
        });

        let names = collect_remote_addon_names(&data).expect("mod list should be present");

        assert_eq!(names.len(), 2);
        assert!(names.contains("@ace"));
        assert!(names.contains("@cba_a3"));
    }

    #[test]
    fn collect_remote_addon_names_none_without_mod_lists() {
        let data = serde_json::json!({ "checksum": "abc" });

        assert!(collect_remote_addon_names(&data).is_none());
    }

    #[test]
    fn foxy_mode_from_db_str_v1() {
        assert_eq!(FoxyMode::from_db_str("FoxyModeV1"), FoxyMode::V1);
    }

    #[test]
    fn foxy_mode_from_db_str_empty() {
        assert_eq!(FoxyMode::from_db_str(""), FoxyMode::None);
    }

    #[test]
    fn foxy_mode_from_db_str_unknown() {
        assert_eq!(FoxyMode::from_db_str("SomethingElse"), FoxyMode::None);
    }

    #[test]
    fn foxy_mode_roundtrip() {
        assert_eq!(
            FoxyMode::from_db_str(FoxyMode::V1.as_db_str()),
            FoxyMode::V1
        );
        assert_eq!(
            FoxyMode::from_db_str(FoxyMode::None.as_db_str()),
            FoxyMode::None
        );
    }
}
