use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use log::{info, warn};

use crate::core::db::{DbErr, DbTxn, DbValue, FoxyDb, params};
use crate::core::models::context::FoxyContext;
use crate::core::models::pending_update::clear_pending_update_for_context;
use crate::core::tasks::create_context::create_context;
use crate::core::utils::format::{sanitize_log_path, sanitize_log_url};

fn normalize_url(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{}/", url)
    }
}

fn normalize_path_for_compare(path: &Path) -> String {
    crate::core::utils::content_hash::normalize_path(&path.to_string_lossy())
}

fn is_safe_child(base: &Path, candidate: &Path) -> bool {
    let base_norm = normalize_path_for_compare(base);
    let candidate_norm = normalize_path_for_compare(candidate);
    if base_norm.is_empty() || candidate_norm.is_empty() {
        return false;
    }
    if base_norm == candidate_norm {
        return false;
    }
    let prefix = format!("{}/", base_norm);
    candidate_norm.starts_with(&prefix)
}

fn resolve_mod_path(base_path: &Path, mod_path: &str) -> Option<PathBuf> {
    let trimmed = mod_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw = PathBuf::from(trimmed);
    if raw.is_absolute() {
        Some(raw)
    } else {
        Some(base_path.join(raw))
    }
}

fn remove_empty_directory_tree(path: &Path) -> std::io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_type = entry.file_type()?;
        if entry_type.is_dir() && !entry_type.is_symlink() {
            remove_empty_directory_tree(&entry.path())?;
        }
    }

    match fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(err) => Err(err),
    }
}

fn remove_repository_files(base_path: &str, file_paths: &[String], mod_paths: &[String]) {
    let base = Path::new(base_path);
    for p in file_paths {
        let Some(candidate) = resolve_mod_path(base, p) else {
            warn!("Skipping empty file path during purge");
            continue;
        };

        if !is_safe_child(base, &candidate) {
            warn!(
                "Skipping unsafe path removal attempt: {}",
                sanitize_log_path(&candidate)
            );
            continue;
        }

        if !candidate.exists() {
            continue;
        }

        if candidate.is_dir() {
            warn!(
                "Skipping tracked file path that resolves to a directory: {}",
                sanitize_log_path(&candidate)
            );
        } else if let Err(err) = fs::remove_file(&candidate) {
            warn!(
                "Failed to remove mod file {}: {}",
                sanitize_log_path(&candidate),
                err
            );
        } else {
            info!("Removed mod file {}", sanitize_log_path(&candidate));
        }
    }

    // Prune empty descendants bottom-up without following symlinks. Never use a
    // recursive delete here: the addon tree may still contain files owned by
    // another repository (or untracked user files).
    for p in mod_paths {
        let Some(candidate) = resolve_mod_path(base, p) else {
            continue;
        };
        if !is_safe_child(base, &candidate) || !candidate.is_dir() {
            continue;
        }
        match remove_empty_directory_tree(&candidate) {
            Ok(true) => info!(
                "Removed empty mod directory {}",
                sanitize_log_path(&candidate)
            ),
            Ok(false) => {}
            Err(err) => warn!(
                "Failed to prune empty mod directories under {}: {}",
                sanitize_log_path(&candidate),
                err
            ),
        }
    }
}

fn remove_addon_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        info!(
            "Addon directory already missing during purge: {}",
            sanitize_log_path(path)
        );
        return Ok(());
    }

    if !path.is_dir() {
        anyhow::bail!("addon path is not a directory: {}", sanitize_log_path(path));
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        anyhow::bail!("addon path has no final directory name");
    };
    if name.trim().is_empty() {
        anyhow::bail!("addon path has an empty final directory name");
    }

    fs::remove_dir_all(path)
        .map(|_| info!("Removed addon directory {}", sanitize_log_path(path)))
        .map_err(Into::into)
}

/// Execute a parameterized statement inside a seam transaction (purge helper).
async fn execute_sql(tx: &DbTxn<'_>, sql: &str, values: Vec<DbValue>) -> Result<(), DbErr> {
    tx.execute(sql, values).await?;
    Ok(())
}

/// Like [`execute_sql`] but times the statement and logs how long it took, so the
/// purge transaction is no longer a silent multi-second black box in the logs
/// (the force-redownload "~18s gap with nothing logged" report). Steps ≥50ms log
/// at INFO; faster ones at DEBUG to avoid noise on small repos.
async fn timed_step(
    tx: &DbTxn<'_>,
    label: &str,
    sql: &str,
    values: Vec<DbValue>,
) -> Result<(), DbErr> {
    let started = Instant::now();
    let affected = tx.execute(sql, values).await?;
    let elapsed = started.elapsed();
    if elapsed >= std::time::Duration::from_millis(50) {
        info!(
            "Purge step '{}' completed in {:.3}s (rows_affected={})",
            label,
            elapsed.as_secs_f64(),
            affected
        );
    } else {
        log::debug!(
            "Purge step '{}' completed in {:.3}s (rows_affected={})",
            label,
            elapsed.as_secs_f64(),
            affected
        );
    }
    Ok(())
}

async fn query_exclusive_paths(
    db: &FoxyDb,
    normalized_url: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let rows = db
        .query_all(
            "SELECT addons.local_path AS addon_path,
                    files.local_path AS file_path,
                    repositories.remote_url AS repository_url
             FROM addons
             INNER JOIN repository_addons
                ON repository_addons.addon_id = addons.id
             INNER JOIN repositories
                ON repositories.id = repository_addons.repository_id
             LEFT JOIN addon_files ON addon_files.addon_id = addons.id
             LEFT JOIN files ON files.id = addon_files.file_id",
            params![],
        )
        .await?;

    let mut target_mod_paths = std::collections::HashMap::<String, String>::new();
    let mut target_file_paths = std::collections::HashMap::<String, String>::new();
    let mut retained_mod_paths = std::collections::HashSet::<String>::new();
    let mut retained_file_paths = std::collections::HashSet::<String>::new();

    for row in &rows {
        let repository_url = row.get_string("repository_url")?;
        let addon_path = row.get_string("addon_path")?;
        let addon_key = normalize_path_for_compare(Path::new(&addon_path));
        let file_path = row.get_opt_string("file_path")?;
        if repository_url == normalized_url {
            target_mod_paths.entry(addon_key).or_insert(addon_path);
            if let Some(file_path) = file_path {
                target_file_paths
                    .entry(normalize_path_for_compare(Path::new(&file_path)))
                    .or_insert(file_path);
            }
        } else {
            retained_mod_paths.insert(addon_key);
            if let Some(file_path) = file_path {
                retained_file_paths.insert(normalize_path_for_compare(Path::new(&file_path)));
            }
        }
    }

    target_mod_paths.retain(|path, _| !retained_mod_paths.contains(path));
    target_file_paths.retain(|path, _| !retained_file_paths.contains(path));
    Ok((
        target_file_paths.into_values().collect(),
        target_mod_paths.into_values().collect(),
    ))
}

pub async fn purge_repository_by_url(repository_url: &str, repo_path: Option<&str>) -> Result<()> {
    let context = create_context().await;
    purge_repository(context, repository_url, repo_path).await
}

pub async fn purge_addon_by_local_path(addon_path: &str) -> Result<usize> {
    let context = create_context().await;
    purge_addon_by_local_path_with_context(context, addon_path).await
}

