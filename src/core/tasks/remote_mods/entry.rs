use super::addon_links::reconcile_repository_addon_links;
use super::helpers::collect_desired_mod_pairs;
use super::upsert::process_mods_upsert;
use crate::core::db::{FoxyDb, params};
use crate::core::models::context::FoxyContext;
use crate::core::models::repository::FoxyRepository;
use crate::core::tasks::remote_files::ModRecheckStats;
use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Whether this repository instance has no linked subfile rows yet. This is the
/// conflict-free case for plain/deferred part inserts even when a repository
/// space has already populated `subfiles` for another repository in the same DB.
/// On query error we return false (keep the safe `ON CONFLICT` path).
async fn repository_subfiles_empty(db: &FoxyDb, repository_id: i64) -> bool {
    match db
        .query_all(
            "SELECT 1 \
             FROM subfiles sf \
             JOIN addon_files af ON af.file_id = sf.file_id \
             JOIN repository_addons ra ON ra.addon_id = af.addon_id \
             WHERE ra.repository_id = ? \
             LIMIT 1",
            params![repository_id],
        )
        .await
    {
        Ok(rows) => rows.is_empty(),
        Err(e) => {
            warn!("Could not check repository subfiles emptiness for fresh bulk load: {e}");
            false
        }
    }
}

