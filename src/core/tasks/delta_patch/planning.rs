use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::{DownloadPatchFile, save_download_patch_file};
use crate::core::models::download_patch_op::{
    DownloadPatchOp, replace_download_patch_ops_for_file,
};
use crate::core::models::modification_file::FoxyModFile;
use crate::core::models::modification_file_part::{FoxyModFilePart, part_display_path};
use crate::core::utils::app_paths;
use anyhow::{Context, anyhow};
use log::{debug, info};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};

use super::types::{
    PATCH_MIN_SAVINGS_PERCENT, PATCH_SCHEMA_VERSION, PATCH_STATUS_PLANNED, PatchArtifact,
    PatchOpType, PatchOperationArtifact, PlannedPatch, checksum_matches, expected_remote_end,
    infer_repository_url, normalize_checksum,
};
fn patch_paths_for_file(file_id: u64) -> (PathBuf, PathBuf) {
    let base = app_paths::foxy_large_payload_dir();
    (
        base.join(format!("file_{}.patch.json", file_id)),
        base.join(format!("file_{}.patch.bin", file_id)),
    )
}

pub(super) fn validate_plan_coverage(
    ops: &[PatchOperationArtifact],
    expected_len: u64,
) -> anyhow::Result<()> {
    if ops.is_empty() {
        return Err(anyhow!("patch plan has no operations"));
    }

    if ops[0].dest_start != 0 {
        return Err(anyhow!(
            "patch plan does not start at byte 0 (starts at {})",
            ops[0].dest_start
        ));
    }

    let mut cursor = 0_u64;
    for op in ops {
        if op.length == 0 {
            return Err(anyhow!("patch plan op {} has zero length", op.data_order));
        }

        if op.dest_start != cursor {
            return Err(anyhow!(
                "patch plan is non-contiguous at op {}: expected start {}, got {}",
                op.data_order,
                cursor,
                op.dest_start
            ));
        }

        cursor = cursor
            .checked_add(op.length)
            .ok_or_else(|| anyhow!("patch plan length overflow at op {}", op.data_order))?;
    }

    if cursor != expected_len {
        return Err(anyhow!(
            "patch plan end mismatch: expected {}, got {}",
            expected_len,
            cursor
        ));
    }

    Ok(())
}

pub(super) fn plan_savings_meet_threshold(download_bytes: u64, full_bytes: u64) -> bool {
    if full_bytes == 0 {
        return false;
    }
    if download_bytes >= full_bytes {
        return false;
    }
    let saved = full_bytes.saturating_sub(download_bytes);
    let savings_percent = saved.saturating_mul(100) / full_bytes;
    match PATCH_MIN_SAVINGS_PERCENT {
        0 => true,
        min_savings_percent => savings_percent >= min_savings_percent,
    }
}

