use super::super::*;
use crate::core::db::DbValue;
use crate::core::tasks::calculate_hashes::{
    AddonHashMetrics, FileHashBatchResult, HashPhaseTimings, RepositoryHashContext,
    calculate_hashes_for_files_in_tree_with_profile_and_sticky_auto,
    calculate_hashes_for_files_with_profile_and_sticky_auto,
};
use crate::core::tasks::init_database::{
    SqliteWriteMetricSnapshot, log_sqlite_write_metrics_since, sqlite_perf_snapshot,
    sqlite_write_metrics_snapshot,
};
use crate::core::utils::format::sanitize_log_url;
use crate::ui::types::HashIoProfilePreference;
use std::collections::BTreeMap;

async fn collect_already_verified_file_ids(
    context: Arc<FoxyContext>,
    file_ids: &HashSet<u64>,
) -> HashSet<u64> {
    let db = context.db();
    let chunk_size = read_chunk_ids();
    let mut verified = HashSet::new();
    let mut ids: Vec<i64> = file_ids.iter().map(|id| *id as i64).collect();
    ids.sort_unstable();

    for chunk in ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT id, local_checksum, remote_checksum FROM files \
             WHERE id IN ({placeholders}) AND remote_checksum != ''"
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                verified.extend(rows.iter().filter_map(|row| {
                    let local = row.get_string("local_checksum").ok()?;
                    let remote = row.get_string("remote_checksum").ok()?;
                    if !local.is_empty() && local == remote {
                        Some(row.get_i64("id").ok()? as u64)
                    } else {
                        None
                    }
                }));
            }
            Err(err) => {
                warn!(
                    "Failed to prefilter already verified hash files; falling back to tree load: {}",
                    err
                );
                return HashSet::new();
            }
        }
    }

    verified
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_incremental_hash_batch(
    context: Arc<FoxyContext>,
    repository_url: &str,
    hash_context: &mut Option<RepositoryHashContext>,
    file_ids: &HashSet<u64>,
    hashed_download_file_ids: &mut HashSet<u64>,
    incremental_hash_duration: &mut Duration,
    hash_tree_loads: &mut usize,
    progress_tx: Option<&Sender<ProgressEvent>>,
    hash_io_profile: HashIoProfilePreference,
    sticky_auto_profile: &mut Option<HashIoProfilePreference>,
    addon_hash_metrics: &mut Vec<AddonHashMetrics>,
    progress_percent: f32,
    clean_part_mark_downloaded_files: bool,
) -> FileHashBatchResult {
    if file_ids.is_empty() {
        return FileHashBatchResult::default();
    }

    let already_verified_file_ids =
        collect_already_verified_file_ids(context.clone(), file_ids).await;
    let file_ids_to_hash: HashSet<u64> = if already_verified_file_ids.is_empty() {
        file_ids.clone()
    } else {
        hashed_download_file_ids.extend(already_verified_file_ids.iter().copied());
        file_ids
            .difference(&already_verified_file_ids)
            .copied()
            .collect()
    };
    if file_ids_to_hash.is_empty() {
        info!(
            "Incremental hash prefilter: all {} requested files already match remote checksum",
            file_ids.len()
        );
        return FileHashBatchResult {
            requested_file_ids: file_ids.clone(),
            processed_file_ids: already_verified_file_ids,
            ..Default::default()
        };
    }
    if !already_verified_file_ids.is_empty() {
        info!(
            "Incremental hash prefilter: skipped {} already verified files out of {} requested before tree load",
            already_verified_file_ids.len(),
            file_ids.len()
        );
    }

    if hash_context.is_none()
        && let Some(loaded) = RepositoryHashContext::load(context.clone(), repository_url).await
    {
        *hash_tree_loads += 1;
        info!(
            "Initialized incremental hash context for repo={} (tree_loads={})",
            repository_url, hash_tree_loads
        );
        *hash_context = Some(loaded);
    }

    let incremental_hash_start = std::time::Instant::now();
    let hash_result = if let Some(hash_context) = hash_context.as_mut() {
        if hash_context.repository_url != repository_url {
            warn!(
                "Incremental hash context repository mismatch: expected={} actual={}",
                hash_context.repository_url, repository_url
            );
        }
        calculate_hashes_for_files_in_tree_with_profile_and_sticky_auto(
            context,
            &mut hash_context.tree,
            &file_ids_to_hash,
            None,
            false,
            hash_io_profile,
            *sticky_auto_profile,
            true,
            clean_part_mark_downloaded_files,
        )
        .await
    } else {
        *hash_tree_loads += 1;
        calculate_hashes_for_files_with_profile_and_sticky_auto(
            context,
            repository_url,
            &file_ids_to_hash,
            None,
            false,
            hash_io_profile,
            *sticky_auto_profile,
            true,
            clean_part_mark_downloaded_files,
        )
        .await
    };
    *incremental_hash_duration += incremental_hash_start.elapsed();
    let batch_elapsed = incremental_hash_start.elapsed();

    if let Some(decision) = hash_result.profile_decision.as_ref() {
        let benchmark_mbps = if decision.benchmark_elapsed.as_secs_f64() > 0.0 {
            (decision.benchmarked_bytes as f64 / (1024.0 * 1024.0))
                / decision.benchmark_elapsed.as_secs_f64()
        } else {
            0.0
        };
        info!(
            "Incremental hash batch summary: repo={} requested_files={} processed_files={} updated_files={} elapsed={:.2}s selected_profile={} benchmark_files={} benchmark_bytes={} benchmark_elapsed={:.2}s benchmark_avg={:.2} MB/s reason={}",
            repository_url,
            hash_result.requested_file_ids.len(),
            hash_result.processed_file_ids.len(),
            hash_result.updated_file_ids.len(),
            batch_elapsed.as_secs_f64(),
            decision.selected,
            decision.benchmarked_files,
            decision.benchmarked_bytes,
            decision.benchmark_elapsed.as_secs_f64(),
            benchmark_mbps,
            decision.reason
        );
        if hash_io_profile == HashIoProfilePreference::Auto && decision.sticky {
            *sticky_auto_profile = Some(decision.selected);
        }
        if let Some(tx) = progress_tx {
            let _ = tx.send(ProgressEvent::Stage {
                label: format!("Hashing downloaded files: profile {}", decision.selected),
                percent: progress_percent,
            });
        }
    } else {
        info!(
            "Incremental hash batch summary: repo={} requested_files={} processed_files={} updated_files={} elapsed={:.2}s selected_profile=<none>",
            repository_url,
            hash_result.requested_file_ids.len(),
            hash_result.processed_file_ids.len(),
            hash_result.updated_file_ids.len(),
            batch_elapsed.as_secs_f64()
        );
    }

    if !hash_result.processed_file_ids.is_empty() {
        hashed_download_file_ids.extend(hash_result.processed_file_ids.iter().copied());
        addon_hash_metrics.extend(hash_result.addon_metrics.iter().cloned());
    } else {
        warn!(
            "Incremental hash returned no updates for repo={} files={}",
            repository_url,
            file_ids.len()
        );
    }
    hash_result
}

