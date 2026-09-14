use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::DownloadPatchFile;
use crate::core::models::download_patch_op::DownloadPatchOp;
use crate::core::tasks::download_files::SharedRollbackSession;
use crate::core::utils::content_hash::FlexHasher;
use crate::core::utils::file_io::{read_at, write_at};
use anyhow::{Context, anyhow};
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::sync::watch;

use super::transfer::{download_range_to_output, hash_file_segment, wait_for_download_resume};
use super::types::{
    ApplySegment, PatchArtifact, PatchOpType, RUN_COPY_BUFFER_SIZE, checksum_matches,
    coalesce_apply_segments, should_abort_copy_fallback,
};
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

/// One op inside a copy run, carried into the blocking copy so part
/// boundaries are hashed inside the sequential stream.
#[derive(Debug, Clone)]
pub(super) struct RunPart {
    pub(super) op_idx: usize,
    pub(super) length: u64,
    pub(super) expected_checksum: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunStop {
    Cancelled,
    /// The part that was in flight restarts from its beginning on resume.
    Paused {
        next_part: usize,
    },
}

pub(super) struct RunOutcome {
    /// `(index within the run, streamed checksum)` for every part completed.
    pub(super) hashes: Vec<(usize, String)>,
    pub(super) stop: Option<RunStop>,
}

/// Copy `parts[first_part..]` as one sequential stream from `source` to
/// `output`, hashing each part boundary on the way. Cancel and pause are
/// checked per chunk, never per op, so a multi-hundred-megabyte run still
/// stops promptly. Positional I/O: nothing seeks.
#[allow(clippy::too_many_arguments)]
pub(super) fn copy_run_blocking(
    source: &std::fs::File,
    output: &std::fs::File,
    source_start: u64,
    dest_start: u64,
    parts: &[RunPart],
    first_part: usize,
    cancel_rx: &watch::Receiver<bool>,
    download_pause_rx: &watch::Receiver<bool>,
) -> std::io::Result<RunOutcome> {
    let skipped: u64 = parts[..first_part].iter().map(|part| part.length).sum();
    let total: u64 = parts[first_part..].iter().map(|part| part.length).sum();
    let mut hashes = Vec::with_capacity(parts.len() - first_part);
    if total == 0 {
        return Ok(RunOutcome { hashes, stop: None });
    }

    let mut source_pos = source_start.saturating_add(skipped);
    let mut dest_pos = dest_start.saturating_add(skipped);
    let mut buffer = vec![0u8; total.min(RUN_COPY_BUFFER_SIZE as u64) as usize];
    let mut part_idx = first_part;
    let mut hasher = Some(FlexHasher::from_checksum(
        &parts[part_idx].expected_checksum,
    ));
    let mut remaining_in_part = parts[part_idx].length;
    let mut remaining = total;

    while remaining > 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        read_at(source, source_pos, &mut buffer[..take])?;
        write_at(output, dest_pos, &buffer[..take])?;
        source_pos += take as u64;
        dest_pos += take as u64;
        remaining -= take as u64;

        let mut slice = &buffer[..take];
        while !slice.is_empty() {
            let n = remaining_in_part.min(slice.len() as u64) as usize;
            if let Some(hasher) = hasher.as_mut() {
                hasher.update(&slice[..n]);
            }
            slice = &slice[n..];
            remaining_in_part -= n as u64;
            if remaining_in_part == 0 {
                if let Some(done) = hasher.take() {
                    hashes.push((part_idx, done.finalize_hex()));
                }
                part_idx += 1;
                if part_idx < parts.len() {
                    hasher = Some(FlexHasher::from_checksum(
                        &parts[part_idx].expected_checksum,
                    ));
                    remaining_in_part = parts[part_idx].length;
                }
            }
        }

        if remaining > 0 {
            if *cancel_rx.borrow() {
                return Ok(RunOutcome {
                    hashes,
                    stop: Some(RunStop::Cancelled),
                });
            }
            if *download_pause_rx.borrow() {
                return Ok(RunOutcome {
                    hashes,
                    stop: Some(RunStop::Paused {
                        next_part: part_idx,
                    }),
                });
            }
        }
    }