pub(crate) fn plan_file_patch(
    file: &FoxyModFile,
    new_parts: &[FoxyModFilePart],
    old_parts: &[FoxyModFilePart],
) -> anyhow::Result<Option<PlannedPatch>> {
    let plan_started = std::time::Instant::now();
    if new_parts.is_empty() || file.length == 0 {
        return Ok(None);
    }

    let local_meta = match std::fs::metadata(&file.local_path) {
        Ok(meta) if meta.is_file() => meta,
        _ => return Ok(None),
    };
    let local_size = local_meta.len();

    let mut new_parts_sorted = new_parts.to_vec();
    new_parts_sorted.sort_by_key(|part| part.data_order);

    let mut old_parts_for_match: Vec<FoxyModFilePart> = old_parts
        .iter()
        .cloned()
        .map(|part| {
            part.with_derived_clean_local_state(&file.local_checksum, &file.remote_checksum)
        })
        .filter(|part| !part.local_checksum.trim().is_empty() && part.local_length > 0)
        .collect();
    if old_parts_for_match.is_empty() {
        old_parts_for_match = new_parts
            .iter()
            .cloned()
            .map(|part| {
                part.with_derived_clean_local_state(&file.local_checksum, &file.remote_checksum)
            })
            .filter(|part| !part.local_checksum.trim().is_empty() && part.local_length > 0)
            .collect();
    }
    if old_parts_for_match.is_empty() {
        debug!(
            "Delta plan skipped for file_id={} path={}: no local part metadata available",
            file.id, file.remote_path
        );
        return Ok(None);
    }

    let mut old_by_path: HashMap<String, Vec<usize>> = HashMap::new();
    let mut old_by_checksum_len: HashMap<(String, u64), Vec<usize>> = HashMap::new();
    for (idx, part) in old_parts_for_match.iter().enumerate() {
        old_by_path
            .entry(part_display_path(&part.path).to_string())
            .or_default()
            .push(idx);
        old_by_checksum_len
            .entry((normalize_checksum(&part.local_checksum), part.local_length))
            .or_default()
            .push(idx);
    }

    let mut used_old_idx = vec![false; old_parts_for_match.len()];
    let mut operations = Vec::with_capacity(new_parts_sorted.len());
    let mut planned_copy_bytes = 0_u64;
    let mut planned_download_bytes = 0_u64;
    let mut blob_offset = 0_u64;
    let mut copy_matches_by_path = 0usize;
    let mut copy_matches_by_checksum_pool = 0usize;
    let mut insert_ops = 0usize;

    for new_part in &new_parts_sorted {
        // Skip zero-length parts early - validate_plan_coverage would reject
        // them anyway, but catching them here avoids producing a broken plan
        // that falls through to a confusing validation error.
        if new_part.remote_length == 0 {
            return Ok(None);
        }
        let mut matched_idx = None;
        let mut matched_via_path = false;
        let target_checksum = normalize_checksum(&new_part.remote_checksum);
        let new_part_display_path = part_display_path(&new_part.path);
        if target_checksum.is_empty() {
            matched_idx = None;
        } else if let Some(path_candidates) = old_by_path.get(new_part_display_path) {
            for idx in path_candidates {
                if used_old_idx[*idx] {
                    continue;
                }
                let candidate = &old_parts_for_match[*idx];
                if candidate.local_length == new_part.remote_length
                    && checksum_matches(&candidate.local_checksum, &target_checksum)
                {
                    matched_idx = Some(*idx);
                    matched_via_path = true;
                    break;
                }
            }
        }

        if matched_idx.is_none() {
            let fallback_key = (target_checksum.clone(), new_part.remote_length);
            if let Some(pool) = old_by_checksum_len.get(&fallback_key) {
                for idx in pool {
                    if used_old_idx[*idx] {
                        continue;
                    }
                    matched_idx = Some(*idx);
                    break;
                }
            }
        }

        if let Some(idx) = matched_idx {
            used_old_idx[idx] = true;
            let candidate = &old_parts_for_match[idx];
            if matched_via_path {
                copy_matches_by_path = copy_matches_by_path.saturating_add(1);
            } else {
                copy_matches_by_checksum_pool = copy_matches_by_checksum_pool.saturating_add(1);
                debug!(
                    "Delta checksum-pool copy match for file_id={} path={}: new_part_path={} new_start={} old_part_path={} old_start={} length={}",
                    file.id,
                    file.remote_path,
                    new_part_display_path,
                    new_part.remote_start,
                    part_display_path(&candidate.path),
                    candidate.local_start,
                    new_part.remote_length
                );
            }
            operations.push(PatchOperationArtifact {
                data_order: new_part.data_order,
                op_type: PatchOpType::CopyLocal.as_str().to_string(),
                dest_start: new_part.remote_start,
                length: new_part.remote_length,
                target_checksum: target_checksum.clone(),
                source_start: Some(candidate.local_start),
                source_checksum: Some(normalize_checksum(&candidate.local_checksum)),
                blob_offset: None,
            });
            planned_copy_bytes = planned_copy_bytes.saturating_add(new_part.remote_length);
        } else {
            operations.push(PatchOperationArtifact {
                data_order: new_part.data_order,
                op_type: PatchOpType::InsertRemote.as_str().to_string(),
                dest_start: new_part.remote_start,
                length: new_part.remote_length,
                target_checksum: target_checksum.clone(),
                source_start: None,
                source_checksum: None,
                blob_offset: Some(blob_offset),
            });
            planned_download_bytes =
                match planned_download_bytes.checked_add(new_part.remote_length) {
                    Some(value) => value,
                    None => return Ok(None), // overflow - skip delta, fall back to full download
                };
            blob_offset = match blob_offset.checked_add(new_part.remote_length) {
                Some(value) => value,
                None => return Ok(None),
            };
            insert_ops = insert_ops.saturating_add(1);
        }
    }

    validate_plan_coverage(&operations, file.length)?;

    if planned_download_bytes >= file.length {
        debug!(
            "Delta plan skipped for file_id={} path={}: planned_download_bytes={} full_bytes={}",
            file.id, file.remote_path, planned_download_bytes, file.length
        );
        return Ok(None);
    }

    if !plan_savings_meet_threshold(planned_download_bytes, file.length) {
        debug!(
            "Delta plan skipped for file_id={} path={}: savings below threshold (planned_download_bytes={} full_bytes={} threshold={}%)",
            file.id,
            file.remote_path,
            planned_download_bytes,
            file.length,
            PATCH_MIN_SAVINGS_PERCENT
        );
        return Ok(None);
    }

    for op in &operations {
        if !PatchOpType::CopyLocal.matches(op) {
            continue;
        }
        let Some(source_start) = op.source_start else {
            return Ok(None);
        };
        let source_end = source_start.saturating_add(op.length);
        if source_end > local_size {
            debug!(
                "Delta plan skipped for file_id={} path={}: source range out of local file bounds (source_end={} local_size={})",
                file.id, file.remote_path, source_end, local_size
            );
            return Ok(None);
        }
    }

    let base_file_expected_size = expected_remote_end(old_parts).unwrap_or(local_size);
    let copy_ops = operations
        .iter()
        .filter(|op| PatchOpType::CopyLocal.matches(op))
        .count();
    let savings_bytes = file.length.saturating_sub(planned_download_bytes);
    let savings_percent = savings_bytes
        .saturating_mul(100)
        .checked_div(file.length)
        .unwrap_or(0);
    debug!(
        "Delta plan built for file_id={} path={}: old_parts={} new_parts={} ops={} copy_ops={} (path_match={} checksum_pool_match={}) insert_ops={} copy_bytes={} download_bytes={} savings_bytes={} savings_percent={}% elapsed={:.2?}",
        file.id,
        file.remote_path,
        old_parts_for_match.len(),
        new_parts_sorted.len(),
        operations.len(),
        copy_ops,
        copy_matches_by_path,
        copy_matches_by_checksum_pool,
        insert_ops,
        planned_copy_bytes,
        planned_download_bytes,
        savings_bytes,
        savings_percent,
        plan_started.elapsed()
    );

    Ok(Some(PlannedPatch {
        artifact: PatchArtifact {
            schema_version: PATCH_SCHEMA_VERSION,
            repository_url: infer_repository_url(&file.remote_path),
            file_id: file.id,
            local_target_path: file.local_path.clone(),
            remote_url: file.remote_path.clone(),
            base_file_expected_size,
            new_file_expected_size: file.length,
            new_file_remote_checksum: normalize_checksum(&file.remote_checksum),
            operations,
        },
        planned_copy_bytes,
        planned_download_bytes,
    }))
}