pub(super) fn render_aggregated_addon_hash_metrics(metrics: &[AddonHashMetrics]) -> String {
    if metrics.is_empty() {
        return String::new();
    }

    let mut by_addon = BTreeMap::<(String, String), AddonHashMetrics>::new();
    for metric in metrics {
        by_addon
            .entry((metric.label.clone(), metric.addon.clone()))
            .and_modify(|existing| existing.merge(metric))
            .or_insert_with(|| metric.clone());
    }

    let mut lines = Vec::new();
    lines.push("-- HASH ADDON SUMMARY --".to_owned());
    lines.push(format!(
        "addons={} raw_batches={}",
        by_addon.len(),
        metrics.len()
    ));
    for metric in by_addon.values() {
        lines.push(format!(
            "addon: label={} name={} files={} missing={} parts={} bytes_hashed={} bytes_estimated={}",
            metric.label,
            metric.addon,
            metric.files,
            metric.missing_files,
            metric.parts,
            metric.hashed_bytes,
            metric.estimated_bytes
        ));
        lines.push(format!(
            "       time: total_parts={:.3}s slowest_file={:.3}s blocking_hash={:.3}s metadata={:.3}s semaphore_wait={:.3}s",
            metric.part_elapsed_sum.as_secs_f64(),
            metric.file_elapsed_max.as_secs_f64(),
            metric.blocking_hash_elapsed_sum.as_secs_f64(),
            metric.metadata_elapsed_sum.as_secs_f64(),
            metric.semaphore_wait_elapsed_sum.as_secs_f64()
        ));
        lines.push(format!(
            "       layout: files={} remote_span_files={} entries={} mapped_parts={} fallback_parts={} parse={:.3}s map={:.3}s total={:.3}s",
            metric.layout_files,
            metric.remote_span_files,
            metric.layout_entries,
            metric.mapped_parts,
            metric.fallback_parts,
            metric.layout_parse_elapsed_sum.as_secs_f64(),
            metric.layout_map_elapsed_sum.as_secs_f64(),
            metric.layout_elapsed_sum.as_secs_f64()
        ));
    }
    lines.push("-- END HASH ADDON SUMMARY --".to_owned());
    lines.join("\n")
}