/// Process required and optional mods for given repository and pre-fetched repository json data
pub(crate) async fn remote_mods_with_data(
    context: Arc<FoxyContext>,
    repository: Arc<FoxyRepository>,
    data: serde_json::Value,
    enabled_overrides: Option<HashMap<String, bool>>,
) {
    let rebuild_start = std::time::Instant::now();

    // Fresh bulk-load fast path (after_turso_regression_analysis6.md, Step 1 / "Option
    // B"): if `subfiles` is globally empty (post-whole-wipe force-redownload or first
    // download) the part insert is a pure append of brand-new rows, so the part insert
    // switches to a plain conflict-free INSERT via `context.fresh_subfiles_load` instead
    // of the `ON CONFLICT (file_id, path)` upsert. The indexes are kept LIVE: analysis
    // #6 measured that dropping + rebuilding them added a serial ~13.78s index rebuild
    // on the critical path, while the insert work it saved was already hidden behind the
    // HTTP manifest fetch + single-writer permit_wait - i.e. deferral was a net loss.
    // Must be decided + set BEFORE spawning so every mod task sees the flag. Gated on
    // emptiness so incremental rebuilds (sibling rows present) keep the safe upsert path.
    // Repository-space note: this check is intentionally scoped to the current
    // repository instance, not the global `subfiles` table.
    let fresh_bulk_load = repository_subfiles_empty(&context.db(), repository.id as i64).await;
    if fresh_bulk_load {
        info!(
            "Metadata rebuild: repository subfiles empty - using conflict-free part bulk load (repo {})",
            repository.remote_url
        );
        context.set_fresh_subfiles_load(true);
    }

    let mut tasks = Vec::new();
    let enabled_overrides = enabled_overrides.map(Arc::new);
    let mut desired_mod_pairs: Vec<(String, String)> = Vec::new();
    let mut desired_mod_pairs_dedupe: HashSet<String> = HashSet::new();

    if let Some(required_mods_data) = data.get("requiredMods") {
        collect_desired_mod_pairs(
            repository.as_ref(),
            context.repository_space_shared_path.as_deref(),
            required_mods_data,
            &mut desired_mod_pairs_dedupe,
            &mut desired_mod_pairs,
        );
        let repository_clone = repository.clone();
        let required_mods_data_clone = required_mods_data.clone();
        let context_clone = context.clone();
        let enabled_overrides_clone = enabled_overrides.clone();
        tasks.push(tokio::spawn(async move {
            process_mods_upsert(
                context_clone,
                repository_clone,
                required_mods_data_clone,
                true,
                enabled_overrides_clone,
            )
            .await
        }));
    }
    if let Some(optional_mods_data) = data.get("optionalMods") {
        collect_desired_mod_pairs(
            repository.as_ref(),
            context.repository_space_shared_path.as_deref(),
            optional_mods_data,
            &mut desired_mod_pairs_dedupe,
            &mut desired_mod_pairs,
        );
        let repository_clone = repository.clone();
        let optional_mods_data_clone = optional_mods_data.clone();
        let context_clone = context.clone();
        let enabled_overrides_clone = enabled_overrides.clone();
        tasks.push(tokio::spawn(async move {
            process_mods_upsert(
                context_clone,
                repository_clone,
                optional_mods_data_clone,
                false,
                enabled_overrides_clone,
            )
            .await
        }));
    }

    let upsert_elapsed = rebuild_start.elapsed();
    info!(
        "Metadata rebuild: mod upsert + link phase completed in {:.2?} for repo {}",
        upsert_elapsed, repository.remote_url
    );

    let parallel_start = std::time::Instant::now();
    let mut all_stats: Vec<ModRecheckStats> = Vec::new();
    let mut all_resolved_ids: HashSet<i64> = HashSet::new();
    for task in tasks {
        match task.await {
            Ok((mut stats, ids)) => {
                all_stats.append(&mut stats);
                all_resolved_ids.extend(ids);
            }
            Err(err) => {
                warn!(
                    "Parallel mod processing task failed for repo {}: {}",
                    repository.remote_url, err
                );
            }
        }
    }
    let parallel_elapsed = parallel_start.elapsed();
    info!(
        "Metadata rebuild: parallel mod processing completed in {:.2?} for repo {}",
        parallel_elapsed, repository.remote_url
    );

    // Close the fresh bulk-load window: clear the flag so any subsequent writes in
    // this session use the normal upsert path. Indexes were never dropped, so there is
    // nothing to rebuild (analysis #6 Step 1 / "Option B").
    if fresh_bulk_load {
        context.set_fresh_subfiles_load(false);
    }

    reconcile_repository_addon_links(
        context.clone(),
        repository.clone(),
        all_resolved_ids,
        desired_mod_pairs.len(),
    )
    .await;

    if !all_stats.is_empty() {
        let total_files: usize = all_stats.iter().map(|s| s.files).sum();
        let total_parts: usize = all_stats.iter().map(|s| s.parts).sum();
        let total_bytes: u64 = all_stats.iter().map(|s| s.bytes).sum();
        let sum_mod_durations: std::time::Duration = all_stats.iter().map(|s| s.duration).sum();
        info!(
            "Mod recheck summary: {} mods, {} files, {} parts, {}B total, sum_of_mod_durations={:.2?}, wall_clock={:.2?}",
            all_stats.len(),
            total_files,
            total_parts,
            total_bytes,
            sum_mod_durations,
            parallel_elapsed
        );
        // Log all mods sorted by duration (slowest first) for easy profiling
        all_stats.sort_by_key(|stat| std::cmp::Reverse(stat.duration));
        for (rank, s) in all_stats.iter().enumerate() {
            let pct = if parallel_elapsed.as_secs_f64() > 0.0 {
                (s.duration.as_secs_f64() / parallel_elapsed.as_secs_f64()) * 100.0
            } else {
                0.0
            };
            let resp_size = if s.http_response_bytes >= 1_048_576 {
                format!("{:.1}MB", s.http_response_bytes as f64 / 1_048_576.0)
            } else if s.http_response_bytes >= 1_024 {
                format!("{:.0}KB", s.http_response_bytes as f64 / 1_024.0)
            } else {
                format!("{}B", s.http_response_bytes)
            };
            info!(
                "  #{} {} | files={} parts={} download={:.0?} ({}) parse={:.0?} file_upsert={:.0?} parts_db={:.0?} total={:.2?} ({:.1}% of wall)",
                rank + 1,
                s.mod_path,
                s.files,
                s.parts,
                s.http_download_duration,
                resp_size,
                s.http_parse_duration,
                s.file_upsert_duration,
                s.parts_persist_duration,
                s.duration,
                pct
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::db_turso::build_test_database;

    async fn test_db() -> FoxyDb {
        FoxyDb::from_turso(build_test_database().await)
    }

    #[tokio::test]
    async fn repository_subfiles_empty_is_scoped_to_repository_instance() {
        let db = test_db().await;
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES \
             (1, 'repo1', 'u1', 'p'), (2, 'repo2', 'u2', 'p')",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, required) VALUES \
             (10, 'a1', 'rp1', 'lp1', 1), (20, 'a2', 'rp2', 'lp2', 1)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES \
             (100, 'f1', 'frp1', 'flp1'), (200, 'f2', 'frp2', 'flp2')",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 10), (2, 20)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (10, 100), (20, 200)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO subfiles (file_id, path, remote_length, remote_start, remote_checksum, data_order) \
             VALUES (100, 'p1', 1, 0, 'c1', 0)",
            Vec::new(),
        )
        .await
        .unwrap();

        assert!(!repository_subfiles_empty(&db, 1).await);
        assert!(
            repository_subfiles_empty(&db, 2).await,
            "repo2 must stay fresh even though repo1 already has subfiles"
        );
    }
}
