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
use crate::core::tasks::remote_mods::{
    RemoteRebuildWork, remote_mods_with_data, resolve_mod_local_path,
    stage_manifests_for_publication,
};
use crate::core::utils::fetch_json::fetch_json_fresh;
use crate::core::utils::speed_of_light::{SolLight, op_id_extra, sol_line};
use crate::ui::types::HashAlgorithmPreference;
use log::{debug, error, info, warn};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod prepared;
pub(crate) use prepared::probe_prepared_repository_checksum;

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
    /// The server explicitly published no addons. Existing local ownership is
    /// retained; this does not authorize deleting installed content.
    pub accepted_empty: bool,
}

fn explicitly_publishes_no_addons(data: &Value) -> bool {
    ["requiredMods", "optionalMods"].into_iter().all(|key| {
        data.get(key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    })
}

pub(super) fn publishes_complete_addon_lists(data: &Value) -> bool {
    let lists: Vec<_> = ["requiredMods", "optionalMods"]
        .into_iter()
        .filter_map(|key| data.get(key))
        .collect();
    lists.iter().all(|list| list.is_array())
        && (lists.len() == 2
            || lists
                .iter()
                .any(|list| list.as_array().is_some_and(|items| !items.is_empty())))
}

fn publication_identity(checksum: &str, addon_index: &Value) -> String {
    let identity = serde_json::json!({
        "checksum": checksum.trim(),
        "requiredMods": addon_index.get("requiredMods"),
        "optionalMods": addon_index.get("optionalMods"),
    });
    identity.to_string()
}

async fn fetch_publication_identity(
    context: Arc<FoxyContext>,
    normalized_url: &str,
    foxy_mode: bool,
) -> Option<String> {
    let repo_data = fetch_json_fresh(context.clone(), &format!("{}repo.json", normalized_url))
        .await
        .ok()?;
    let repo_manifest =
        serde_json::from_value::<RemoteRepositoryManifest>(repo_data.clone()).ok()?;
    if foxy_mode {
        let addon_data = fetch_json_fresh(context, &format!("{}foxy_addons.json", normalized_url))
            .await
            .ok()?;
        if !publishes_complete_addon_lists(&addon_data) {
            return None;
        }
        let checksum = addon_data
            .get("checksum")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&repo_manifest.checksum);
        Some(publication_identity(checksum, &addon_data))
    } else if publishes_complete_addon_lists(&repo_data) {
        Some(publication_identity(&repo_manifest.checksum, &repo_data))
    } else {
        None
    }
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

/// The repository checksum stored at the start of a refresh. A new instance
/// keeps an empty value until its addon data is applied, so an interrupted
/// refresh still looks changed to the next probe.
fn repository_remote_checksum_until_applied(previous: Option<&str>) -> &str {
    previous
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
}

async fn store_repository_remote_checksum(
    context: &FoxyContext,
    repository_id: u64,
    remote_checksum: &str,
) -> bool {
    match context
        .db()
        .execute_retry(
            "store repository remote checksum",
            "UPDATE repositories SET remote_checksum = ? WHERE id = ?",
            params![remote_checksum, repository_id as i64],
        )
        .await
    {
        Ok(_) => true,
        Err(err) => {
            error!(
                "Failed to store repository remote checksum for repository_id={}: {}",
                repository_id, err
            );
            false
        }
    }
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

/// Published addons by lowercase name with the `checkSum` the addon list
/// gives for each (empty when it gives none).
fn collect_remote_addon_checksums(data: &Value) -> Option<HashMap<String, String>> {
    let mut checksums = HashMap::new();
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
            let checksum = mod_data
                .get("checkSum")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            checksums.insert(mod_name.to_lowercase(), checksum);
        }
    }

    saw_mod_list.then_some(checksums)
}

/// Whether every addon checksum the remote addon list publishes is the one
/// stored for that addon. The stored value is copied from the same list, so
/// a difference means the addon's manifest changed since it was last read,
/// even when the repository checksum already reads as clean: a sibling sync
/// can bring the local files up to the new version first.
fn remote_addon_checksums_match(
    remote: &HashMap<String, String>,
    stored: &HashMap<String, String>,
) -> bool {
    remote.iter().all(|(name, checksum)| {
        checksum.is_empty()
            || stored
                .get(name)
                .is_some_and(|stored| stored.trim().eq_ignore_ascii_case(checksum))
    })
}

