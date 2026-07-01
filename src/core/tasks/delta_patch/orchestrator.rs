use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::{
    DownloadPatchFile, delete_download_patch_file_by_file_id, load_download_patch_file,
    update_download_patch_file_status,
};
use crate::core::models::download_patch_op::{
    delete_download_patch_ops_for_file, fetch_download_patch_ops_for_file,
};
use crate::core::models::download_target_file::DownloadTargetFile;
use crate::core::tasks::download_files::{
    AdaptiveBandwidthLimiter, DownloadMetrics, SharedRollbackSession,
};
use anyhow::Context;
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::watch;

use super::apply::{
    apply_patch_to_temp_file, cleanup_patch_artifacts, compute_file_integrity_hash,
    diagnose_patch_output_segments, promote_temp_file_atomically, restore_backup,
    validate_runtime_ops,
};
use super::planning::load_patch_artifact;
use super::transfer::{download_patch_blob_ranges_parallel, preflight_copy_sources};
use super::types::{
    PATCH_STATUS_APPLYING, PATCH_STATUS_DONE, PATCH_STATUS_DOWNLOADING, PATCH_STATUS_FALLBACK_FULL,
    PATCH_STATUS_READY, PatchOpType, checksum_matches,
    compute_tree_checksum_from_segment_checksums, keep_patch_artifacts_for_diagnostics,
};
async fn mark_patch_fallback(
    context: Arc<FoxyContext>,
    patch_file: &DownloadPatchFile,
    reason: &str,
) {
    warn!(
        "Delta patch fallback for file_id={} reason={}",
        patch_file.file_id, reason
    );

    if let Err(err) = update_download_patch_file_status(
        context.clone(),
        patch_file.file_id as i64,
        PATCH_STATUS_FALLBACK_FULL,
        Some(reason),
    )
    .await
    {
        warn!(
            "Failed to update patch fallback status for file_id={}: {}",
            patch_file.file_id, err
        );
    }

    if keep_patch_artifacts_for_diagnostics() {
        return;
    }

    if let Err(err) =
        cleanup_patch_artifacts(&patch_file.patch_json_path, &patch_file.patch_blob_path).await
    {
        warn!(
            "Failed to clean patch artifacts for file_id={}: {}",
            patch_file.file_id, err
        );
    }
    if let Err(err) =
        delete_download_patch_ops_for_file(context.clone(), patch_file.file_id as i64).await
    {
        warn!(
            "Failed to delete patch ops after fallback for file_id={}: {}",
            patch_file.file_id, err
        );
    }
    if let Err(err) =
        delete_download_patch_file_by_file_id(context, patch_file.file_id as i64).await
    {
        warn!(
            "Failed to delete patch file row after fallback for file_id={}: {}",
            patch_file.file_id, err
        );
    }
}

