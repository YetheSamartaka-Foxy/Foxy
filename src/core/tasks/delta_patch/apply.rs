use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::DownloadPatchFile;
use crate::core::models::download_patch_op::DownloadPatchOp;
use crate::core::tasks::download_files::SharedRollbackSession;
use anyhow::{Context, anyhow};
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use super::transfer::{
    copy_range_with_hash, download_range_to_output, hash_file_segment, wait_for_download_resume,
};
use super::types::{PatchArtifact, PatchOpType, checksum_matches, should_abort_copy_fallback};
pub(super) fn validate_runtime_ops(
    ops: &[DownloadPatchOp],
    expected_len: u64,
) -> anyhow::Result<()> {
    if ops.is_empty() {
        return Err(anyhow!("patch operation list is empty"));
    }
    if ops[0].dest_start != 0 {
        return Err(anyhow!(
            "patch operation list starts at {}, expected 0",
            ops[0].dest_start
        ));
    }

    let mut cursor = 0_u64;
    for op in ops {
        if op.length == 0 {
            return Err(anyhow!("patch op {} has zero length", op.data_order));
        }
        if op.dest_start != cursor {
            return Err(anyhow!(
                "non-contiguous patch op {}: expected start {}, got {}",
                op.data_order,
                cursor,
                op.dest_start
            ));
        }
        cursor = cursor
            .checked_add(op.length)
            .ok_or_else(|| anyhow!("patch op {} causes offset overflow", op.data_order))?;
    }

    if cursor != expected_len {
        return Err(anyhow!(
            "patch op coverage mismatch: expected {}, got {}",
            expected_len,
            cursor
        ));
    }

    Ok(())
}

pub(super) async fn diagnose_patch_output_segments(
    target_path: &Path,
    patch_ops: &[DownloadPatchOp],
    max_logged: usize,
) -> anyhow::Result<usize> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(target_path)
        .await
        .with_context(|| format!("failed to open patched output {}", target_path.display()))?;

    let mut mismatches = 0usize;
    let mut io_buf = Vec::new();
    for op in patch_ops {
        let actual = hash_file_segment(
            &mut file,
            op.dest_start,
            op.length,
            &mut io_buf,
            &op.target_checksum,
        )
        .await?;
        if checksum_matches(&op.target_checksum, &actual) {
            continue;
        }

        mismatches = mismatches.saturating_add(1);
        if mismatches <= max_logged {
            warn!(
                "Delta output segment mismatch: file_id={} op={} type={} dest_start={} length={} expected={} actual={} source_start={:?} blob_offset={:?}",
                op.file_id,
                op.data_order,
                op.op_type,
                op.dest_start,
                op.length,
                op.target_checksum,
                actual,
                op.source_start,
                op.blob_offset
            );
        }
    }

    if mismatches > max_logged {
        warn!(
            "Delta output segment mismatch logging truncated: total={} omitted={}",
            mismatches,
            mismatches - max_logged
        );
    }

    Ok(mismatches)
}