async fn repository_linked_addon_checksums(
    context: Arc<FoxyContext>,
    repository_id: i64,
) -> Option<HashMap<String, String>> {
    let rows = match context
        .db()
        .query_all(
            r#"SELECT a.name AS name, a.remote_checksum AS remote_checksum
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
            .filter_map(|row| {
                let name = row.get_string("name").ok()?.trim().to_lowercase();
                let checksum = row.get_string("remote_checksum").unwrap_or_default();
                (!name.is_empty()).then_some((name, checksum))
            })
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
    // `remote_state_complete_from_addons` is satisfied with part_count = 0. The
    // one hole in that derivation (files persisted, parts never landed) is
    // closed by the `LIMIT 1` part probe below.
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

    let mut states = rows
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

    let mut empty_addons = context.empty_remote_addon_names();
    if states.iter().any(|addon| addon.file_count == 0)
        && let Ok(Some(row)) = context
            .db()
            .query_one(
                "SELECT remote_url, local_path FROM repositories WHERE id = ?",
                params![repository_id],
            )
            .await
        && let (Ok(url), Ok(path)) = (row.get_string("remote_url"), row.get_string("local_path"))
        && let Ok((_, accepted)) =
            crate::core::api::check_contracts::accepted_empty_scope(&context, &url, &path)
    {
        empty_addons.extend(accepted);
    }
    for addon in &mut states {
        if empty_addons.contains(&addon.name.to_lowercase()) {
            addon.enabled = false;
        }
    }

    if !remote_state_complete_from_addons(&states, enabled_overrides) {
        return false;
    }

    // The derivation above assumes the parts landed with the files. A deferred
    // rebuild writes the file rows inline and holds the part rows in memory until
    // the hash pass persists them, so a process exit in between leaves every file
    // "complete" over an empty `subfiles`. Such a graph cannot produce local tree
    // hashes and the quick scan preflight already refuses it; the skip decision
    // must agree, or every sync re-reads the whole repository for nothing.
    let enabled_file_count: i64 = states
        .iter()
        .filter(|addon| addon_enabled_for_remote_state(addon, enabled_overrides))
        .map(|addon| addon.file_count)
        .sum();
    if enabled_file_count == 0 || context.deferred_part_count() > 0 {
        return true;
    }
    match repository_part_rows_exist(context, repository_id).await {
        Some(true) => true,
        Some(false) => {
            info!(
                "Repository id={} has {} enabled files but no part rows and no deferred part inserts; remote state is incomplete",
                repository_id, enabled_file_count
            );
            false
        }
        None => false,
    }
}

/// Indexed `LIMIT 1` probe for any part row under the repository's enabled
/// addons. Mirrors the quick scan preflight probe so both gates agree on what a
/// missing part graph looks like.
async fn repository_part_rows_exist(context: Arc<FoxyContext>, repository_id: i64) -> Option<bool> {
    match context
        .db()
        .query_one(
            r#"SELECT 1 FROM subfiles sf
               WHERE sf.file_id IN (
                   SELECT af.file_id
                   FROM addon_files af
                   JOIN repository_addons ra ON ra.addon_id = af.addon_id
                   JOIN addons a ON a.id = ra.addon_id
                   WHERE ra.repository_id = ? AND a.enabled = 1
               )
               LIMIT 1"#,
            params![repository_id],
        )
        .await
    {
        Ok(row) => Some(row.is_some()),
        Err(err) => {
            warn!(
                "Failed to probe part rows for repository_id={}: {}",
                repository_id, err
            );
            None
        }
    }
}

/// Emit the `SOL op=remote_refresh` action record (conventions/SPEED_OF_LIGHT.md,
/// O5): one line per refresh with its branch as `outcome`, the index requests
/// it made, and the manifest work when the graph was rebuilt.
fn log_remote_refresh_sol(
    context: &FoxyContext,
    started: std::time::Instant,
    outcome: &str,
    index_requests: usize,
    work: Option<RemoteRebuildWork>,
) {
    let work = work.unwrap_or_default();
    let secs = |d: std::time::Duration| format!("{:.3}", d.as_secs_f64());
    let mut extras = vec![
        ("outcome", outcome.to_string()),
        ("index_requests", index_requests.to_string()),
        ("manifest_requests", work.mods.to_string()),
        ("mods", work.mods.to_string()),
        ("files", work.files.to_string()),
        ("parts", work.parts.to_string()),
        ("response_bytes", work.response_bytes.to_string()),
        ("fetch_sum_s", secs(work.fetch)),
        ("parse_sum_s", secs(work.parse)),
        ("persist_sum_s", secs(work.persist)),
        ("fan_out_wall_s", secs(work.fan_out_wall)),
        ("timer_scope", "action_wall".to_string()),
    ];
    extras.extend(op_id_extra(context.operation_id()));
    info!(
        "{}",
        sol_line(
            "remote_refresh",
            0,
            started.elapsed(),
            &SolLight::SelfBaseline,
            &extras,
        )
    );
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
    remote_repository_attempt(
        context,
        repository_url,
        local_path_override,
        enabled_overrides,
        force_refresh,
        hash_algorithm_preference,
        0,
    )
    .await
}

async fn remote_repository_attempt(
    context: Arc<FoxyContext>,
    repository_url: &str,
    local_path_override: Option<&str>,
    enabled_overrides: Option<&std::collections::HashMap<String, bool>>,
    force_refresh: bool,
    hash_algorithm_preference: HashAlgorithmPreference,
    retry_count: u8,
) -> Option<RemoteRepositoryMetadata> {
    context.clear_staged_manifest_responses();
    // Normalize remote URL to always have trailing slash for consistent path joins
    let normalized_url = crate::core::models::repository::normalize_repository_url(repository_url);
    let refresh_started = std::time::Instant::now();
    let mut index_requests = 1usize;
    let repo_url = format!("{}repo.json", normalized_url);
    info!("Loading repository metadata from: {}", repo_url);

    let data = match fetch_json_fresh(context.clone(), &repo_url).await {
        Ok(d) => d,
        Err(e) => {
            error!(
                "Error fetching addons for repository {}: {}",
                repository_url, e
            );
            log_remote_refresh_sol(&context, refresh_started, "failed", index_requests, None);
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
        index_requests += 1;
        info!(
            "FoxyMode detected - loading mod metadata from: {}",
            foxy_addons_url
        );
        match fetch_json_fresh(context.clone(), &foxy_addons_url).await {
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
    let remote_mods_data = foxy_addons_data.as_ref().unwrap_or(&data);
    if !publishes_complete_addon_lists(remote_mods_data) {
        warn!(
            "Repository {} addon index is incomplete: requiredMods and optionalMods must both be present arrays",
            normalized_url
        );
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "incomplete_index",
            index_requests,
            None,
        );
        return None;
    }
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
        repository_remote_checksum_until_applied(previous_remote_checksum.as_deref()),
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
            log_remote_refresh_sol(&context, refresh_started, "failed", index_requests, None);
            return None;
        }
    };
    // Decisions below compare the fetched checksum. The database row keeps the
    // previous value until the data behind this one has been applied.
    repository.remote_checksum = remote_checksum.clone();

    let has_linked_addons =
        repository_has_linked_addons(context.clone(), repository.id as i64).await;
    let has_remote_state =
        repository_has_remote_state(context.clone(), repository.id as i64, enabled_overrides).await;
    if explicitly_publishes_no_addons(remote_mods_data) {
        let expected_publication = publication_identity(&remote_checksum, remote_mods_data);
        let current_publication =
            fetch_publication_identity(context.clone(), &normalized_url, foxy_mode.is_foxy()).await;
        index_requests += if foxy_mode.is_foxy() { 2 } else { 1 };
        if current_publication.as_deref() != Some(expected_publication.as_str()) {
            if retry_count == 0 {
                return Box::pin(remote_repository_attempt(
                    context,
                    repository_url,
                    local_path_override,
                    enabled_overrides,
                    true,
                    hash_algorithm_preference,
                    1,
                ))
                .await;
            }
            log_remote_refresh_sol(
                &context,
                refresh_started,
                "publication_changed",
                index_requests,
                None,
            );
            return None;
        }
        if crate::core::api::check_contracts::record_empty_scope(
            &context,
            &normalized_url,
            &normalized_local_path,
            true,
            None,
        )
        .is_err()
        {
            return None;
        }
        info!(
            "Repository {} explicitly publishes no addons; retaining its existing local ownership graph",
            normalized_url
        );
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "accepted_empty",
            index_requests,
            None,
        );
        return Some(RemoteRepositoryMetadata {
            foxy_mode,
            app_update_url,
            skipped: false,
            repository_complete: false,
            remote_graph_complete: false,
            remote_graph_fetched: false,
            accepted_empty: true,
        });
    }
    let expected_publication = publication_identity(&remote_checksum, remote_mods_data);
    let remote_addon_checksums = collect_remote_addon_checksums(remote_mods_data);
    let linked_addon_checksums =
        repository_linked_addon_checksums(context.clone(), repository.id as i64).await;
    let remote_addon_names: Option<HashSet<String>> = remote_addon_checksums
        .as_ref()
        .map(|checksums| checksums.keys().cloned().collect());
    let linked_addon_names: Option<HashSet<String>> = linked_addon_checksums
        .as_ref()
        .map(|checksums| checksums.keys().cloned().collect());
    let remote_addon_links_match = remote_addon_names
        .as_ref()
        .zip(linked_addon_names.as_ref())
        .is_none_or(|(remote, linked)| remote == linked);
    let remote_addon_versions_match = remote_addon_checksums
        .as_ref()
        .zip(linked_addon_checksums.as_ref())
        .is_none_or(|(remote, stored)| remote_addon_checksums_match(remote, stored));
    let remote_addon_properties_match =
        prepared::addon_properties_match(&context, repository.id, remote_mods_data)
            .await
            .unwrap_or(true);
    if remote_addon_links_match && !remote_addon_versions_match {
        info!(
            "Repository {} addon checksums differ from the remote addon list; forcing metadata rebuild",
            repository.remote_url
        );
    }
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

    let has_forced_mod_refreshes =
        !context.forced_mod_refreshes.is_empty() || !context.reapplied_mod_manifests.is_empty();
    if should_skip_clean_remote_refresh(
        &repository,
        has_linked_addons,
        has_remote_state,
        context.recheck_level,
        force_refresh
            || has_forced_mod_refreshes
            || !remote_addon_links_match
            || !remote_addon_versions_match
            || !remote_addon_properties_match
            || !repository_space_paths_match,
    ) {
        if !store_repository_remote_checksum(&context, repository.id, &remote_checksum).await {
            log_remote_refresh_sol(&context, refresh_started, "failed", index_requests, None);
            return None;
        }
        crate::core::api::check_contracts::record_empty_scope(
            &context,
            &normalized_url,
            &normalized_local_path,
            false,
            None,
        )
        .ok()?;
        info!("Up-to-date: Repository {}.", repository.remote_url.clone());
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "skipped_clean",
            index_requests,
            None,
        );
        return Some(RemoteRepositoryMetadata {
            foxy_mode,
            app_update_url,
            skipped: true,
            repository_complete: true,
            remote_graph_complete: true,
            remote_graph_fetched: false,
            accepted_empty: false,
        });
    }
    if should_skip_unchanged_remote_graph(
        &remote_checksum,
        previous_remote_checksum.as_deref(),
        local_path_unchanged,
        remote_graph_complete
            && remote_addon_links_match
            && remote_addon_versions_match
            && remote_addon_properties_match,
        context.recheck_level,
        force_refresh || has_forced_mod_refreshes || !repository_space_paths_match,
    ) {
        crate::core::api::check_contracts::record_empty_scope(
            &context,
            &normalized_url,
            &normalized_local_path,
            false,
            None,
        )
        .ok()?;
        info!(
            "Repository {} remote graph unchanged (repo.json checksum matches stored remote checksum); using existing DB metadata for local verification",
            repository.remote_url
        );
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "graph_unchanged",
            index_requests,
            None,
        );
        return Some(RemoteRepositoryMetadata {
            foxy_mode,
            app_update_url,
            skipped: false,
            repository_complete,
            remote_graph_complete: true,
            remote_graph_fetched: false,
            accepted_empty: false,
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
        log_remote_refresh_sol(&context, refresh_started, "failed", index_requests, None);
        return None;
    }

    // Sync - for FoxyMode repos, use the foxy_addons.json payload fetched above for its
    // mod lists (repo.json mod lists are empty in pure FoxyMode). Falls back to repo.json
    // data when the foxy_addons.json fetch failed or this is not a FoxyMode repo.
    let mods_data = foxy_addons_data.unwrap_or(data);

    let repository = Arc::new(repository);

    if let Err(error) = stage_manifests_for_publication(
        context.clone(),
        repository.as_ref(),
        &mods_data,
        enabled_overrides,
    )
    .await
    {
        context.clear_staged_manifest_responses();
        if error == crate::core::tasks::remote_files::ADDON_PUBLICATION_CHANGED && retry_count == 0
        {
            return Box::pin(remote_repository_attempt(
                context,
                repository_url,
                local_path_override,
                enabled_overrides,
                true,
                hash_algorithm_preference,
                1,
            ))
            .await;
        }
        warn!(
            "Could not stage repository {} manifests before applying metadata: {}",
            normalized_url, error
        );
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "manifest_stage_failed",
            index_requests,
            None,
        );
        return None;
    }
    let current_publication =
        fetch_publication_identity(context.clone(), &normalized_url, foxy_mode.is_foxy()).await;
    index_requests += if foxy_mode.is_foxy() { 2 } else { 1 };
    if current_publication.as_deref() != Some(expected_publication.as_str()) {
        context.clear_staged_manifest_responses();
        warn!(
            "Repository {} publication changed while staging manifests; retrying before applying metadata ({}/{})",
            normalized_url,
            retry_count + 1,
            2
        );
        if retry_count == 0 {
            return Box::pin(remote_repository_attempt(
                context,
                repository_url,
                local_path_override,
                enabled_overrides,
                true,
                hash_algorithm_preference,
                1,
            ))
            .await;
        }
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "publication_changed",
            index_requests,
            None,
        );
        return None;
    }

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

    let rebuild_work = remote_mods_with_data(
        context.clone(),
        repository.clone(),
        mods_data,
        enabled_overrides.cloned(),
        true,
    )
    .await;
    context.clear_staged_manifest_responses();

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

    let final_has_linked_addons =
        repository_has_linked_addons(context.clone(), repository.id as i64).await;
    let final_has_remote_state =
        repository_has_remote_state(context.clone(), repository.id as i64, enabled_overrides).await;
    let stored_addon_checksums =
        repository_linked_addon_checksums(context.clone(), repository.id as i64).await;
    let published_addons_applied = remote_addon_checksums.as_ref().is_none_or(|published| {
        stored_addon_checksums
            .as_ref()
            .is_some_and(|stored| remote_addon_checksums_match(published, stored))
    });
    let graph_ready = final_has_linked_addons
        && final_has_remote_state
        && published_addons_applied
        && !remote_checksum.trim().is_empty();
    if !graph_ready
        || !store_repository_remote_checksum(&context, repository.id, &remote_checksum).await
    {
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "failed",
            index_requests,
            Some(rebuild_work),
        );
        return None;
    }
    if crate::core::api::check_contracts::record_empty_scope(
        &context,
        &normalized_url,
        &normalized_local_path,
        false,
        None,
    )
    .is_err()
    {
        return None;
    }
    for (name, empty) in context.remote_addon_empty_states() {
        if crate::core::api::check_contracts::record_empty_scope(
            &context,
            &normalized_url,
            &normalized_local_path,
            false,
            Some((&name, empty)),
        )
        .is_err()
        {
            return None;
        }
    }
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
    if !final_remote_graph_complete {
        log_remote_refresh_sol(
            &context,
            refresh_started,
            "failed",
            index_requests,
            Some(rebuild_work),
        );
        return None;
    }
    log_remote_refresh_sol(
        &context,
        refresh_started,
        "rebuilt",
        index_requests,
        Some(rebuild_work),
    );

    Some(RemoteRepositoryMetadata {
        foxy_mode,
        app_update_url,
        skipped: false,
        repository_complete: final_repository_complete,
        remote_graph_complete: final_remote_graph_complete,
        remote_graph_fetched: true,
        accepted_empty: false,
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

    async fn seed_repository_with_file(fdb: &FoxyDb) {
        fdb.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path, remote_checksum) VALUES (1, 'r', 'https://example.com/', 'C:/mods/', 'REPO')",
            params![],
        )
        .await
        .expect("seed repository");
        fdb.execute(
            "INSERT INTO addons (id, name, local_path, enabled, remote_checksum, required) VALUES (1, '@a', 'C:/mods/@a', 1, 'ADDON', 1)",
            params![],
        )
        .await
        .expect("seed addon");
        fdb.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 1)",
            params![],
        )
        .await
        .expect("link addon");
        fdb.execute(
            "INSERT INTO files (id, name, remote_path, local_path, remote_checksum, length) VALUES (10, 'a.pbo', 'https://example.com/@a/addons/a.pbo', 'C:/mods/@a/addons/a.pbo', 'FILE', 4)",
            params![],
        )
        .await
        .expect("seed file");
        fdb.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (1, 10)",
            params![],
        )
        .await
        .expect("link file");
    }

    #[tokio::test]
    async fn remote_state_incomplete_when_files_have_no_part_rows() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());
        seed_repository_with_file(&fdb).await;
        let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));

        assert!(!repository_has_remote_state(context.clone(), 1, None).await);

        fdb.execute(
            "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) VALUES (10, 'p0', 0, 0, 4, 0, '', 'PART', 0)",
            params![],
        )
        .await
        .expect("seed part");

        assert!(repository_has_remote_state(context, 1, None).await);
    }

    #[tokio::test]
    async fn remote_state_complete_when_part_rows_are_deferred_in_memory() {
        use crate::core::models::context::DeferredPartInsert;

        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());
        seed_repository_with_file(&fdb).await;
        let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
        context.set_defer_part_inserts(true);
        context.buffer_deferred_parts(vec![DeferredPartInsert {
            file_id: 10,
            path: "p0".to_owned(),
            remote_length: 4,
            remote_start: 0,
            remote_checksum: "PART".to_owned(),
            data_order: 0,
        }]);

        assert!(repository_has_remote_state(context, 1, None).await);
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
        repo.local_path = "C:/Mods/Alpha/".to_string();

        assert!(local_path_identity_unchanged(&repo, "c:\\mods\\alpha\\"));
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
    fn collect_remote_addon_checksums_deduplicates_required_and_optional() {
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

        let names = collect_remote_addon_checksums(&data).expect("mod list should be present");

        assert_eq!(names.len(), 2);
        assert!(names.contains_key("@ace"));
        assert!(names.contains_key("@cba_a3"));
    }

    #[test]
    fn addon_list_checksum_change_is_detected_against_stored_checksums() {
        let data = serde_json::json!({
            "requiredMods": [
                { "modName": "@Mod_01", "checkSum": "NEW" },
                { "modName": "@mod_02", "checkSum": "same" },
                { "modName": "@mod_03" }
            ]
        });
        let remote = collect_remote_addon_checksums(&data).expect("mod list should be present");
        let stored = |first: &str| {
            HashMap::from([
                ("@mod_01".to_string(), first.to_string()),
                ("@mod_02".to_string(), "SAME".to_string()),
                ("@mod_03".to_string(), "ANY".to_string()),
            ])
        };

        assert_eq!(remote.get("@mod_01").map(String::as_str), Some("NEW"));
        assert!(remote_addon_checksums_match(&remote, &stored("NEW")));
        assert!(!remote_addon_checksums_match(&remote, &stored("OLD")));
        assert!(!remote_addon_checksums_match(&remote, &HashMap::new()));
    }

    #[test]
    fn collect_remote_addon_checksums_none_without_mod_lists() {
        let data = serde_json::json!({ "checksum": "abc" });

        assert!(collect_remote_addon_checksums(&data).is_none());
    }

    #[test]
    fn explicitly_empty_addon_lists_are_distinct_from_omitted_lists() {
        assert!(explicitly_publishes_no_addons(&json!({
            "requiredMods": [],
            "optionalMods": []
        })));
        assert!(!explicitly_publishes_no_addons(&json!({
            "requiredMods": [],
            "checksum": "abc"
        })));
        assert!(!explicitly_publishes_no_addons(&json!({
            "requiredMods": [{"modName": "@mod_01"}],
            "optionalMods": []
        })));
        assert!(!publishes_complete_addon_lists(
            &json!({"requiredMods": []})
        ));
        assert!(publishes_complete_addon_lists(&json!({
            "requiredMods": [{"modName": "@mod_01"}]
        })));
        assert!(publishes_complete_addon_lists(&json!({
            "requiredMods": [],
            "optionalMods": []
        })));
    }

    #[test]
    fn publication_identity_includes_names_even_when_checksum_matches() {
        let first = json!({
            "requiredMods": [{"modName": "@mod_01", "checkSum": "SAME"}],
            "optionalMods": []
        });
        let second = json!({
            "requiredMods": [{"modName": "@mod_02", "checkSum": "SAME"}],
            "optionalMods": []
        });

        assert_ne!(
            publication_identity("ROLLUP", &first),
            publication_identity("ROLLUP", &second)
        );
    }

    #[tokio::test]
    async fn fresh_publication_probe_detects_membership_change_over_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for addon_name in ["@mod_01", "@mod_02"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                let body = format!(
                    r#"{{"repoName":"MainRepo","checksum":"ROLLUP","requiredMods":[{{"modName":"{addon_name}","checkSum":"SAME"}}],"optionalMods":[]}}"#
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let context = Arc::new(FoxyContext::new(
            crate::core::tasks::db_turso::build_test_database().await,
            reqwest::Client::new(),
        ));

        let first = fetch_publication_identity(context.clone(), &url, false)
            .await
            .unwrap();
        let second = fetch_publication_identity(context, &url, false)
            .await
            .unwrap();

        assert_ne!(first, second);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn repeated_publication_race_keeps_previous_child_graph() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let manifests = r#"{"files":[{"path":"new.bin","checksum":"FILE_NEW","length":4,"parts":[{"path":"new.bin","checksum":"PART_NEW","start":0,"length":4}]}]}"#;
            let publications = ["PUB_A", "PUB_B", "PUB_B", "PUB_C"];
            let mut repo_requests = 0usize;
            let mut total_requests = 0usize;
            while total_requests < 6 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let length = stream.read(&mut request).await.unwrap();
                let first_line = String::from_utf8_lossy(&request[..length]);
                let body = if first_line.starts_with("GET /repo.json ") {
                    let checksum = publications[repo_requests];
                    repo_requests += 1;
                    format!(
                        r#"{{"repoName":"MainRepo","checksum":"{checksum}","requiredMods":[{{"modName":"@mod_new","checkSum":"NEW_ADDON"}}],"optionalMods":[]}}"#
                    )
                } else {
                    manifests.to_owned()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                total_requests += 1;
            }
        });

        let database = crate::core::tasks::db_turso::build_test_database().await;
        let context = Arc::new(FoxyContext::new(database, reqwest::Client::new()));
        let db = context.db();
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path, local_checksum, remote_checksum) VALUES (1, 'MainRepo', ?, 'R:/Mods/MainRepo/', 'LOCAL_OLD', 'PUB_OLD')",
            params![url.clone()],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, enabled, required, local_checksum, remote_checksum, local_content_hash) VALUES (10, '@mod_old', ?, 'R:/Mods/MainRepo/@mod_old', 1, 1, 'LOCAL_ADDON', 'OLD_ADDON', 'CONTENT')",
            params![format!("{url}@mod_old")],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, length) VALUES (20, 'old.bin', ?, 'R:/Mods/MainRepo/@mod_old/old.bin', 'LOCAL_FILE', 'OLD_FILE', 4)",
            params![format!("{url}@mod_old/old.bin")],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 10)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (10, 20)",
            params![],
        )
        .await
        .unwrap();

        let result = remote_repository(
            context.clone(),
            &url,
            Some("R:/Mods/MainRepo/"),
            None,
            true,
            HashAlgorithmPreference::default(),
        )
        .await;
        assert!(result.is_none());
        server.await.unwrap();

        let addons = db
            .query_all(
                "SELECT name FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id WHERE ra.repository_id = 1",
                params![],
            )
            .await
            .unwrap();
        let names: HashSet<String> = addons
            .iter()
            .filter_map(|row| row.get_string("name").ok())
            .collect();
        assert_eq!(names, HashSet::from(["@mod_old".to_owned()]));
        let files = db
            .query_all(
                "SELECT name FROM addon_files af JOIN files f ON f.id = af.file_id WHERE af.addon_id = 10",
                params![],
            )
            .await
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].get_string("name").unwrap(), "old.bin");
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