    Ok(RunOutcome { hashes, stop: None })
}

/// Stream a copy run, resuming after pauses, and return the streamed checksum
/// of every part (`None` for a part whose source range lies outside the file).
#[allow(clippy::too_many_arguments)]
async fn copy_run_with_hashes(
    source: Arc<std::fs::File>,
    source_len: u64,
    output: Arc<std::fs::File>,
    source_start: u64,
    dest_start: u64,
    parts: Arc<Vec<RunPart>>,
    download_pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<Vec<Option<String>>> {
    let mut hashes: Vec<Option<String>> = vec![None; parts.len()];
    // Source ranges are contiguous, so once one part runs past the file every
    // later part does too; only the leading valid prefix is streamed.
    let mut readable_parts = 0usize;
    let mut cursor = source_start;
    for part in parts.iter() {
        match cursor.checked_add(part.length) {
            Some(end) if end <= source_len => {
                readable_parts += 1;
                cursor = end;
            }
            _ => break,
        }
    }
    if readable_parts == 0 {
        return Ok(hashes);
    }
    let readable: Arc<Vec<RunPart>> = if readable_parts == parts.len() {
        parts
    } else {
        Arc::new(parts[..readable_parts].to_vec())
    };

    let mut next_part = 0usize;
    loop {
        wait_for_download_resume(download_pause_rx, cancel_rx).await?;
        let source = source.clone();
        let output = output.clone();
        let parts = readable.clone();
        let cancel = cancel_rx.clone();
        let pause = download_pause_rx.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            copy_run_blocking(
                &source,
                &output,
                source_start,
                dest_start,
                &parts,
                next_part,
                &cancel,
                &pause,
            )
        })
        .await
        .context("copy run task failed")??;
        for (part_idx, checksum) in outcome.hashes {
            hashes[part_idx] = Some(checksum);
        }
        match outcome.stop {
            None => return Ok(hashes),
            Some(RunStop::Cancelled) => anyhow::bail!("download cancelled"),
            Some(RunStop::Paused { next_part: resume }) => next_part = resume,
        }
    }
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

    let segments = coalesce_apply_segments(patch_ops)?;

    let old_meta = fs::metadata(&local_target_path).await.with_context(|| {
        format!(
            "base file does not exist or is inaccessible: {}",
            local_target_path.display()
        )
    })?;
    let old_len = old_meta.len();

    let old_file = Arc::new(
        std::fs::OpenOptions::new()
            .read(true)
            .open(&local_target_path)
            .with_context(|| format!("failed to open old file {}", local_target_path.display()))?,
    );

    let output_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(&tmp_path)
        .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?;
    output_file
        .set_len(artifact.new_file_expected_size)
        .context("failed to size temp output file")?;
    let output_file = Arc::new(output_file);

    let blob_file = Arc::new(
        std::fs::OpenOptions::new()
            .read(true)
            .open(&patch_file.patch_blob_path)
            .with_context(|| format!("failed to open patch blob {}", patch_file.patch_blob_path))?,
    );

    let mut segment_checksums: Vec<String> = vec![String::new(); patch_ops.len()];

    let copy_ops_total = patch_ops
        .iter()
        .filter(|op| PatchOpType::CopyLocal.matches(op))
        .count();
    let copy_runs = segments
        .iter()
        .filter(|segment| matches!(segment, ApplySegment::CopyRun { .. }))
        .count();
    let mut attempted_copy_ops = 0usize;
    let mut attempted_copy_bytes = 0_u64;
    let mut fallback_copy_ops = 0usize;
    let mut fallback_copy_bytes = 0_u64;

    let apply_phase_started = std::time::Instant::now();
    debug!(
        "Delta apply plan: file_id={} ops={} copy_ops={} copy_runs={} insert_ops={}",
        artifact.file_id,
        patch_ops.len(),
        copy_ops_total,
        copy_runs,
        patch_ops.len().saturating_sub(copy_ops_total)
    );

    for segment in &segments {
        wait_for_download_resume(&mut download_pause_rx, &mut cancel_rx).await?;
        let segment_started = std::time::Instant::now();

        match segment {
            ApplySegment::CopyRun {
                op_range,
                source_start,
                dest_start,
                length,
            } => {
                let parts: Arc<Vec<RunPart>> = Arc::new(
                    op_range
                        .clone()
                        .map(|op_idx| RunPart {
                            op_idx,
                            length: patch_ops[op_idx].length,
                            expected_checksum: patch_ops[op_idx].target_checksum.clone(),
                        })
                        .collect(),
                );
                let streamed = copy_run_with_hashes(
                    old_file.clone(),
                    old_len,
                    output_file.clone(),
                    *source_start,
                    *dest_start,
                    parts.clone(),
                    &mut download_pause_rx,
                    &mut cancel_rx,
                )
                .await;
                let streamed = match streamed {
                    Ok(hashes) => hashes,
                    Err(err) if *cancel_rx.borrow() => return Err(err),
                    Err(err) => {
                        warn!(
                            "Copy run failed ({}), falling back to range downloads: file_id={} ops={}..{} source_start={} dest_start={} length={}",
                            err,
                            artifact.file_id,
                            op_range.start,
                            op_range.end,
                            source_start,
                            dest_start,
                            length
                        );
                        vec![None; parts.len()]
                    }
                };

                for (run_idx, part) in parts.iter().enumerate() {
                    let op = &patch_ops[part.op_idx];
                    attempted_copy_ops = attempted_copy_ops.saturating_add(1);
                    attempted_copy_bytes = attempted_copy_bytes.saturating_add(op.length);
                    let copied_checksum = streamed[run_idx].as_deref();
                    // Verify against target_checksum (the expected output), not
                    // source_checksum, so the check remains correct even if the
                    // two checksums diverge due to planning edge cases.
                    let checksum_ok = copied_checksum
                        .is_some_and(|actual| checksum_matches(&op.target_checksum, actual));
                    if !checksum_ok {
                        fallback_copy_ops = fallback_copy_ops.saturating_add(1);
                        fallback_copy_bytes = fallback_copy_bytes.saturating_add(op.length);
                        warn!(
                            "Copy op fallback to remote range: file_id={} op={} source_start={:?} dest_start={} length={} source_checksum={:?} copied_checksum={:?}",
                            op.file_id,
                            op.data_order,
                            op.source_start,
                            op.dest_start,
                            op.length,
                            op.source_checksum,
                            copied_checksum
                        );
                        if should_abort_copy_fallback(
                            attempted_copy_ops,
                            attempted_copy_bytes,
                            fallback_copy_ops,
                            fallback_copy_bytes,
                        ) {
                            let fallback_ops_percent = (fallback_copy_ops as u64)
                                .saturating_mul(100)
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
                            output_file.clone(),
                            &mut download_pause_rx,
                            &mut cancel_rx,
                        )
                        .await?;
                    }
                    segment_checksums[part.op_idx] = op.target_checksum.clone();
                }

                let elapsed = segment_started.elapsed();
                if elapsed > std::time::Duration::from_millis(500) {
                    info!(
                        "Slow delta op: file_id={} op={} type=copy_run ops={} length={} elapsed={:.2?}",
                        artifact.file_id,
                        patch_ops[op_range.start].data_order,
                        op_range.len(),
                        length,
                        elapsed
                    );
                }
            }
            ApplySegment::Insert { op_idx } => {
                let op = &patch_ops[*op_idx];
                let blob_offset = op
                    .blob_offset
                    .ok_or_else(|| anyhow!("insert op {} missing blob_offset", op.data_order))?;
                let part = Arc::new(vec![RunPart {
                    op_idx: *op_idx,
                    length: op.length,
                    expected_checksum: op.target_checksum.clone(),
                }]);
                let blob_len = blob_offset.saturating_add(op.length);
                let copied = copy_run_with_hashes(
                    blob_file.clone(),
                    blob_len,
                    output_file.clone(),
                    blob_offset,
                    op.dest_start,
                    part,
                    &mut download_pause_rx,
                    &mut cancel_rx,
                )
                .await;

                match copied {
                    Ok(hashes)
                        if hashes.first().is_some_and(|actual| {
                            actual
                                .as_deref()
                                .is_some_and(|actual| checksum_matches(&op.target_checksum, actual))
                        }) =>
                    {
                        debug!(
                            "Insert op applied from patch blob: file_id={} op={} blob_offset={} dest_start={} length={} elapsed={:.2?}",
                            op.file_id,
                            op.data_order,
                            blob_offset,
                            op.dest_start,
                            op.length,
                            segment_started.elapsed()
                        );
                    }
                    Err(err) if *cancel_rx.borrow() => return Err(err),
                    other => {
                        match other {
                            Ok(hashes) => warn!(
                                "Insert op blob checksum mismatch, downloading fallback range: file_id={} op={} blob_offset={} dest_start={} length={} expected={} actual={:?}",
                                op.file_id,
                                op.data_order,
                                blob_offset,
                                op.dest_start,
                                op.length,
                                op.target_checksum,
                                hashes.first().cloned().flatten()
                            ),
                            Err(err) => warn!(
                                "Insert op blob read failed, downloading fallback range: file_id={} op={} blob_offset={} dest_start={} length={} error={}",
                                op.file_id,
                                op.data_order,
                                blob_offset,
                                op.dest_start,
                                op.length,
                                err
                            ),
                        }
                        download_range_to_output(
                            context.clone(),
                            &artifact.remote_url,
                            op.dest_start,
                            op.length,
                            &op.target_checksum,
                            output_file.clone(),
                            &mut download_pause_rx,
                            &mut cancel_rx,
                        )
                        .await?;
                    }
                }
                let elapsed = segment_started.elapsed();
                if elapsed > std::time::Duration::from_millis(500) {
                    info!(
                        "Slow delta op: file_id={} op={} type={} length={} elapsed={:.2?}",
                        op.file_id, op.data_order, op.op_type, op.length, elapsed
                    );
                }
                // Every path above ensures the segment matches target_checksum:
                // blob copy verified directly, fallback verified by download_range_to_output.
                segment_checksums[*op_idx] = op.target_checksum.clone();
            }
        }
    }

    info!(
        "Delta patch apply completed: file_id={} ops={} copy_runs={} copy_ops_attempted={} copy_bytes={} fallback_copy_ops={} fallback_copy_bytes={} output_size={} elapsed={:.2?}",
        artifact.file_id,
        patch_ops.len(),
        copy_runs,
        attempted_copy_ops,
        attempted_copy_bytes,
        fallback_copy_ops,
        fallback_copy_bytes,
        artifact.new_file_expected_size,
        apply_phase_started.elapsed()
    );

    let output_for_sync = output_file.clone();
    tokio::task::spawn_blocking(move || output_for_sync.sync_all())
        .await
        .context("failed to join temp file fsync")?
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
        let storage = crate::core::tasks::calculate_hashes::detect_storage_class_for_path(
            &path.to_string_lossy(),
        );
        let file_len = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let strategy = crate::core::utils::content_hash::select_blake3_read_strategy(
            crate::ui::types::HashIoProfilePreference::Auto,
            storage,
            file_len,
            &path,
        );
        crate::core::utils::content_hash::blake3_file_hash_with(&path, strategy)
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