pub async fn purge_addon_by_local_path_with_context(
    context: Arc<FoxyContext>,
    addon_path: &str,
) -> Result<usize> {
    let addon_path = addon_path.trim();
    if addon_path.is_empty() {
        return Ok(0);
    }

    let target_path = PathBuf::from(addon_path);
    remove_addon_directory(&target_path)?;

    let target_key = normalize_path_for_compare(&target_path);
    let db = context.db();
    let addon_rows = db
        .query_all("SELECT id, local_path FROM addons", params![])
        .await?;
    let mut addon_ids: Vec<i64> = Vec::new();
    for row in &addon_rows {
        let local_path = row.get_string("local_path")?;
        if normalize_path_for_compare(Path::new(&local_path)) == target_key {
            addon_ids.push(row.get_i64("id")?);
        }
    }

    if addon_ids.is_empty() {
        info!(
            "Addon purge removed files but found no database rows for {}",
            sanitize_log_path(&target_path)
        );
        return Ok(0);
    }

    let deleted_count = addon_ids.len();
    let db_purge_started_at = Instant::now();
    // Exclusive: Turso (beta) hard-wedges this bulk delete when overlapped by a
    // read/write on another connection/runtime (see DB_EXCLUSIVE).
    db.transaction_exclusive("purge addon", |tx| {
        let addon_ids = addon_ids.clone();
        Box::pin(async move {
            execute_sql(
                tx,
                "CREATE TEMP TABLE IF NOT EXISTS temp.foxy_purge_addon_ids (
                    addon_id INTEGER PRIMARY KEY
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "CREATE TEMP TABLE IF NOT EXISTS temp.foxy_purge_orphan_file_ids (
                    file_id INTEGER PRIMARY KEY
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "CREATE TEMP TABLE IF NOT EXISTS temp.foxy_purge_affected_repo_ids (
                    repository_id INTEGER PRIMARY KEY
                 )",
                vec![],
            )
            .await?;

            for table in [
                "temp.foxy_purge_addon_ids",
                "temp.foxy_purge_orphan_file_ids",
                "temp.foxy_purge_affected_repo_ids",
            ] {
                execute_sql(tx, &format!("DELETE FROM {table}"), vec![]).await?;
            }

            for addon_id in addon_ids {
                execute_sql(
                    tx,
                    "INSERT OR IGNORE INTO temp.foxy_purge_addon_ids (addon_id)
                     VALUES (?)",
                    vec![addon_id.into()],
                )
                .await?;
            }

            execute_sql(
                tx,
                "INSERT OR IGNORE INTO temp.foxy_purge_affected_repo_ids (repository_id)
                 SELECT repository_addons.repository_id
                 FROM repository_addons
                 INNER JOIN temp.foxy_purge_addon_ids
                    ON temp.foxy_purge_addon_ids.addon_id = repository_addons.addon_id",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "INSERT OR IGNORE INTO temp.foxy_purge_orphan_file_ids (file_id)
                 SELECT addon_files.file_id
                 FROM addon_files
                 INNER JOIN temp.foxy_purge_addon_ids
                    ON temp.foxy_purge_addon_ids.addon_id = addon_files.addon_id
                 WHERE NOT EXISTS (
                     SELECT 1
                     FROM addon_files AS retained_addon_files
                     WHERE retained_addon_files.file_id = addon_files.file_id
                       AND retained_addon_files.addon_id NOT IN (
                           SELECT addon_id
                           FROM temp.foxy_purge_addon_ids
                       )
                 )",
                vec![],
            )
            .await?;

            timed_step(
                tx,
                "delete download_target_file_part",
                "DELETE FROM download_target_file_part
                 WHERE subfile_id IN (
                     SELECT id
                     FROM subfiles
                     WHERE file_id IN (
                         SELECT file_id
                         FROM temp.foxy_purge_orphan_file_ids
                     )
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete subfiles",
                "DELETE FROM subfiles
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete download_patch_op",
                "DELETE FROM download_patch_op
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete download_patch_file",
                "DELETE FROM download_patch_file
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete download_target_file",
                "DELETE FROM download_target_file
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "DELETE FROM addon_files
                 WHERE addon_id IN (
                     SELECT addon_id
                     FROM temp.foxy_purge_addon_ids
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "DELETE FROM repository_addons
                 WHERE addon_id IN (
                     SELECT addon_id
                     FROM temp.foxy_purge_addon_ids
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "DELETE FROM addons
                 WHERE id IN (
                     SELECT addon_id
                     FROM temp.foxy_purge_addon_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete files",
                "DELETE FROM files
                 WHERE id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "UPDATE repositories
                 SET local_checksum = '',
                     remote_checksum = '',
                     local_content_hash = ''
                 WHERE id IN (
                     SELECT repository_id
                     FROM temp.foxy_purge_affected_repo_ids
                 )",
                vec![],
            )
            .await?;

            Ok(())
        })
    })
    .await?;

    let _ = context
        .db()
        .execute("PRAGMA wal_checkpoint(PASSIVE)", params![])
        .await;
    info!(
        "Addon purge completed for {} in {:.2}s (addons={})",
        sanitize_log_path(&target_path),
        db_purge_started_at.elapsed().as_secs_f64(),
        deleted_count
    );

    Ok(deleted_count)
}

pub async fn purge_repository_db_only_by_url(repository_url: &str) -> Result<()> {
    let context = create_context().await;
    purge_repository_internal(context, repository_url, None, false, None).await
}

/// Wipe cached database rows for a SINGLE repository instance, identified by its
/// remote URL *and* local download folder. Sibling repositories that share the
/// same remote URL but live in a different folder (an independent install, or a
/// different entry in another repository space) are left untouched.
pub async fn purge_repository_db_only_by_url_and_path(
    repository_url: &str,
    local_path: &str,
) -> Result<()> {
    let context = create_context().await;
    purge_repository_internal(context, repository_url, None, false, Some(local_path)).await
}

pub async fn purge_repository(
    context: Arc<FoxyContext>,
    repository_url: &str,
    repo_path: Option<&str>,
) -> Result<()> {
    purge_repository_internal(context, repository_url, repo_path, true, None).await
}

/// Purge exactly one installed repository instance, identified by its remote URL
/// and local folder. This is used by force redownload inside the sync pipeline so
/// the following remote refresh/download sees the same missing-file state as the
/// old pre-Turso purge-first flow.
pub async fn purge_repository_instance(
    context: Arc<FoxyContext>,
    repository_url: &str,
    local_path: &str,
) -> Result<()> {
    purge_repository_internal(
        context,
        repository_url,
        Some(local_path),
        true,
        Some(local_path),
    )
    .await
}

async fn purge_repository_internal(
    context: Arc<FoxyContext>,
    repository_url: &str,
    repo_path: Option<&str>,
    remove_local_files: bool,
    scope_local_path: Option<&str>,
) -> Result<()> {
    let normalized_url = normalize_url(repository_url);
    info!(
        "Purging repository data for {}",
        sanitize_log_url(&normalized_url)
    );

    let db = context.db();
    // (id, local_path) per repository instance for this URL, ordered by id.
    let repositories: Vec<(i64, String)> = db
        .query_all(
            "SELECT id, local_path FROM repositories WHERE remote_url = ? ORDER BY id ASC",
            params![normalized_url.clone()],
        )
        .await?
        .iter()
        .map(|row| Ok::<_, DbErr>((row.get_i64("id")?, row.get_string("local_path")?)))
        .collect::<Result<_, DbErr>>()?;
    if repositories.is_empty() {
        info!(
            "Purge skipped: repository {} not found in local database",
            sanitize_log_url(&normalized_url)
        );
        clear_pending_update_for_context(context, &normalized_url)
            .await
            .ok();
        return Ok(());
    }

    // Restrict the purge to a single repository instance when a local path is
    // given. Two repositories can share one remote URL while pointing at
    // different download folders; wiping by URL alone would destroy the sibling's
    // cached addon/file/part rows (and its computed local hashes).
    let scoped_repo_ids: Vec<i64> = match scope_local_path {
        Some(path) => {
            let target_key = normalize_path_for_compare(Path::new(path));
            let ids: Vec<i64> = repositories
                .iter()
                .filter(|(_, local_path)| {
                    normalize_path_for_compare(Path::new(local_path)) == target_key
                })
                .map(|(id, _)| *id)
                .collect();
            if ids.is_empty() {
                info!(
                    "Purge skipped: repository {} has no cached instance at the requested local path",
                    sanitize_log_url(&normalized_url)
                );
                return Ok(());
            }
            ids
        }
        None => repositories.iter().map(|(id, _)| *id).collect(),
    };

    let repo_local_path = repositories[0].1.clone();
    let (file_paths, mod_paths) = if remove_local_files {
        query_exclusive_paths(&db, &normalized_url).await?
    } else {
        (Vec::new(), Vec::new())
    };

    if remove_local_files
        && let Some(base) =
            repo_path.or_else(|| (!repo_local_path.is_empty()).then_some(repo_local_path.as_str()))
    {
        remove_repository_files(base, &file_paths, &mod_paths);
    }

    // Whole-table-wipe fast path (analysis4 P1-a): when the scoped repositories
    // are *every* repository in the database, all addons/files/subfiles become
    // orphaned, so the per-row `… WHERE … IN (SELECT … FROM temp.orphan_ids)`
    // deletes can be replaced by unqualified `DELETE FROM <table>`. That skips
    // the 66k-row `subfiles` subquery materialization (the production purge's two
    // dominant steps - `delete download_target_file_part` 13.4s over zero rows
    // and `delete subfiles` 14.9s - are both that scan). Strictly gated on
    // "these are all the repos" so a sibling repo's rows are never wiped.
    let total_repo_count: i64 = db
        .query_all("SELECT COUNT(*) AS count FROM repositories", vec![])
        .await?
        .first()
        .map(|row| row.get_i64("count"))
        .transpose()?
        .unwrap_or(0);
    let whole_wipe = total_repo_count > 0 && scoped_repo_ids.len() as i64 == total_repo_count;

    info!(
        "Repository purge: starting DB transaction for {} (repo_instances={}, whole_wipe={})",
        sanitize_log_url(&normalized_url),
        scoped_repo_ids.len(),
        whole_wipe
    );
    let db_purge_started_at = Instant::now();
    // Exclusive: a force-redownload purge holds this ~17s bulk-delete transaction
    // (66k+ subfiles) and Turso (beta) hard-wedges if any read/write on another
    // connection/runtime overlaps it - the production force-redownload hang. The
    // write gate only serializes writers; the UI list-cache pending-update
    // workers and quick scans issue ungated reads on their own runtimes. See
    // DB_EXCLUSIVE and `repro_purge_wedge_under_concurrency`.
    db.transaction_exclusive("purge repository", |tx| {
        let scoped_repo_ids = scoped_repo_ids.clone();
        Box::pin(async move {
            if whole_wipe {
                // Child-first order so the no-cascade `subfiles → files` FK (and
                // the others) never blocks the delete. download_target_file* have
                // no FK; the rest cascade, but we delete them explicitly anyway.
                // `subfiles` is special-cased to DROP+recreate (P0-a) and must come
                // AFTER download_target_file_part is emptied (its rows reference
                // subfiles.id) and BEFORE `files` (subfiles' FK target).
                for (label, table) in [
                    (
                        "delete download_target_file_part",
                        "download_target_file_part",
                    ),
                    ("delete subfiles", "subfiles"),
                    ("delete download_patch_op", "download_patch_op"),
                    ("delete download_patch_file", "download_patch_file"),
                    ("delete download_target_file", "download_target_file"),
                    ("delete addon_files", "addon_files"),
                    ("delete repository_addons", "repository_addons"),
                    ("delete files", "files"),
                    ("delete addons", "addons"),
                    ("delete repositories", "repositories"),
                ] {
                    if table == "subfiles" {
                        // P0-a (after_turso_regression_analysis5.md): a 66k-row
                        // `DELETE FROM subfiles` costs ~9s on TFR_40K even on a
                        // freshly-compacted file - intrinsic Turso ~0.14ms/row over
                        // 4 B-trees. Since a whole wipe is dropping every repo, the
                        // table can be DROPped (O(1) page dealloc) and recreated
                        // empty instead. Recreated WITH all indexes so the table is
                        // always in a consistent shape even if no rebuild follows;
                        // the deferred-index bulk load (P0-b) manages its own
                        // drop/rebuild around the subsequent insert.
                        timed_step(tx, "drop subfiles", "DROP TABLE IF EXISTS subfiles", vec![])
                            .await?;
                        execute_sql(
                            tx,
                            crate::core::tasks::db_turso::SUBFILES_CREATE_TABLE,
                            vec![],
                        )
                        .await?;
                        for idx_sql in crate::core::tasks::db_turso::SUBFILES_INDEX_CREATE_SQL {
                            execute_sql(tx, idx_sql, vec![]).await?;
                        }
                        continue;
                    }
                    timed_step(tx, label, &format!("DELETE FROM {table}"), vec![]).await?;
                }
                return Ok(());
            }
            execute_sql(
                tx,
                "CREATE TEMP TABLE IF NOT EXISTS temp.foxy_purge_repo_ids (
                    id INTEGER PRIMARY KEY
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "CREATE TEMP TABLE IF NOT EXISTS temp.foxy_purge_addon_ids (
                    addon_id INTEGER PRIMARY KEY
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "CREATE TEMP TABLE IF NOT EXISTS temp.foxy_purge_orphan_addon_ids (
                    addon_id INTEGER PRIMARY KEY
                 )",
                vec![],
            )
            .await?;
            execute_sql(
                tx,
                "CREATE TEMP TABLE IF NOT EXISTS temp.foxy_purge_orphan_file_ids (
                    file_id INTEGER PRIMARY KEY
                 )",
                vec![],
            )
            .await?;

            for table in [
                "temp.foxy_purge_repo_ids",
                "temp.foxy_purge_addon_ids",
                "temp.foxy_purge_orphan_addon_ids",
                "temp.foxy_purge_orphan_file_ids",
            ] {
                execute_sql(tx, &format!("DELETE FROM {table}"), vec![]).await?;
            }

            for repo_id in &scoped_repo_ids {
                execute_sql(
                    tx,
                    "INSERT OR IGNORE INTO temp.foxy_purge_repo_ids (id)
                     VALUES (?)",
                    vec![(*repo_id).into()],
                )
                .await?;
            }
            timed_step(
                tx,
                "collect addon ids",
                "INSERT OR IGNORE INTO temp.foxy_purge_addon_ids (addon_id)
                 SELECT repository_addons.addon_id
                 FROM repository_addons
                 INNER JOIN temp.foxy_purge_repo_ids
                    ON temp.foxy_purge_repo_ids.id = repository_addons.repository_id",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "collect orphan addon ids",
                "INSERT OR IGNORE INTO temp.foxy_purge_orphan_addon_ids (addon_id)
                 SELECT addon_id
                 FROM temp.foxy_purge_addon_ids
                 WHERE NOT EXISTS (
                     SELECT 1
                     FROM repository_addons AS other_repository_addons
                     WHERE other_repository_addons.addon_id =
                           temp.foxy_purge_addon_ids.addon_id
                       AND other_repository_addons.repository_id NOT IN (
                           SELECT id
                           FROM temp.foxy_purge_repo_ids
                       )
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "collect orphan file ids",
                "INSERT OR IGNORE INTO temp.foxy_purge_orphan_file_ids (file_id)
                 SELECT addon_files.file_id
                 FROM addon_files
                 INNER JOIN temp.foxy_purge_orphan_addon_ids
                    ON temp.foxy_purge_orphan_addon_ids.addon_id = addon_files.addon_id
                 WHERE NOT EXISTS (
                     SELECT 1
                     FROM addon_files AS retained_addon_files
                     WHERE retained_addon_files.file_id = addon_files.file_id
                       AND retained_addon_files.addon_id NOT IN (
                           SELECT addon_id
                           FROM temp.foxy_purge_orphan_addon_ids
                       )
                 )",
                vec![],
            )
            .await?;

            timed_step(
                tx,
                "delete repositories",
                "DELETE FROM repositories
                 WHERE id IN (SELECT id FROM temp.foxy_purge_repo_ids)",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete download_target_file_part",
                "DELETE FROM download_target_file_part
                 WHERE subfile_id IN (
                     SELECT id
                     FROM subfiles
                     WHERE file_id IN (
                         SELECT file_id
                         FROM temp.foxy_purge_orphan_file_ids
                     )
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete subfiles",
                "DELETE FROM subfiles
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete download_patch_op",
                "DELETE FROM download_patch_op
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete download_patch_file",
                "DELETE FROM download_patch_file
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete download_target_file",
                "DELETE FROM download_target_file
                 WHERE file_id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete addon_files",
                "DELETE FROM addon_files
                 WHERE addon_id IN (
                     SELECT addon_id
                     FROM temp.foxy_purge_orphan_addon_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete addons",
                "DELETE FROM addons
                 WHERE id IN (
                     SELECT addon_id
                     FROM temp.foxy_purge_orphan_addon_ids
                 )",
                vec![],
            )
            .await?;
            timed_step(
                tx,
                "delete files",
                "DELETE FROM files
                 WHERE id IN (
                     SELECT file_id
                     FROM temp.foxy_purge_orphan_file_ids
                 )",
                vec![],
            )
            .await?;

            Ok(())
        })
    })
    .await?;

    let txn_elapsed = db_purge_started_at.elapsed();
    info!(
        "Repository purge: DB transaction committed for {} in {:.2}s",
        sanitize_log_url(&normalized_url),
        txn_elapsed.as_secs_f64()
    );

    // Checkpoint WAL after the large bulk delete to prevent unbounded WAL growth.
    let checkpoint_started = Instant::now();
    let _ = context
        .db()
        .execute("PRAGMA wal_checkpoint(PASSIVE)", params![])
        .await;
    let checkpoint_elapsed = checkpoint_started.elapsed();
    if checkpoint_elapsed >= std::time::Duration::from_millis(50) {
        info!(
            "Repository purge: WAL checkpoint completed in {:.2}s",
            checkpoint_elapsed.as_secs_f64()
        );
    }

    clear_pending_update_for_context(context, &normalized_url)
        .await
        .ok();
    info!(
        "Repository purge completed for {} in {:.2}s (txn={:.2}s, checkpoint={:.2}s)",
        sanitize_log_url(&normalized_url),
        db_purge_started_at.elapsed().as_secs_f64(),
        txn_elapsed.as_secs_f64(),
        checkpoint_elapsed.as_secs_f64()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Statement runner used to seed the Turso test databases (the seam's typed
    /// params; replaces the old SeaORM `execute_sql`). Each call takes a fresh
    /// tuned connection, which is enough for the autocommit seeding here.
    async fn execute_sql(
        db: &turso::Database,
        sql: &str,
        values: Vec<DbValue>,
    ) -> Result<(), DbErr> {
        let conn = crate::core::tasks::db_turso::connect_tuned(db)
            .await
            .map_err(|e| DbErr::Custom(format!("turso: {e}")))?;
        let tvals: Vec<turso::Value> = values.into_iter().map(DbValue::into_turso_value).collect();
        conn.execute(sql, tvals)
            .await
            .map_err(|e| DbErr::Custom(format!("turso: {e}")))?;
        Ok(())
    }

    async fn create_test_db() -> (tempfile::TempDir, Arc<turso::Database>) {
        // The temp dir holds the addon file tree; the database file is managed
        // (and leaked) by `build_test_database`.
        let temp = tempfile::tempdir().expect("temp dir");
        let db = crate::core::tasks::db_turso::build_test_database().await;
        (temp, db)
    }

    async fn count_rows(db: &turso::Database, table: &str, predicate: &str) -> i64 {
        let conn = crate::core::tasks::db_turso::connect_tuned(db)
            .await
            .expect("connect for count");
        let sql = format!("SELECT COUNT(*) AS count FROM {table} WHERE {predicate}");
        let mut rows = conn.query(&sql, ()).await.expect("count query");
        let row = rows.next().await.expect("count step").expect("count row");
        row.get::<i64>(0).expect("count value")
    }

    #[tokio::test]
    async fn purge_repository_preserves_shared_addons_and_files() {
        let (temp, db) = create_test_db().await;
        let local_root = temp.path().join("mods");
        let orphan_dir = local_root.join("@orphan");
        let shared_dir = local_root.join("@shared");
        std::fs::create_dir_all(&orphan_dir).expect("create orphan addon dir");
        std::fs::create_dir_all(&shared_dir).expect("create shared addon dir");

        let repo_a_url = "https://example.invalid/repo-a/";
        let repo_b_url = "https://example.invalid/repo-b/";
        execute_sql(
            db.as_ref(),
            "INSERT INTO repositories
             (id, name, remote_url, local_path, image, local_checksum, remote_checksum,
              local_content_hash, foxy_mode)
             VALUES
             (1, 'Repo A', ?, ?, '', '', '', '', ''),
             (2, 'Repo B', ?, ?, '', '', '', '', '')",
            vec![
                repo_a_url.into(),
                local_root.to_string_lossy().to_string().into(),
                repo_b_url.into(),
                local_root.to_string_lossy().to_string().into(),
            ],
        )
        .await
        .expect("insert repositories");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addons
             (id, name, display_name, remote_path, local_path, client_side, enabled,
              local_checksum, remote_checksum, local_content_hash, required, data_order)
             VALUES
             (11, '@orphan', '', 'remote/orphan', '@orphan', 0, 1, '', '', '', 1, 0),
             (12, '@shared', '', 'remote/shared', '@shared', 0, 1, '', '', '', 1, 1)",
            vec![],
        )
        .await
        .expect("insert addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO repository_addons (repository_id, addon_id)
             VALUES (1, 11), (1, 12), (2, 12)",
            vec![],
        )
        .await
        .expect("insert repository addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO files
             (id, name, remote_path, local_path, local_checksum, remote_checksum,
              local_content_hash, length, data_order)
             VALUES
             (21, 'orphan.pbo', 'remote/orphan.pbo', 'local/orphan.pbo', '', '', '', 1, 0),
             (22, 'shared.pbo', 'remote/shared.pbo', 'local/shared.pbo', '', '', '', 1, 1)",
            vec![],
        )
        .await
        .expect("insert files");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addon_files (addon_id, file_id)
             VALUES (11, 21), (12, 22)",
            vec![],
        )
        .await
        .expect("insert addon files");
        execute_sql(
            db.as_ref(),
            "INSERT INTO subfiles
             (id, file_id, path, local_length, local_start, remote_length, remote_start,
              local_checksum, remote_checksum, data_order)
             VALUES
             (31, 21, 'orphan.part', 1, 0, 1, 0, '', '', 0),
             (32, 22, 'shared.part', 1, 0, 1, 0, '', '', 0)",
            vec![],
        )
        .await
        .expect("insert file parts");
        execute_sql(
            db.as_ref(),
            "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size)
             VALUES (21, 'remote/orphan.pbo', 'local/orphan.pbo', 1),
                    (22, 'remote/shared.pbo', 'local/shared.pbo', 1)",
            vec![],
        )
        .await
        .expect("insert download targets");
        execute_sql(
            db.as_ref(),
            "INSERT INTO download_target_file_part
             (subfile_id, download_remote_url, download_local_path, size, offset)
             VALUES (31, 'remote/orphan.part', 'local/orphan.part', 1, 0),
                    (32, 'remote/shared.part', 'local/shared.part', 1, 0)",
            vec![],
        )
        .await
        .expect("insert download target parts");

        let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
        purge_repository(context, repo_a_url, None)
            .await
            .expect("purge repo A");

        assert_eq!(count_rows(db.as_ref(), "repositories", "id = 1").await, 0);
        assert_eq!(count_rows(db.as_ref(), "repositories", "id = 2").await, 1);
        assert_eq!(count_rows(db.as_ref(), "addons", "id = 11").await, 0);
        assert_eq!(count_rows(db.as_ref(), "addons", "id = 12").await, 1);
        assert_eq!(count_rows(db.as_ref(), "files", "id = 21").await, 0);
        assert_eq!(count_rows(db.as_ref(), "files", "id = 22").await, 1);
        assert_eq!(count_rows(db.as_ref(), "subfiles", "id = 31").await, 0);
        assert_eq!(count_rows(db.as_ref(), "subfiles", "id = 32").await, 1);
        assert_eq!(
            count_rows(db.as_ref(), "download_target_file", "file_id = 21").await,
            0
        );
        assert_eq!(
            count_rows(db.as_ref(), "download_target_file", "file_id = 22").await,
            1
        );
        assert_eq!(
            count_rows(db.as_ref(), "download_target_file_part", "subfile_id = 31").await,
            0
        );
        assert_eq!(
            count_rows(db.as_ref(), "download_target_file_part", "subfile_id = 32").await,
            1
        );
        assert_eq!(
            count_rows(
                db.as_ref(),
                "repository_addons",
                "repository_id = 2 AND addon_id = 12"
            )
            .await,
            1
        );
        assert!(!orphan_dir.exists());
        assert!(shared_dir.exists());
    }

    #[tokio::test]
    async fn purge_repository_whole_wipe_when_sole_repo_clears_all_tables() {
        // When the purged repo is the ONLY repo in the database, the whole-wipe
        // fast path (analysis4 P1-a) runs unqualified `DELETE FROM <table>` and
        // must clear every graph table - equivalent to the scoped path but without
        // the 66k-row subfiles subquery scans.
        let (temp, db) = create_test_db().await;
        let local_root = temp.path().join("mods");
        std::fs::create_dir_all(local_root.join("@only")).expect("create addon dir");

        let url = "https://example.invalid/only/";
        execute_sql(
            db.as_ref(),
            "INSERT INTO repositories
             (id, name, remote_url, local_path, image, local_checksum, remote_checksum,
              local_content_hash, foxy_mode)
             VALUES (1, 'Only', ?, ?, '', '', '', '', '')",
            vec![url.into(), local_root.to_string_lossy().to_string().into()],
        )
        .await
        .expect("insert repository");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addons
             (id, name, display_name, remote_path, local_path, client_side, enabled,
              local_checksum, remote_checksum, local_content_hash, required, data_order)
             VALUES (11, '@only', '', 'remote/only', '@only', 0, 1, '', '', '', 1, 0)",
            vec![],
        )
        .await
        .expect("insert addon");
        execute_sql(
            db.as_ref(),
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 11)",
            vec![],
        )
        .await
        .expect("insert repository addon");
        execute_sql(
            db.as_ref(),
            "INSERT INTO files
             (id, name, remote_path, local_path, local_checksum, remote_checksum,
              local_content_hash, length, data_order)
             VALUES (21, 'only.pbo', 'remote/only.pbo', 'local/only.pbo', '', '', '', 1, 0)",
            vec![],
        )
        .await
        .expect("insert file");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addon_files (addon_id, file_id) VALUES (11, 21)",
            vec![],
        )
        .await
        .expect("insert addon file");
        execute_sql(
            db.as_ref(),
            "INSERT INTO subfiles
             (id, file_id, path, local_length, local_start, remote_length, remote_start,
              local_checksum, remote_checksum, data_order)
             VALUES (31, 21, 'only.part', 1, 0, 1, 0, '', '', 0)",
            vec![],
        )
        .await
        .expect("insert subfile");
        execute_sql(
            db.as_ref(),
            "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size)
             VALUES (21, 'remote/only.pbo', 'local/only.pbo', 1)",
            vec![],
        )
        .await
        .expect("insert download target");
        execute_sql(
            db.as_ref(),
            "INSERT INTO download_target_file_part
             (subfile_id, download_remote_url, download_local_path, size, offset)
             VALUES (31, 'remote/only.part', 'local/only.part', 1, 0)",
            vec![],
        )
        .await
        .expect("insert download target part");

        let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
        purge_repository(context, url, None)
            .await
            .expect("purge sole repo");

        for table in [
            "repositories",
            "addons",
            "repository_addons",
            "files",
            "addon_files",
            "subfiles",
            "download_target_file",
            "download_target_file_part",
        ] {
            assert_eq!(
                count_rows(db.as_ref(), table, "1 = 1").await,
                0,
                "whole-wipe should clear {table}"
            );
        }

        // P0-a: subfiles was DROP+recreated, not DELETEd. Confirm it is a usable,
        // empty table whose `(file_id, path)` unique index was restored - a second
        // insert of the same (file_id, path) must be rejected, so the post-purge
        // `ON CONFLICT` upsert path still resolves against the index.
        execute_sql(
            db.as_ref(),
            "INSERT INTO files
             (id, name, remote_path, local_path, local_checksum, remote_checksum,
              local_content_hash, length, data_order)
             VALUES (21, 'only.pbo', 'remote/only.pbo', 'local/only.pbo', '', '', '', 1, 0)",
            vec![],
        )
        .await
        .expect("re-insert file after wipe");
        execute_sql(
            db.as_ref(),
            "INSERT INTO subfiles
             (file_id, path, local_length, local_start, remote_length, remote_start,
              local_checksum, remote_checksum, data_order)
             VALUES (21, 'only.part', 0, 0, 1, 0, '', '', 0)",
            vec![],
        )
        .await
        .expect("insert into recreated subfiles");
        let dup = execute_sql(
            db.as_ref(),
            "INSERT INTO subfiles
             (file_id, path, local_length, local_start, remote_length, remote_start,
              local_checksum, remote_checksum, data_order)
             VALUES (21, 'only.part', 0, 0, 1, 0, '', '', 0)",
            vec![],
        )
        .await;
        assert!(
            dup.is_err(),
            "recreated subfiles must enforce the (file_id, path) unique index"
        );
    }

    #[tokio::test]
    async fn purge_repository_preserves_same_physical_paths_with_distinct_database_rows() {
        let (temp, db) = create_test_db().await;
        let local_root = temp.path().join("mods");
        let addon_dir = local_root.join("@shared");
        std::fs::create_dir_all(&addon_dir).expect("create shared addon dir");
        let exclusive_file = addon_dir.join("repo-a.pbo");
        let shared_file = addon_dir.join("shared.pbo");
        std::fs::write(&exclusive_file, b"repo a").expect("write exclusive file");
        std::fs::write(&shared_file, b"shared").expect("write shared file");

        let repo_a_url = "https://example.invalid/repo-a/";
        let repo_b_url = "https://example.invalid/repo-b/";
        execute_sql(
            db.as_ref(),
            "INSERT INTO repositories
             (id, name, remote_url, local_path, image, local_checksum, remote_checksum,
              local_content_hash, foxy_mode)
             VALUES (1, 'Repo A', ?, ?, '', '', '', '', ''),
                    (2, 'Repo B', ?, ?, '', '', '', '', '')",
            vec![
                repo_a_url.into(),
                local_root.to_string_lossy().to_string().into(),
                repo_b_url.into(),
                local_root.to_string_lossy().to_string().into(),
            ],
        )
        .await
        .expect("insert repositories");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addons
             (id, name, display_name, remote_path, local_path, client_side, enabled,
              local_checksum, remote_checksum, local_content_hash, required, data_order)
             VALUES (11, '@shared-a', '', 'remote/a', ?, 0, 1, '', '', '', 1, 0),
                    (12, '@shared-b', '', 'remote/b', ?, 0, 1, '', '', '', 1, 0)",
            vec![
                addon_dir.to_string_lossy().to_string().into(),
                addon_dir.to_string_lossy().to_string().into(),
            ],
        )
        .await
        .expect("insert addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO repository_addons (repository_id, addon_id)
             VALUES (1, 11), (2, 12)",
            vec![],
        )
        .await
        .expect("insert repository addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO files
             (id, name, remote_path, local_path, local_checksum, remote_checksum,
              local_content_hash, length, data_order)
             VALUES (21, 'repo-a.pbo', 'remote/a/repo-a.pbo', ?, '', '', '', 1, 0),
                    (22, 'shared-a.pbo', 'remote/a/shared.pbo', ?, '', '', '', 1, 1),
                    (23, 'shared-b.pbo', 'remote/b/shared.pbo', ?, '', '', '', 1, 0)",
            vec![
                exclusive_file.to_string_lossy().to_string().into(),
                shared_file.to_string_lossy().to_string().into(),
                shared_file.to_string_lossy().to_string().into(),
            ],
        )
        .await
        .expect("insert files");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addon_files (addon_id, file_id)
             VALUES (11, 21), (11, 22), (12, 23)",
            vec![],
        )
        .await
        .expect("insert addon files");

        let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
        purge_repository(context, repo_a_url, Some(&local_root.to_string_lossy()))
            .await
            .expect("purge repo A");

        assert!(!exclusive_file.exists());
        assert!(shared_file.exists());
        assert!(addon_dir.exists());
        assert_eq!(count_rows(db.as_ref(), "repositories", "id = 2").await, 1);
        assert_eq!(count_rows(db.as_ref(), "addons", "id = 12").await, 1);
        assert_eq!(count_rows(db.as_ref(), "files", "id = 23").await, 1);
    }

    #[tokio::test]
    async fn purge_db_only_scoped_preserves_sibling_with_same_url_other_folder() {
        // Two repositories share one remote URL but download into different
        // folders (e.g. an independent install plus the same repo inside a
        // repository space). Each instance owns its own addon/file/part rows
        // because those are keyed by local path. A path-scoped DB wipe of the
        // first instance must not touch the second.
        let (temp, db) = create_test_db().await;
        let folder_a = temp.path().join("install_a");
        let folder_b = temp.path().join("install_b");
        let shared_url = "https://example.invalid/repo/";

        // The bootstrap schema already keys repositories on the composite
        // (remote_url, local_path) identity, so one URL can have several folder
        // instances without any table rebuild.
        execute_sql(
            db.as_ref(),
            "INSERT INTO repositories
             (id, name, remote_url, local_path, image, local_checksum, remote_checksum,
              local_content_hash, foxy_mode)
             VALUES
             (1, 'Install A', ?, ?, '', 'LOCAL_A', 'REMOTE', 'CONTENT_A', ''),
             (2, 'Install B', ?, ?, '', 'LOCAL_B', 'REMOTE', 'CONTENT_B', '')",
            vec![
                shared_url.into(),
                folder_a.to_string_lossy().to_string().into(),
                shared_url.into(),
                folder_b.to_string_lossy().to_string().into(),
            ],
        )
        .await
        .expect("insert repositories");
        // Per-instance addon rows (distinct local_path per folder).
        execute_sql(
            db.as_ref(),
            "INSERT INTO addons
             (id, name, display_name, remote_path, local_path, client_side, enabled,
              local_checksum, remote_checksum, local_content_hash, required, data_order)
             VALUES
             (11, '@m', '', 'remote/@m', ?, 0, 1, 'A', 'R', 'CA', 1, 0),
             (12, '@m', '', 'remote/@m', ?, 0, 1, 'B', 'R', 'CB', 1, 0)",
            vec![
                folder_a.join("@m").to_string_lossy().to_string().into(),
                folder_b.join("@m").to_string_lossy().to_string().into(),
            ],
        )
        .await
        .expect("insert addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO repository_addons (repository_id, addon_id)
             VALUES (1, 11), (2, 12)",
            vec![],
        )
        .await
        .expect("insert repository addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO files
             (id, name, remote_path, local_path, local_checksum, remote_checksum,
              local_content_hash, length, data_order)
             VALUES
             (21, 'm.pbo', 'remote/@m/m.pbo', 'a/@m/m.pbo', 'A', 'R', 'CA', 1, 0),
             (22, 'm.pbo', 'remote/@m/m.pbo', 'b/@m/m.pbo', 'B', 'R', 'CB', 1, 0)",
            vec![],
        )
        .await
        .expect("insert files");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addon_files (addon_id, file_id)
             VALUES (11, 21), (12, 22)",
            vec![],
        )
        .await
        .expect("insert addon files");
        execute_sql(
            db.as_ref(),
            "INSERT INTO subfiles
             (id, file_id, path, local_length, local_start, remote_length, remote_start,
              local_checksum, remote_checksum, data_order)
             VALUES
             (31, 21, 'm.part', 1, 0, 1, 0, 'A', 'R', 0),
             (32, 22, 'm.part', 1, 0, 1, 0, 'B', 'R', 0)",
            vec![],
        )
        .await
        .expect("insert file parts");

        let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
        purge_repository_internal(
            context,
            shared_url,
            None,
            false,
            Some(&folder_a.to_string_lossy()),
        )
        .await
        .expect("scoped purge of install A");

        // Install A's rows are gone…
        assert_eq!(count_rows(db.as_ref(), "repositories", "id = 1").await, 0);
        assert_eq!(count_rows(db.as_ref(), "addons", "id = 11").await, 0);
        assert_eq!(count_rows(db.as_ref(), "files", "id = 21").await, 0);
        assert_eq!(count_rows(db.as_ref(), "subfiles", "id = 31").await, 0);
        // …while the sibling install B (same URL, other folder) is untouched,
        // including its computed local hashes.
        assert_eq!(count_rows(db.as_ref(), "repositories", "id = 2").await, 1);
        assert_eq!(count_rows(db.as_ref(), "addons", "id = 12").await, 1);
        assert_eq!(count_rows(db.as_ref(), "files", "id = 22").await, 1);
        assert_eq!(count_rows(db.as_ref(), "subfiles", "id = 32").await, 1);
        let repo_b = FoxyDb::from_turso(db.clone())
            .query_one(
                "SELECT local_checksum, local_content_hash FROM repositories WHERE id = 2",
                params![],
            )
            .await
            .expect("load repo B")
            .expect("repo B should remain");
        assert_eq!(repo_b.get_string("local_checksum").unwrap(), "LOCAL_B");
        assert_eq!(
            repo_b.get_string("local_content_hash").unwrap(),
            "CONTENT_B"
        );
    }

    // ── normalize_url ───────────────────────────────────────────────────

    #[tokio::test]
    async fn purge_addon_removes_selected_addon_and_orphan_files_only() {
        let (temp, db) = create_test_db().await;
        let local_root = temp.path().join("mods");
        let target_dir = local_root.join("@target");
        let sibling_dir = local_root.join("@sibling");
        std::fs::create_dir_all(&target_dir).expect("create target addon dir");
        std::fs::create_dir_all(&sibling_dir).expect("create sibling addon dir");
        std::fs::write(target_dir.join("config.cpp"), b"class CfgMods {};").expect("write addon");

        execute_sql(
            db.as_ref(),
            "INSERT INTO repositories
             (id, name, remote_url, local_path, image, local_checksum, remote_checksum,
              local_content_hash, foxy_mode)
             VALUES
             (1, 'Repo A', 'https://example.invalid/repo-a/', ?, '', 'LOCAL', 'REMOTE', 'CONTENT', '')",
            vec![local_root.to_string_lossy().to_string().into()],
        )
        .await
        .expect("insert repository");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addons
             (id, name, display_name, remote_path, local_path, client_side, enabled,
              local_checksum, remote_checksum, local_content_hash, required, data_order)
             VALUES
             (11, '@target', '', 'remote/target', ?, 0, 1, '', '', '', 1, 0),
             (12, '@sibling', '', 'remote/sibling', ?, 0, 1, '', '', '', 1, 1)",
            vec![
                target_dir.to_string_lossy().to_string().into(),
                sibling_dir.to_string_lossy().to_string().into(),
            ],
        )
        .await
        .expect("insert addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO repository_addons (repository_id, addon_id)
             VALUES (1, 11), (1, 12)",
            vec![],
        )
        .await
        .expect("insert repository addons");
        execute_sql(
            db.as_ref(),
            "INSERT INTO files
             (id, name, remote_path, local_path, local_checksum, remote_checksum,
              local_content_hash, length, data_order)
             VALUES
             (21, 'target.pbo', 'remote/target.pbo', 'local/target.pbo', '', '', '', 1, 0),
             (22, 'shared.pbo', 'remote/shared.pbo', 'local/shared.pbo', '', '', '', 1, 1)",
            vec![],
        )
        .await
        .expect("insert files");
        execute_sql(
            db.as_ref(),
            "INSERT INTO addon_files (addon_id, file_id)
             VALUES (11, 21), (11, 22), (12, 22)",
            vec![],
        )
        .await
        .expect("insert addon files");
        execute_sql(
            db.as_ref(),
            "INSERT INTO subfiles
             (id, file_id, path, local_length, local_start, remote_length, remote_start,
              local_checksum, remote_checksum, data_order)
             VALUES
             (31, 21, 'target.part', 1, 0, 1, 0, '', '', 0),
             (32, 22, 'shared.part', 1, 0, 1, 0, '', '', 0)",
            vec![],
        )
        .await
        .expect("insert file parts");

        let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
        let deleted =
            purge_addon_by_local_path_with_context(context, &target_dir.to_string_lossy())
                .await
                .expect("purge addon");

        assert_eq!(deleted, 1);
        assert_eq!(count_rows(db.as_ref(), "addons", "id = 11").await, 0);
        assert_eq!(count_rows(db.as_ref(), "addons", "id = 12").await, 1);
        assert_eq!(
            count_rows(db.as_ref(), "repository_addons", "addon_id = 11").await,
            0
        );
        assert_eq!(count_rows(db.as_ref(), "files", "id = 21").await, 0);
        assert_eq!(count_rows(db.as_ref(), "files", "id = 22").await, 1);
        assert_eq!(count_rows(db.as_ref(), "subfiles", "id = 31").await, 0);
        assert_eq!(count_rows(db.as_ref(), "subfiles", "id = 32").await, 1);
        let repo = FoxyDb::from_turso(db.clone())
            .query_one(
                "SELECT local_checksum, remote_checksum, local_content_hash FROM repositories WHERE id = 1",
                params![],
            )
            .await
            .expect("load repository")
            .expect("repository should remain");
        assert_eq!(repo.get_string("local_checksum").unwrap(), "");
        assert_eq!(repo.get_string("remote_checksum").unwrap(), "");
        assert_eq!(repo.get_string("local_content_hash").unwrap(), "");
        assert!(!target_dir.exists());
        assert!(sibling_dir.exists());
    }
    #[test]
    fn normalize_url_adds_trailing_slash() {
        assert_eq!(normalize_url("https://example.com"), "https://example.com/");
    }

    #[test]
    fn normalize_url_keeps_existing_trailing_slash() {
        assert_eq!(
            normalize_url("https://example.com/"),
            "https://example.com/"
        );
    }

    #[test]
    fn normalize_url_preserves_path() {
        assert_eq!(
            normalize_url("https://example.com/repo/mods"),
            "https://example.com/repo/mods/"
        );
    }

    #[test]
    fn normalize_url_empty() {
        assert_eq!(normalize_url(""), "/");
    }

    // ── normalize_path_for_compare ──────────────────────────────────────

    #[test]
    fn normalize_path_for_compare_backslash_to_forward() {
        let result = normalize_path_for_compare(Path::new("C:\\mods\\addon"));
        assert!(!result.contains('\\'));
    }

    #[test]
    fn normalize_path_for_compare_strips_trailing() {
        let result = normalize_path_for_compare(Path::new("/mods/addon/"));
        assert!(!result.ends_with('/'));
    }

    // ── is_safe_child ───────────────────────────────────────────────────

    #[test]
    fn is_safe_child_valid_child() {
        assert!(is_safe_child(
            Path::new("/mods"),
            Path::new("/mods/my_addon")
        ));
    }

    #[test]
    fn is_safe_child_same_path_is_not_safe() {
        assert!(!is_safe_child(Path::new("/mods"), Path::new("/mods")));
    }

    #[test]
    fn is_safe_child_parent_traversal_is_not_safe() {
        assert!(!is_safe_child(
            Path::new("/mods/addons"),
            Path::new("/mods")
        ));
    }

    #[test]
    fn is_safe_child_sibling_is_not_safe() {
        assert!(!is_safe_child(Path::new("/mods/a"), Path::new("/mods/b")));
    }

    #[test]
    fn is_safe_child_empty_base_is_not_safe() {
        assert!(!is_safe_child(Path::new(""), Path::new("/mods")));
    }

    #[test]
    fn is_safe_child_empty_candidate_is_not_safe() {
        assert!(!is_safe_child(Path::new("/mods"), Path::new("")));
    }

    #[test]
    fn is_safe_child_deeply_nested() {
        assert!(is_safe_child(
            Path::new("/mods"),
            Path::new("/mods/addon/sub/deep/file")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn is_safe_child_windows_backslashes() {
        assert!(is_safe_child(
            Path::new("C:\\mods"),
            Path::new("C:\\mods\\addon")
        ));
    }

    // ── resolve_mod_path ────────────────────────────────────────────────

    #[test]
    fn resolve_mod_path_relative() {
        let result = resolve_mod_path(Path::new("/base"), "@my_mod");
        assert_eq!(result, Some(PathBuf::from("/base/@my_mod")));
    }

    #[test]
    fn resolve_mod_path_absolute() {
        let result = resolve_mod_path(Path::new("/base"), "/absolute/mod");
        assert_eq!(result, Some(PathBuf::from("/absolute/mod")));
    }

    #[test]
    fn resolve_mod_path_empty_returns_none() {
        assert_eq!(resolve_mod_path(Path::new("/base"), ""), None);
    }

    #[test]
    fn resolve_mod_path_whitespace_only_returns_none() {
        assert_eq!(resolve_mod_path(Path::new("/base"), "   "), None);
    }

    #[test]
    fn resolve_mod_path_trims_whitespace() {
        let result = resolve_mod_path(Path::new("/base"), "  @mod  ");
        assert_eq!(result, Some(PathBuf::from("/base/@mod")));
    }

    // ── remove_repository_files (filesystem interaction) ────────────────

    #[test]
    fn remove_repository_files_skips_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let sibling = tempfile::tempdir().unwrap();

        // Create a file in the sibling directory
        let sibling_file = sibling.path().join("important.txt");
        std::fs::write(&sibling_file, b"do not delete").unwrap();

        let paths = vec![sibling.path().to_string_lossy().to_string()];
        remove_repository_files(&base.to_string_lossy(), &paths, &[]);

        assert!(
            sibling_file.exists(),
            "sibling file should not have been deleted"
        );
    }

    #[test]
    fn remove_repository_files_removes_tracked_files_and_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let addon_dir = base.join("@my_mod");
        let nested_dir = addon_dir.join("addons").join("data");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let tracked_file = nested_dir.join("config.cpp");
        std::fs::write(&tracked_file, b"class CfgMods {};").unwrap();

        remove_repository_files(
            &base.to_string_lossy(),
            &[tracked_file.to_string_lossy().to_string()],
            &["@my_mod".to_string()],
        );

        assert!(
            !addon_dir.exists(),
            "child addon directory should have been removed"
        );
    }

    #[test]
    fn remove_repository_files_preserves_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let addon_dir = dir.path().join("@shared");
        std::fs::create_dir(&addon_dir).unwrap();
        let untracked_file = addon_dir.join("keep.txt");
        std::fs::write(&untracked_file, b"keep").unwrap();

        remove_repository_files(
            &dir.path().to_string_lossy(),
            &[],
            &[addon_dir.to_string_lossy().to_string()],
        );

        assert!(untracked_file.exists());
        assert!(addon_dir.exists());
    }

    /// Full-scale reproducer for the force-redownload purge hang. Seeds the
    /// complete graph at the real TFR_40K scale (1515 files / 66,336 subfiles /
    /// parts) and runs the purge's exact statement sequence inside one
    /// transaction with per-statement timing, to pinpoint which delete wedges on
    /// Turso's beta planner. Run:
    /// `cargo test -p Foxy bench_purge_part_delete_strategies -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn bench_purge_part_delete_strategies() {
        use crate::core::tasks::db_turso::connect_tuned;

        const FILES: i64 = 1515;
        const SUBS: i64 = 66_336;
        const ADDONS: i64 = 41;

        // Turso (beta) has no recursive CTE / generate_series, so seed via
        // chunked multi-row VALUES inserts with inlined integer literals (no
        // bound params → no 999-variable limit). Test-only data, no injection risk.
        async fn seed(db: &turso::Database) {
            const CHUNK: i64 = 2000;
            let mut files_vals = Vec::new();
            for n in 1..=FILES {
                files_vals.push(format!("({n},'f{n}','r{n}','l{n}',1,0)"));
            }
            execute_sql(
                db,
                &format!(
                    "INSERT INTO files (id,name,remote_path,local_path,length,data_order) VALUES {}",
                    files_vals.join(",")
                ),
                vec![],
            )
            .await
            .expect("seed files");

            let mut n = 1i64;
            while n <= SUBS {
                let end = (n + CHUNK - 1).min(SUBS);
                let mut vals = Vec::new();
                for i in n..=end {
                    let fid = ((i - 1) % FILES) + 1;
                    vals.push(format!("({i},{fid},'p{i}',1,0,1,0,'','',0)"));
                }
                execute_sql(
                    db,
                    &format!(
                        "INSERT INTO subfiles
                           (id,file_id,path,local_length,local_start,remote_length,
                            remote_start,local_checksum,remote_checksum,data_order)
                         VALUES {}",
                        vals.join(",")
                    ),
                    vec![],
                )
                .await
                .expect("seed subfiles");
                n = end + 1;
            }

            let mut n = 1i64;
            while n <= SUBS {
                let end = (n + CHUNK - 1).min(SUBS);
                let mut vals = Vec::new();
                for i in n..=end {
                    vals.push(format!("({i},'r','l',1,0)"));
                }
                execute_sql(
                    db,
                    &format!(
                        "INSERT INTO download_target_file_part
                           (subfile_id,download_remote_url,download_local_path,size,offset)
                         VALUES {}",
                        vals.join(",")
                    ),
                    vec![],
                )
                .await
                .expect("seed dtfp");
                n = end + 1;
            }
        }

        // Seed the rest of the graph the purge transaction walks.
        async fn seed_graph(db: &turso::Database) {
            execute_sql(
                db,
                "INSERT INTO repositories (id,name,remote_url,local_path) VALUES (1,'r','http://x/','S:/x')",
                vec![],
            )
            .await
            .expect("seed repo");
            let mut addon_vals = Vec::new();
            let mut ra_vals = Vec::new();
            for a in 1..=ADDONS {
                addon_vals.push(format!("({a},'@a{a}','r{a}','l{a}',1,1,0)"));
                ra_vals.push(format!("(1,{a})"));
            }
            execute_sql(
                db,
                &format!(
                    "INSERT INTO addons (id,name,remote_path,local_path,enabled,required,data_order) VALUES {}",
                    addon_vals.join(",")
                ),
                vec![],
            )
            .await
            .expect("seed addons");
            execute_sql(
                db,
                &format!(
                    "INSERT INTO repository_addons (repository_id,addon_id) VALUES {}",
                    ra_vals.join(",")
                ),
                vec![],
            )
            .await
            .expect("seed repo_addons");
            let mut n = 1i64;
            while n <= FILES {
                let end = (n + 1999).min(FILES);
                let mut af = Vec::new();
                let mut dtf = Vec::new();
                for i in n..=end {
                    af.push(format!("({},{i})", ((i - 1) % ADDONS) + 1));
                    dtf.push(format!("({i},'r','l',1)"));
                }
                execute_sql(
                    db,
                    &format!(
                        "INSERT INTO addon_files (addon_id,file_id) VALUES {}",
                        af.join(",")
                    ),
                    vec![],
                )
                .await
                .expect("seed addon_files");
                execute_sql(
                    db,
                    &format!(
                        "INSERT INTO download_target_file (file_id,download_remote_url,download_local_path,size) VALUES {}",
                        dtf.join(",")
                    ),
                    vec![],
                )
                .await
                .expect("seed dtf");
                n = end + 1;
            }
        }

        let (_t, db) = create_test_db().await;
        let seed_started = Instant::now();
        seed(db.as_ref()).await;
        seed_graph(db.as_ref()).await;
        eprintln!(
            "[bench-purge] seeded files={FILES} subs={SUBS} addons={ADDONS} in {:.2}s",
            seed_started.elapsed().as_secs_f64()
        );

        // Replicate the purge transaction statement-by-statement with timing.
        let conn = connect_tuned(db.as_ref()).await.expect("connect");
        let step = |label: &'static str, sql: &'static str| {
            let conn = &conn;
            async move {
                let t = Instant::now();
                conn.execute(sql, ())
                    .await
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                eprintln!(
                    "[bench-purge]   {label:<28} {:.3}s",
                    t.elapsed().as_secs_f64()
                );
                0
            }
        };

        let txn_started = Instant::now();
        conn.execute("BEGIN", ()).await.unwrap();
        step(
            "create repo_ids",
            "CREATE TEMP TABLE temp.foxy_purge_repo_ids (id INTEGER PRIMARY KEY)",
        )
        .await;
        step(
            "create addon_ids",
            "CREATE TEMP TABLE temp.foxy_purge_addon_ids (addon_id INTEGER PRIMARY KEY)",
        )
        .await;
        step(
            "create orphan_addon_ids",
            "CREATE TEMP TABLE temp.foxy_purge_orphan_addon_ids (addon_id INTEGER PRIMARY KEY)",
        )
        .await;
        step(
            "create orphan_file_ids",
            "CREATE TEMP TABLE temp.foxy_purge_orphan_file_ids (file_id INTEGER PRIMARY KEY)",
        )
        .await;
        step(
            "insert repo_ids",
            "INSERT OR IGNORE INTO temp.foxy_purge_repo_ids (id) VALUES (1)",
        )
        .await;
        step("insert addon_ids", "INSERT OR IGNORE INTO temp.foxy_purge_addon_ids (addon_id) SELECT repository_addons.addon_id FROM repository_addons INNER JOIN temp.foxy_purge_repo_ids ON temp.foxy_purge_repo_ids.id = repository_addons.repository_id").await;
        step("insert orphan_addon_ids", "INSERT OR IGNORE INTO temp.foxy_purge_orphan_addon_ids (addon_id) SELECT addon_id FROM temp.foxy_purge_addon_ids WHERE NOT EXISTS (SELECT 1 FROM repository_addons o WHERE o.addon_id = temp.foxy_purge_addon_ids.addon_id AND o.repository_id NOT IN (SELECT id FROM temp.foxy_purge_repo_ids))").await;
        step("insert orphan_file_ids", "INSERT OR IGNORE INTO temp.foxy_purge_orphan_file_ids (file_id) SELECT addon_files.file_id FROM addon_files INNER JOIN temp.foxy_purge_orphan_addon_ids ON temp.foxy_purge_orphan_addon_ids.addon_id = addon_files.addon_id WHERE NOT EXISTS (SELECT 1 FROM addon_files r WHERE r.file_id = addon_files.file_id AND r.addon_id NOT IN (SELECT addon_id FROM temp.foxy_purge_orphan_addon_ids))").await;
        step(
            "delete repositories",
            "DELETE FROM repositories WHERE id IN (SELECT id FROM temp.foxy_purge_repo_ids)",
        )
        .await;
        step("delete dtfp (nested)", "DELETE FROM download_target_file_part WHERE subfile_id IN (SELECT id FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.foxy_purge_orphan_file_ids))").await;
        step("delete subfiles", "DELETE FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.foxy_purge_orphan_file_ids)").await;
        step("delete download_patch_op", "DELETE FROM download_patch_op WHERE file_id IN (SELECT file_id FROM temp.foxy_purge_orphan_file_ids)").await;
        step("delete download_patch_file", "DELETE FROM download_patch_file WHERE file_id IN (SELECT file_id FROM temp.foxy_purge_orphan_file_ids)").await;
        step("delete download_target_file", "DELETE FROM download_target_file WHERE file_id IN (SELECT file_id FROM temp.foxy_purge_orphan_file_ids)").await;
        step("delete addon_files", "DELETE FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.foxy_purge_orphan_addon_ids)").await;
        step("delete addons", "DELETE FROM addons WHERE id IN (SELECT addon_id FROM temp.foxy_purge_orphan_addon_ids)").await;
        step(
            "delete files",
            "DELETE FROM files WHERE id IN (SELECT file_id FROM temp.foxy_purge_orphan_file_ids)",
        )
        .await;
        let commit_started = Instant::now();
        conn.execute("COMMIT", ()).await.unwrap();
        eprintln!(
            "[bench-purge]   {:<28} {:.3}s",
            "COMMIT",
            commit_started.elapsed().as_secs_f64()
        );
        eprintln!(
            "[bench-purge] TOTAL transaction {:.3}s",
            txn_started.elapsed().as_secs_f64()
        );

        // Fast-path candidate: whole-table DELETE (truncate-style) vs the scoped
        // WHERE-IN deletes above. Re-seed then measure.
        seed(db.as_ref()).await;
        let conn2 = connect_tuned(db.as_ref()).await.expect("connect2");
        let t = Instant::now();
        conn2
            .execute("DELETE FROM download_target_file_part", ())
            .await
            .unwrap();
        eprintln!(
            "[bench-purge]   whole-table dtfp           {:.3}s",
            t.elapsed().as_secs_f64()
        );
        let t = Instant::now();
        conn2.execute("DELETE FROM subfiles", ()).await.unwrap();
        eprintln!(
            "[bench-purge]   whole-table subfiles       {:.3}s",
            t.elapsed().as_secs_f64()
        );
    }
}