pub(crate) async fn try_patch_first(
    context: Arc<FoxyContext>,
    download_target: &DownloadTargetFile,
    download_pause_rx: watch::Receiver<bool>,
    cancel_rx: watch::Receiver<bool>,
    rollback_session: Option<SharedRollbackSession>,
    rate_limiter: Arc<AdaptiveBandwidthLimiter>,
    metrics: Arc<DownloadMetrics>,
) -> anyhow::Result<bool> {
    let file_id = download_target.file_id as i64;
    let patch_started = std::time::Instant::now();

    let Some(patch_file) = load_download_patch_file(context.clone(), file_id)
        .await
        .context("failed to load patch row")?
    else {
        debug!(
            "Delta patch plan not available for file_id={}, falling back to full download",
            file_id
        );
        return Ok(false);
    };

    let artifact = match load_patch_artifact(&patch_file.patch_json_path).await {
        Ok(artifact) => artifact,
        Err(err) => {
            mark_patch_fallback(
                context,
                &patch_file,
                &format!("patch artifact read failed: {}", err),
            )
            .await;
            return Ok(false);
        }
    };

    if artifact.file_id != patch_file.file_id {
        mark_patch_fallback(
            context,
            &patch_file,
            "patch artifact file_id mismatch with DB record",
        )
        .await;
        return Ok(false);
    }

    if artifact.new_file_expected_size == 0 {
        mark_patch_fallback(
            context,
            &patch_file,
            "patch artifact has zero expected output size",
        )
        .await;
        return Ok(false);
    }

    let mut patch_ops = match fetch_download_patch_ops_for_file(context.clone(), file_id).await {
        Ok(ops) => ops,
        Err(err) => {
            mark_patch_fallback(
                context,
                &patch_file,
                &format!("failed to fetch patch ops: {}", err),
            )
            .await;
            return Ok(false);
        }
    };

    let insert_ops = patch_ops
        .iter()
        .filter(|op| PatchOpType::InsertRemote.matches(op))
        .count();
    let copy_ops = patch_ops.len().saturating_sub(insert_ops);
    let planned_download_bytes: u64 = patch_ops
        .iter()
        .filter(|op| PatchOpType::InsertRemote.matches(op))
        .map(|op| op.length)
        .sum();
    info!(
        "Delta patch attempt: file_id={} remote_url={} local_path={} ops={} copy_ops={} insert_ops={} planned_download_bytes={} full_bytes={} patch_blob={}",
        patch_file.file_id,
        artifact.remote_url,
        artifact.local_target_path,
        patch_ops.len(),
        copy_ops,
        insert_ops,
        planned_download_bytes,
        artifact.new_file_expected_size,
        patch_file.patch_blob_path
    );

    if let Err(err) = validate_runtime_ops(&patch_ops, artifact.new_file_expected_size) {
        mark_patch_fallback(
            context,
            &patch_file,
            &format!("invalid patch plan: {}", err),
        )
        .await;
        return Ok(false);
    }

    let planned_tree_checksum = compute_tree_checksum_from_segment_checksums(
        patch_ops.iter().map(|op| op.target_checksum.as_str()),
    );
    if !checksum_matches(&artifact.new_file_remote_checksum, &planned_tree_checksum) {
        mark_patch_fallback(
            context,
            &patch_file,
            &format!(
                "patch plan checksum mismatch expected={} planned={}",
                artifact.new_file_remote_checksum, planned_tree_checksum
            ),
        )
        .await;
        return Ok(false);
    }

    let preflight =
        match preflight_copy_sources(Path::new(&artifact.local_target_path), &patch_ops).await {
            Ok(stats) => stats,
            Err(err) => {
                mark_patch_fallback(
                    context,
                    &patch_file,
                    &format!("copy source preflight failed: {}", err),
                )
                .await;
                return Ok(false);
            }
        };
    if preflight.copy_ops_total > 0 {
        debug!(
            "Delta copy-source preflight: file_id={} sampled_ops={}/{} sampled_bytes={}/{} mismatches={} mismatch_bytes={}",
            patch_file.file_id,
            preflight.checked_ops,
            preflight.copy_ops_total,
            preflight.checked_bytes,
            preflight.copy_bytes_total,
            preflight.mismatch_ops,
            preflight.mismatch_bytes
        );
    }
    if preflight.mismatch_ops > 0 {
        mark_patch_fallback(
            context,
            &patch_file,
            &format!(
                "copy source preflight mismatch sampled_ops={}/{} mismatches={} sampled_bytes={}/{} mismatch_bytes={}",
                preflight.checked_ops,
                preflight.copy_ops_total,
                preflight.mismatch_ops,
                preflight.checked_bytes,
                preflight.copy_bytes_total,
                preflight.mismatch_bytes
            ),
        )
        .await;
        return Ok(false);
    }

    let preflight_elapsed = patch_started.elapsed();

    if let Err(err) =
        update_download_patch_file_status(context.clone(), file_id, PATCH_STATUS_DOWNLOADING, None)
            .await
    {
        warn!(
            "Failed to update patch file {} status to DOWNLOADING: {}",
            file_id, err
        );
    } else {
        debug!(
            "Delta patch state transition: file_id={} status={} elapsed={:.2?}",
            file_id,
            PATCH_STATUS_DOWNLOADING,
            patch_started.elapsed()
        );
    }

    // Use parallel insert-op downloads - non-overlapping blob offsets allow
    // concurrent random-access writes, improving throughput for patchable files.
    const PATCH_PARALLEL_CONCURRENCY: usize = 4;
    if let Err(err) = download_patch_blob_ranges_parallel(
        context.clone(),
        &artifact,
        &patch_file,
        &mut patch_ops,
        download_pause_rx.clone(),
        cancel_rx.clone(),
        PATCH_PARALLEL_CONCURRENCY,
        rate_limiter,
        metrics,
    )
    .await
    {
        mark_patch_fallback(
            context,
            &patch_file,
            &format!("patch blob range download failed: {}", err),
        )
        .await;
        return Ok(false);
    }

    let download_elapsed = patch_started.elapsed();

    if let Err(err) =
        update_download_patch_file_status(context.clone(), file_id, PATCH_STATUS_READY, None).await
    {
        warn!(
            "Failed to update patch file {} status to READY: {}",
            file_id, err
        );
    } else {
        debug!(
            "Delta patch state transition: file_id={} status={} elapsed={:.2?}",
            file_id,
            PATCH_STATUS_READY,
            patch_started.elapsed()
        );
    }
    if let Err(err) =
        update_download_patch_file_status(context.clone(), file_id, PATCH_STATUS_APPLYING, None)
            .await
    {
        warn!(
            "Failed to update patch file {} status to APPLYING: {}",
            file_id, err
        );
    } else {
        debug!(
            "Delta patch state transition: file_id={} status={} elapsed={:.2?}",
            file_id,
            PATCH_STATUS_APPLYING,
            patch_started.elapsed()
        );
    }

    let tmp_path_for_cleanup = PathBuf::from(format!("{}.foxy.tmp", artifact.local_target_path));
    let (temp_path, segment_checksums) = match apply_patch_to_temp_file(
        context.clone(),
        &artifact,
        &patch_file,
        &patch_ops,
        download_pause_rx,
        cancel_rx,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            // Clean up the orphaned .foxy.tmp file before falling back
            if let Err(cleanup_err) = fs::remove_file(&tmp_path_for_cleanup).await
                && cleanup_err.kind() != std::io::ErrorKind::NotFound
            {
                warn!(
                    "Failed to clean up temp file {} after apply failure: {}",
                    tmp_path_for_cleanup.display(),
                    cleanup_err
                );
            }
            mark_patch_fallback(
                context,
                &patch_file,
                &format!("patch apply failed: {}", err),
            )
            .await;
            return Ok(false);
        }
    };

    let apply_elapsed = patch_started.elapsed();

    let backup_path = match promote_temp_file_atomically(
        &artifact.local_target_path,
        &temp_path,
        download_target.file_id,
        rollback_session.clone(),
    )
    .await
    {
        Ok(path) => path,
        Err(err) => {
            let _ = fs::remove_file(&temp_path).await;
            mark_patch_fallback(
                context,
                &patch_file,
                &format!("patch promote failed: {}", err),
            )
            .await;
            return Ok(false);
        }
    };

    let target_path = PathBuf::from(&artifact.local_target_path);
    // Compute tree checksum from segment checksums collected during apply -
    // avoids re-reading the entire output file from disk.
    let final_tree_checksum =
        compute_tree_checksum_from_segment_checksums(segment_checksums.iter().map(|s| s.as_str()));

    if !checksum_matches(&artifact.new_file_remote_checksum, &final_tree_checksum) {
        let output_file_md5 = compute_file_integrity_hash(&target_path).await.ok();
        match diagnose_patch_output_segments(&target_path, &patch_ops, 24).await {
            Ok(mismatches) => warn!(
                "Delta patch output segment diagnostics for file_id={} found {} mismatched ops",
                patch_file.file_id, mismatches
            ),
            Err(err) => warn!(
                "Failed to run delta segment diagnostics for file_id={}: {}",
                patch_file.file_id, err
            ),
        }
        if let Some(backup_path) = backup_path.as_ref() {
            if let Err(restore_err) = restore_backup(&target_path, backup_path).await {
                warn!(
                    "Failed to restore backup after checksum mismatch for file_id={}: {}",
                    patch_file.file_id, restore_err
                );
            }
        } else if let Some(session) = rollback_session.as_ref() {
            let mut rollback = session.lock().await;
            if let Err(restore_err) = rollback
                .restore_entry(download_target.file_id, &target_path)
                .await
            {
                warn!(
                    "Failed to restore rollback backup after checksum mismatch for file_id={}: {}",
                    patch_file.file_id, restore_err
                );
            }
        }
        // Clean up the .foxy.tmp file (it was already renamed to target, so
        // it exists as the target now; after restore, the temp is gone).
        // But ensure we also remove any lingering .foxy.tmp:
        let _ = fs::remove_file(&tmp_path_for_cleanup).await;
        mark_patch_fallback(
            context,
            &patch_file,
            &format!(
                "final tree checksum mismatch expected={} actual={} output_md5={}",
                artifact.new_file_remote_checksum,
                final_tree_checksum,
                output_file_md5.unwrap_or_else(|| "unavailable".to_string())
            ),
        )
        .await;
        return Ok(false);
    }

    debug!(
        "Delta patch final tree checksum verified for file_id={} checksum={}",
        patch_file.file_id, final_tree_checksum
    );

    if let Some(backup_path) = backup_path.as_ref()
        && let Err(err) = fs::remove_file(backup_path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        warn!(
            "Failed to remove patch backup file for file_id={}: {}",
            patch_file.file_id, err
        );
    }

    if let Err(err) =
        update_download_patch_file_status(context.clone(), file_id, PATCH_STATUS_DONE, None).await
    {
        warn!(
            "Failed to set patch status done for file_id={}: {}",
            patch_file.file_id, err
        );
    }

    if let Err(err) =
        cleanup_patch_artifacts(&patch_file.patch_json_path, &patch_file.patch_blob_path).await
    {
        warn!(
            "Failed to clean patch artifacts for successful patch file_id={}: {}",
            patch_file.file_id, err
        );
    }
    if let Err(err) = delete_download_patch_ops_for_file(context.clone(), file_id).await {
        warn!(
            "Failed to delete patch ops for successful patch file_id={}: {}",
            patch_file.file_id, err
        );
    }
    if let Err(err) = delete_download_patch_file_by_file_id(context.clone(), file_id).await {
        warn!(
            "Failed to delete patch row for successful patch file_id={}: {}",
            patch_file.file_id, err
        );
    }

    let total_elapsed = patch_started.elapsed();
    let download_phase = download_elapsed.saturating_sub(preflight_elapsed);
    let apply_phase = apply_elapsed.saturating_sub(download_elapsed);
    let verify_promote_phase = total_elapsed.saturating_sub(apply_elapsed);
    let savings_bytes = artifact
        .new_file_expected_size
        .saturating_sub(planned_download_bytes);
    let savings_percent = savings_bytes
        .saturating_mul(100)
        .checked_div(artifact.new_file_expected_size)
        .unwrap_or(0);
    info!(
        "Delta patch applied successfully: file_id={} local_path={} total_elapsed={:.2?} preflight={:.2?} download={:.2?} apply={:.2?} verify_promote={:.2?} planned_download_bytes={} full_bytes={} savings_bytes={} savings_percent={}%",
        patch_file.file_id,
        artifact.local_target_path,
        total_elapsed,
        preflight_elapsed,
        download_phase,
        apply_phase,
        verify_promote_phase,
        planned_download_bytes,
        artifact.new_file_expected_size,
        savings_bytes,
        savings_percent
    );
    Ok(true)
}