pub(super) struct HashTotalSummary<'a> {
    pub repo: &'a str,
    pub files: usize,
    pub bytes: u64,
    pub incremental_files: usize,
    pub remaining_files: usize,
    pub tree_loads: usize,
    pub finalized: bool,
    pub total_elapsed: Duration,
    pub incremental_elapsed: Duration,
    pub finalize_elapsed: Duration,
    pub selected_profile: &'a str,
    pub phase_timings: HashPhaseTimings,
    pub critical_tail_after_download: Duration,
    pub overlapped_with_download: Duration,
}

fn sum_metric_duration<F>(metrics: &[AddonHashMetrics], mut field: F) -> Duration
where
    F: FnMut(&AddonHashMetrics) -> Duration,
{
    metrics
        .iter()
        .fold(Duration::ZERO, |total, metric| total + field(metric))
}

pub(super) fn render_hash_total_summary(
    summary: &HashTotalSummary<'_>,
    addon_metrics: &[AddonHashMetrics],
) -> String {
    let repo = sanitize_log_url(summary.repo);
    let avg_mbps = if summary.total_elapsed.as_secs_f64() > 0.0 {
        (summary.bytes as f64 / (1024.0 * 1024.0)) / summary.total_elapsed.as_secs_f64()
    } else {
        0.0
    };
    let raw_hash_blocking =
        sum_metric_duration(addon_metrics, |metric| metric.blocking_hash_elapsed_sum);
    let hash_metadata = sum_metric_duration(addon_metrics, |metric| metric.metadata_elapsed_sum);
    let addon_repo_rollup_persist = summary.phase_timings.addon_rollup_persist
        + summary.phase_timings.repository_rollup_persist;

    [
        "-- HASH METRICS SUMMARY --".to_owned(),
        format!(
            "repo={} selected_profile={} finalized={}",
            repo, summary.selected_profile, summary.finalized
        ),
        format!(
            "work: files={} incremental_files={} final_files={} tree_loads={}",
            summary.files, summary.incremental_files, summary.remaining_files, summary.tree_loads
        ),
        format!("bytes: total={} avg={:.2} MB/s", summary.bytes, avg_mbps),
        format!(
            "time: total={:.2}s incremental={:.2}s finalize={:.2}s",
            summary.total_elapsed.as_secs_f64(),
            summary.incremental_elapsed.as_secs_f64(),
            summary.finalize_elapsed.as_secs_f64()
        ),
        "-- HASH PHASE BREAKDOWN --".to_owned(),
        format!(
            "raw_hash_blocking={:.2}s hash_metadata={:.2}s part_checksum_persist={:.2}s file_rollup_persist={:.2}s addon_repo_rollup_persist={:.2}s critical_tail_after_download={:.2}s overlapped_with_download={:.2}s",
            raw_hash_blocking.as_secs_f64(),
            hash_metadata.as_secs_f64(),
            summary.phase_timings.part_checksum_persist.as_secs_f64(),
            summary.phase_timings.file_rollup_persist.as_secs_f64(),
            addon_repo_rollup_persist.as_secs_f64(),
            summary.critical_tail_after_download.as_secs_f64(),
            summary.overlapped_with_download.as_secs_f64()
        ),
        format!(
            "phase_wall: hash_scheduler={:.2}s apply_part_hashes={:.2}s file_rollup={:.2}s addon_rollup={:.2}s repository_rollup={:.2}s",
            summary.phase_timings.hash_wall.as_secs_f64(),
            summary.phase_timings.apply_part_hashes.as_secs_f64(),
            summary.phase_timings.file_rollup.as_secs_f64(),
            summary.phase_timings.addon_rollup.as_secs_f64(),
            summary.phase_timings.repository_rollup.as_secs_f64()
        ),
        format!(
            "clean_mark: files={} parts={} fallback_part_update_parts={}",
            summary.phase_timings.clean_part_mark_files,
            summary.phase_timings.clean_part_mark_parts,
            summary.phase_timings.fallback_part_update_parts
        ),
        "-- END HASH PHASE BREAKDOWN --".to_owned(),
        "-- END HASH METRICS --".to_owned(),
    ]
    .join("\n")
}