pub(crate) async fn apply_patch_to_temp_file(
    context: Arc<FoxyContext>,
    artifact: &PatchArtifact,
    patch_file: &DownloadPatchFile,
    patch_ops: &[DownloadPatchOp],
    mut download_pause_rx: watch::Receiver<bool>,
    mut cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<(PathBuf, Vec<String>)> {
    let local_target_path = PathBuf::from(&artifact.local_target_path);
    let tmp_path = PathBuf::from(format!("{}.foxy.tmp", artifact.local_target_path));

    let old_meta = fs::metadata(&local_target_path).await.with_context(|| {
        format!(
            "base file does not exist or is inaccessible: {}",
            local_target_path.display()
        )
    })?;
    let old_len = old_meta.len();

    let mut old_file = OpenOptions::new()
        .read(true)
        .open(&local_target_path)
        .await
        .with_context(|| format!("failed to open old file {}", local_target_path.display()))?;

    let output_raw = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(&tmp_path)
        .await
        .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?;
    output_raw
        .set_len(artifact.new_file_expected_size)
        .await
        .context("failed to size temp output file")?;
    let mut output_file = tokio::io::BufWriter::with_capacity(1024 * 1024, output_raw);

    let mut blob_file = OpenOptions::new()
        .read(true)
        .open(&patch_file.patch_blob_path)
        .await
        .with_context(|| format!("failed to open patch blob {}", patch_file.patch_blob_path))?;

    let mut segment_checksums: Vec<String> = Vec::with_capacity(patch_ops.len());
    let mut io_buf = Vec::new();

    let copy_ops_total = patch_ops
        .iter()
        .filter(|op| PatchOpType::CopyLocal.matches(op))
        .count();
    let mut attempted_copy_ops = 0usize;
    let mut attempted_copy_bytes = 0_u64;
    let mut fallback_copy_ops = 0usize;
    let mut fallback_copy_bytes = 0_u64;

    let apply_phase_started = std::time::Instant::now();

    for op in patch_ops {
        let Some(op_type) = PatchOpType::from_str(&op.op_type) else {
            return Err(anyhow!("unsupported patch op type {}", op.op_type));
        };

        wait_for_download_resume(&mut download_pause_rx, &mut cancel_rx).await?;
        let op_started = std::time::Instant::now();
        debug!(
            "Applying delta op: file_id={} op={} type={} dest_start={} length={} target_checksum={}",
            op.file_id, op.data_order, op.op_type, op.dest_start, op.length, op.target_checksum
        );

        match op_type {
            PatchOpType::CopyLocal => {
                attempted_copy_ops = attempted_copy_ops.saturating_add(1);
                attempted_copy_bytes = attempted_copy_bytes.saturating_add(op.length);
                let Some(source_start) = op.source_start else {
                    return Err(anyhow!("copy op {} missing source_start", op.data_order));
                };
                let Some(source_checksum) = op.source_checksum.as_ref() else {
                    return Err(anyhow!("copy op {} missing source_checksum", op.data_order));
                };

                let source_end = source_start
                    .checked_add(op.length)
                    .ok_or_else(|| anyhow!("copy op {} source overflow", op.data_order))?;
                let copy_valid = source_end <= old_len;

                let copied_checksum = if copy_valid {
                    match copy_range_with_hash(
                        &mut old_file,
                        source_start,
                        &mut output_file,
                        op.dest_start,
                        op.length,
                        &mut io_buf,
                        &op.target_checksum,
                    )
                    .await
                    {
                        Ok(checksum) => Some(checksum),
                        Err(err) => {
                            warn!(
                                "Copy op {} failed ({}), falling back to range download",
                                op.data_order, err
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                // Verify against target_checksum (the expected output), not
                // source_checksum, so the check remains correct even if the
                // two checksums diverge due to planning edge cases.
                let checksum_ok = copied_checksum
                    .as_ref()
                    .map(|actual| checksum_matches(&op.target_checksum, actual))
                    .unwrap_or(false);

                if !checksum_ok {
                    fallback_copy_ops = fallback_copy_ops.saturating_add(1);
                    fallback_copy_bytes = fallback_copy_bytes.saturating_add(op.length);
                    warn!(
                        "Copy op fallback to remote range: file_id={} op={} source_start={:?} dest_start={} length={} source_checksum={} copied_checksum={:?} copy_valid={}",
                        op.file_id,
                        op.data_order,
                        op.source_start,
                        op.dest_start,
                        op.length,
                        source_checksum,
                        copied_checksum,
                        copy_valid
                    );
                    if should_abort_copy_fallback(
                        attempted_copy_ops,
                        attempted_copy_bytes,
                        fallback_copy_ops,
                        fallback_copy_bytes,
                    ) {
                        let fallback_ops_percent = (fallback_copy_ops as u64).saturating_mul(100)
                            / attempted_copy_ops as u64;
                        let fallback_bytes_percent = fallback_copy_bytes
                            .saturating_mul(100)
                            .checked_div(attempted_copy_bytes)
                            .unwrap_or(0);
                        return Err(anyhow!(
                            "aborting delta apply due widespread copy fallback: file_id={} fallback_ops={}/{} ({}%) total_copy_ops={} fallback_bytes={}/{} ({}%)",
                            op.file_id,
                            fallback_copy_ops,
                            attempted_copy_ops,
                            fallback_ops_percent,
                            copy_ops_total,
                            fallback_copy_bytes,
                            attempted_copy_bytes,
                            fallback_bytes_percent
                        ));
                    }
                    download_range_to_output(
                        context.clone(),
                        &artifact.remote_url,
                        op.dest_start,
                        op.length,
                        &op.target_checksum,
                        &mut output_file,
                        &mut download_pause_rx,
                        &mut cancel_rx,
                    )
                    .await?;
                } else if let Some(actual_checksum) = copied_checksum {
                    debug!(
                        "Copy op applied from local source: file_id={} op={} source_start={} dest_start={} length={} checksum={} elapsed={:.2?}",
                        op.file_id,
                        op.data_order,
                        source_start,
                        op.dest_start,
                        op.length,
                        actual_checksum,
                        op_started.elapsed()
                    );
                }
            }
            PatchOpType::InsertRemote => {
                let Some(blob_offset) = op.blob_offset else {
                    return Err(anyhow!("insert op {} missing blob_offset", op.data_order));
                };

                let copied_checksum = copy_range_with_hash(
                    &mut blob_file,
                    blob_offset,
                    &mut output_file,
                    op.dest_start,
                    op.length,
                    &mut io_buf,
                    &op.target_checksum,
                )
                .await;

                match copied_checksum {
                    Ok(actual_checksum)
                        if checksum_matches(&op.target_checksum, &actual_checksum) =>
                    {
                        debug!(
                            "Insert op applied from patch blob: file_id={} op={} blob_offset={} dest_start={} length={} checksum={} elapsed={:.2?}",
                            op.file_id,
                            op.data_order,
                            blob_offset,
                            op.dest_start,
                            op.length,
                            actual_checksum,
                            op_started.elapsed()
                        );
                    }
                    Ok(actual_checksum) => {
                        warn!(
                            "Insert op blob checksum mismatch, downloading fallback range: file_id={} op={} blob_offset={} dest_start={} length={} expected={} actual={}",
                            op.file_id,
                            op.data_order,
                            blob_offset,
                            op.dest_start,
                            op.length,
                            op.target_checksum,
                            actual_checksum
                        );
                        download_range_to_output(
                            context.clone(),
                            &artifact.remote_url,
                            op.dest_start,
                            op.length,
                            &op.target_checksum,
                            &mut output_file,
                            &mut download_pause_rx,
                            &mut cancel_rx,
                        )
                        .await?;
                    }
                    Err(err) => {
                        warn!(
                            "Insert op blob read failed, downloading fallback range: file_id={} op={} blob_offset={} dest_start={} length={} error={}",
                            op.file_id, op.data_order, blob_offset, op.dest_start, op.length, err
                        );
                        download_range_to_output(
                            context.clone(),
                            &artifact.remote_url,
                            op.dest_start,
                            op.length,
                            &op.target_checksum,
                            &mut output_file,
                            &mut download_pause_rx,
                            &mut cancel_rx,
                        )
                        .await?;
                    }
                }
            }
        }

        let op_elapsed = op_started.elapsed();
        if op_elapsed > std::time::Duration::from_millis(500) {
            info!(
                "Slow delta op: file_id={} op={} type={} length={} elapsed={:.2?}",
                op.file_id, op.data_order, op.op_type, op.length, op_elapsed
            );
        }

        // Every path above ensures the segment matches target_checksum:
        // - CopyLocal success: verified against target_checksum
        // - CopyLocal/InsertRemote fallback: download_range_to_output verifies target_checksum
        // - InsertRemote success: verified against target_checksum directly
        segment_checksums.push(op.target_checksum.clone());
    }

    info!(
        "Delta patch apply completed: file_id={} ops={} copy_ops_attempted={} copy_bytes={} fallback_copy_ops={} fallback_copy_bytes={} output_size={} elapsed={:.2?}",
        artifact.file_id,
        patch_ops.len(),
        attempted_copy_ops,
        attempted_copy_bytes,
        fallback_copy_ops,
        fallback_copy_bytes,
        artifact.new_file_expected_size,
        apply_phase_started.elapsed()
    );

    output_file
        .flush()
        .await
        .context("failed to flush temp file")?;
    output_file
        .get_ref()
        .sync_all()
        .await
        .context("failed to fsync temp file")?;

    Ok((tmp_path, segment_checksums))
}

pub(crate) async fn promote_temp_file_atomically(
    local_target_path: &str,
    temp_path: &Path,
    file_id: u64,
    rollback_session: Option<SharedRollbackSession>,
) -> anyhow::Result<Option<PathBuf>> {
    let target_path = PathBuf::from(local_target_path);
    if let Some(session) = rollback_session {
        let mut rollback = session.lock().await;
        rollback
            .promote_file(file_id, temp_path, &target_path)
            .await?;
        return Ok(None);
    }

    let backup_path = PathBuf::from(format!("{}.foxy.bak", local_target_path));

    // Remove stale backup unconditionally - handle NotFound gracefully
    // instead of a TOCTOU exists() check.
    match fs::remove_file(&backup_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            warn!(
                "Failed to remove stale backup before promote: {}: {}",
                backup_path.display(),
                err
            );
        }
    }

    fs::rename(&target_path, &backup_path)
        .await
        .with_context(|| {
            format!(
                "failed to create backup before patch promote: {} -> {}",
                target_path.display(),
                backup_path.display()
            )
        })?;

    if let Err(err) = fs::rename(temp_path, &target_path).await {
        let _ = fs::rename(&backup_path, &target_path).await;
        return Err(anyhow!(err)).context("failed to promote patched temp file");
    }

    Ok(Some(backup_path))
}

pub(crate) async fn cleanup_patch_artifacts(
    patch_json_path: &str,
    patch_blob_path: &str,
) -> anyhow::Result<()> {
    if !patch_json_path.is_empty() {
        match fs::remove_file(patch_json_path).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow!(err))
                    .context(format!("failed to remove patch json {}", patch_json_path));
            }
        }
    }

    if !patch_blob_path.is_empty() {
        match fs::remove_file(patch_blob_path).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow!(err))
                    .context(format!("failed to remove patch blob {}", patch_blob_path));
            }
        }
    }

    Ok(())
}

/// Compute a whole-file integrity hash for diagnostic logging.
/// Uses BLAKE3 (local-only, never compared to remote server checksums).
pub(super) async fn compute_file_integrity_hash(path: &Path) -> anyhow::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::core::utils::content_hash::blake3_file_hash(&path)
            .map_err(|e| anyhow::anyhow!("failed to hash {}: {}", path.display(), e))
    })
    .await
    .context("failed to join file hashing task")?
}

pub(super) async fn restore_backup(target_path: &Path, backup_path: &Path) -> anyhow::Result<()> {
    // Remove the failed patched file unconditionally - handle NotFound gracefully
    // instead of a TOCTOU exists() check.
    match fs::remove_file(target_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(anyhow!(err)).context(format!(
                "failed to remove patched file {} before restore",
                target_path.display()
            ));
        }
    }

    fs::rename(backup_path, target_path)
        .await
        .with_context(|| {
            format!(
                "failed to restore backup {} -> {}",
                backup_path.display(),
                target_path.display()
            )
        })?;

    Ok(())
}