fn artifact_ops_to_models(file_id: u64, ops: &[PatchOperationArtifact]) -> Vec<DownloadPatchOp> {
    ops.iter()
        .map(|op| DownloadPatchOp {
            id: 0,
            file_id,
            data_order: op.data_order,
            op_type: op.op_type.clone(),
            dest_start: op.dest_start,
            length: op.length,
            target_checksum: normalize_checksum(&op.target_checksum),
            source_start: op.source_start,
            source_checksum: op.source_checksum.clone().map(|v| normalize_checksum(&v)),
            blob_offset: op.blob_offset,
            downloaded_bytes: 0,
            retry_count: 0,
        })
        .collect()
}

pub(crate) async fn persist_patch_plan(
    context: Arc<FoxyContext>,
    plan: &PlannedPatch,
) -> anyhow::Result<()> {
    let persist_started = std::time::Instant::now();
    let (patch_json_path, patch_blob_path) = patch_paths_for_file(plan.artifact.file_id);
    let patch_json_string = serde_json::to_string_pretty(&plan.artifact)
        .context("failed to serialize patch artifact")?;
    {
        let json_path = patch_json_path.clone();
        let json_bytes = patch_json_string.into_bytes();
        tokio::task::spawn_blocking(move || {
            crate::core::utils::fs_safety::atomic_write(&json_path, &json_bytes)
        })
        .await?
        .with_context(|| format!("failed to write {}", patch_json_path.display()))?;
    }

    let patch_blob_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(&patch_blob_path)
        .await
        .with_context(|| format!("failed to open {}", patch_blob_path.display()))?;
    patch_blob_file
        .set_len(plan.planned_download_bytes)
        .await
        .with_context(|| format!("failed to resize {}", patch_blob_path.display()))?;
    drop(patch_blob_file);

    let patch_file = DownloadPatchFile {
        file_id: plan.artifact.file_id,
        patch_json_path: patch_json_path.to_string_lossy().to_string(),
        patch_blob_path: patch_blob_path.to_string_lossy().to_string(),
        planned_copy_bytes: plan.planned_copy_bytes,
        planned_download_bytes: plan.planned_download_bytes,
        status: PATCH_STATUS_PLANNED.to_string(),
        last_error: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

    save_download_patch_file(context.clone(), &patch_file)
        .await
        .context("failed to save patch file row")?;

    let ops = artifact_ops_to_models(plan.artifact.file_id, &plan.artifact.operations);
    replace_download_patch_ops_for_file(context, plan.artifact.file_id as i64, &ops)
        .await
        .context("failed to save patch op rows")?;

    let insert_ops = ops
        .iter()
        .filter(|op| PatchOpType::InsertRemote.matches(op))
        .count();
    let copy_ops = ops.len().saturating_sub(insert_ops);
    let savings_bytes = plan
        .artifact
        .new_file_expected_size
        .saturating_sub(plan.planned_download_bytes);
    let savings_percent = savings_bytes
        .saturating_mul(100)
        .checked_div(plan.artifact.new_file_expected_size)
        .unwrap_or(0);
    info!(
        "Persisted delta patch plan: file_id={} ops={} copy_ops={} insert_ops={} copy_bytes={} planned_download_bytes={} full_bytes={} savings_bytes={} savings_percent={}% elapsed={:.2?}",
        plan.artifact.file_id,
        ops.len(),
        copy_ops,
        insert_ops,
        plan.planned_copy_bytes,
        plan.planned_download_bytes,
        plan.artifact.new_file_expected_size,
        savings_bytes,
        savings_percent,
        persist_started.elapsed()
    );

    Ok(())
}

pub(super) async fn load_patch_artifact(path: &str) -> anyhow::Result<PatchArtifact> {
    let bytes = fs::read(path)
        .await
        .with_context(|| format!("failed to read patch artifact {}", path))?;
    let artifact: PatchArtifact = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse patch artifact {}", path))?;
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_plan_coverage ──────────────────────────────────────────

    #[test]
    fn validate_plan_coverage_empty_ops_errors() {
        let result = validate_plan_coverage(&[], 100);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no operations"));
    }

    #[test]
    fn validate_plan_coverage_single_op_exact() {
        let ops = vec![PatchOperationArtifact {
            data_order: 0,
            op_type: PatchOpType::CopyLocal.as_str().to_string(),
            dest_start: 0,
            length: 100,
            target_checksum: "ABC".to_string(),
            source_start: Some(0),
            source_checksum: Some("ABC".to_string()),
            blob_offset: None,
        }];
        assert!(validate_plan_coverage(&ops, 100).is_ok());
    }

    #[test]
    fn validate_plan_coverage_contiguous_ops() {
        let ops = vec![
            PatchOperationArtifact {
                data_order: 0,
                op_type: PatchOpType::CopyLocal.as_str().to_string(),
                dest_start: 0,
                length: 50,
                target_checksum: "A".to_string(),
                source_start: Some(0),
                source_checksum: Some("A".to_string()),
                blob_offset: None,
            },
            PatchOperationArtifact {
                data_order: 1,
                op_type: PatchOpType::InsertRemote.as_str().to_string(),
                dest_start: 50,
                length: 50,
                target_checksum: "B".to_string(),
                source_start: None,
                source_checksum: None,
                blob_offset: Some(0),
            },
        ];
        assert!(validate_plan_coverage(&ops, 100).is_ok());
    }

    #[test]
    fn validate_plan_coverage_non_zero_start_errors() {
        let ops = vec![PatchOperationArtifact {
            data_order: 0,
            op_type: PatchOpType::CopyLocal.as_str().to_string(),
            dest_start: 10, // doesn't start at 0
            length: 90,
            target_checksum: "A".to_string(),
            source_start: Some(10),
            source_checksum: Some("A".to_string()),
            blob_offset: None,
        }];
        let result = validate_plan_coverage(&ops, 100);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("does not start at byte 0")
        );
    }

    #[test]
    fn validate_plan_coverage_gap_errors() {
        let ops = vec![
            PatchOperationArtifact {
                data_order: 0,
                op_type: PatchOpType::CopyLocal.as_str().to_string(),
                dest_start: 0,
                length: 40,
                target_checksum: "A".to_string(),
                source_start: Some(0),
                source_checksum: Some("A".to_string()),
                blob_offset: None,
            },
            PatchOperationArtifact {
                data_order: 1,
                op_type: PatchOpType::InsertRemote.as_str().to_string(),
                dest_start: 60, // gap from 40..60
                length: 40,
                target_checksum: "B".to_string(),
                source_start: None,
                source_checksum: None,
                blob_offset: Some(0),
            },
        ];
        let result = validate_plan_coverage(&ops, 100);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("non-contiguous"));
    }

    #[test]
    fn validate_plan_coverage_length_mismatch_errors() {
        let ops = vec![PatchOperationArtifact {
            data_order: 0,
            op_type: PatchOpType::CopyLocal.as_str().to_string(),
            dest_start: 0,
            length: 80, // covers only 80 out of 100
            target_checksum: "A".to_string(),
            source_start: Some(0),
            source_checksum: Some("A".to_string()),
            blob_offset: None,
        }];
        let result = validate_plan_coverage(&ops, 100);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("end mismatch"));
    }

    #[test]
    fn validate_plan_coverage_zero_length_op_errors() {
        let ops = vec![PatchOperationArtifact {
            data_order: 0,
            op_type: PatchOpType::CopyLocal.as_str().to_string(),
            dest_start: 0,
            length: 0,
            target_checksum: "A".to_string(),
            source_start: Some(0),
            source_checksum: Some("A".to_string()),
            blob_offset: None,
        }];
        let result = validate_plan_coverage(&ops, 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("zero length"));
    }

    // ── plan_savings_meet_threshold ─────────────────────────────────────

    #[test]
    fn plan_savings_zero_full_bytes_returns_false() {
        assert!(!plan_savings_meet_threshold(0, 0));
    }

    #[test]
    fn plan_savings_download_exceeds_full_returns_false() {
        assert!(!plan_savings_meet_threshold(200, 100));
    }

    #[test]
    fn plan_savings_download_equals_full_returns_false() {
        assert!(!plan_savings_meet_threshold(100, 100));
    }

    #[test]
    fn plan_savings_some_savings_returns_true() {
        // PATCH_MIN_SAVINGS_PERCENT is 0, so any savings should pass
        assert!(plan_savings_meet_threshold(50, 100));
    }

    #[test]
    fn plan_savings_minimal_savings_returns_true() {
        assert!(plan_savings_meet_threshold(99, 100));
    }

    #[test]
    fn plan_savings_zero_download_returns_true() {
        // 100% savings
        assert!(plan_savings_meet_threshold(0, 100));
    }

    #[test]
    fn plan_file_patch_uses_derived_clean_part_locals() {
        let dir = tempfile::tempdir().unwrap();
        let local_path = dir.path().join("file.pbo");
        std::fs::write(&local_path, vec![0u8; 10]).unwrap();
        let file = FoxyModFile {
            id: 10,
            local_path: local_path.to_string_lossy().to_string(),
            remote_path: "https://example.invalid/file.pbo".to_string(),
            local_checksum: "FILE".to_string(),
            remote_checksum: "FILE".to_string(),
            length: 10,
            ..Default::default()
        };
        let old_parts = vec![FoxyModFilePart {
            id: 1,
            file_id: 10,
            path: "part0".to_string(),
            remote_checksum: "AA".to_string(),
            remote_length: 10,
            remote_start: 0,
            data_order: 0,
            ..Default::default()
        }];
        let new_parts = old_parts.clone();

        let plan = plan_file_patch(&file, &new_parts, &old_parts)
            .unwrap()
            .expect("derived clean local part should be copyable");

        assert_eq!(plan.planned_download_bytes, 0);
        assert_eq!(plan.planned_copy_bytes, 10);
        assert_eq!(plan.artifact.operations.len(), 1);
        assert_eq!(
            plan.artifact.operations[0].op_type,
            PatchOpType::CopyLocal.as_str()
        );
        assert_eq!(plan.artifact.operations[0].source_start, Some(0));
    }
}