pub(super) struct SqlitePerfRunGuard {
    pub repository_url: String,
    pub mode: SyncMode,
    pub started_at: Instant,
    pub baseline: crate::core::tasks::init_database::SqlitePerfSnapshot,
    pub write_metric_baseline: BTreeMap<String, SqliteWriteMetricSnapshot>,
    final_report_logged: bool,
}

impl Drop for SqlitePerfRunGuard {
    fn drop(&mut self) {
        if self.final_report_logged {
            return;
        }
        let delta = sqlite_perf_snapshot().delta_since(self.baseline);
        info!(
            "SQLite sync metrics: repo={} mode={:?} lock_retries={} avg_backoff_ms={:.1} total_backoff_ms={} db_write_time_ms={:.1} elapsed_ms={}",
            self.repository_url,
            self.mode,
            delta.lock_retries,
            delta.avg_backoff_ms(),
            delta.lock_backoff_ms_total,
            delta.db_write_time_ms(),
            self.started_at.elapsed().as_millis()
        );
        log_sqlite_write_metrics_since(
            &self.write_metric_baseline,
            &format!("repo={} mode={:?}", self.repository_url, self.mode),
        );
    }
}

impl SqlitePerfRunGuard {
    pub(super) fn start(repository_url: String, mode: SyncMode, started_at: Instant) -> Self {
        Self {
            repository_url,
            mode,
            started_at,
            baseline: sqlite_perf_snapshot(),
            write_metric_baseline: sqlite_write_metrics_snapshot(),
            final_report_logged: false,
        }
    }

    pub(super) fn mark_final_report_logged(&mut self) {
        self.final_report_logged = true;
    }

    pub(super) fn render_summary(&self) -> String {
        let delta = sqlite_perf_snapshot().delta_since(self.baseline);
        let current = sqlite_write_metrics_snapshot();
        let mut categories = current
            .into_iter()
            .filter_map(|(label, metric)| {
                let delta = metric.delta_since(
                    self.write_metric_baseline
                        .get(&label)
                        .copied()
                        .unwrap_or_default(),
                );
                (delta.calls > 0).then_some((label, delta))
            })
            .collect::<Vec<_>>();
        categories.sort_by_key(|entry| std::cmp::Reverse(entry.1.total_time_ns_total));

        let mut lines = Vec::new();
        lines.push("-- DATABASE METRICS SUMMARY --".to_owned());
        lines.push(format!(
            "sqlite: mode={:?} lock_retries={} avg_backoff_ms={:.1} total_backoff_ms={} db_write_time_ms={:.1} elapsed_ms={}",
            self.mode,
            delta.lock_retries,
            delta.avg_backoff_ms(),
            delta.lock_backoff_ms_total,
            delta.db_write_time_ms(),
            self.started_at.elapsed().as_millis()
        ));
        lines.push(format!("write_categories={}", categories.len()));
        for (label, metric) in categories.into_iter().take(12) {
            lines.push(format!(
                "  {:<32} calls={} committed={} failed={} retries={} backoff_ms={} permit_wait_ms={:.1} total_ms={:.1}",
                label,
                metric.calls,
                metric.committed,
                metric.failed,
                metric.lock_retries,
                metric.lock_backoff_ms_total,
                metric.permit_wait_ms(),
                metric.total_time_ms()
            ));
        }
        lines.push("-- END DATABASE METRICS --".to_owned());
        lines.join("\n")
    }
}
