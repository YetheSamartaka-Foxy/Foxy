use crate::core::models::download_patch_op::DownloadPatchOp;
use crate::core::models::modification_file_part::FoxyModFilePart;
use crate::core::utils::content_hash::FlexHasher;
use serde::{Deserialize, Serialize};
pub(super) const PATCH_SCHEMA_VERSION: u32 = 1;
pub(super) const PATCH_STATUS_PLANNED: &str = "planned";
pub(super) const PATCH_STATUS_DOWNLOADING: &str = "downloading";
pub(super) const PATCH_STATUS_READY: &str = "ready";
pub(super) const PATCH_STATUS_APPLYING: &str = "applying";
pub(super) const PATCH_STATUS_DONE: &str = "done";
pub(super) const PATCH_STATUS_FALLBACK_FULL: &str = "fallback_full";
pub(super) const PATCH_MIN_SAVINGS_PERCENT: u64 = 0;
pub(super) const PATCH_DOWNLOAD_MAX_RETRIES: u32 = 3;
pub(super) const PATCH_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub(super) const COPY_BUFFER_SIZE: usize = 512 * 1024;
/// Chunk size of the sequential copy-run stream. Large enough that a
/// rotational disk alternating between the source and the output spends its
/// time transferring rather than seeking.
pub(super) const RUN_COPY_BUFFER_SIZE: usize = 8 * 1024 * 1024;
pub(super) const PATCH_PREFLIGHT_COPY_SAMPLE_OPS: usize = 24;
pub(super) const PATCH_COPY_FALLBACK_ABORT_MIN_ATTEMPTED_OPS: usize = 12;
const PATCH_COPY_FALLBACK_ABORT_PERCENT: u64 = 75;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatchOpType {
    CopyLocal,
    InsertRemote,
}

pub(super) trait PatchOpLike {
    fn op_type(&self) -> &str;
}

impl<T> PatchOpLike for &T
where
    T: PatchOpLike + ?Sized,
{
    fn op_type(&self) -> &str {
        (*self).op_type()
    }
}

impl PatchOpLike for DownloadPatchOp {
    fn op_type(&self) -> &str {
        &self.op_type
    }
}

impl PatchOpType {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            PatchOpType::CopyLocal => "copy_local",
            PatchOpType::InsertRemote => "insert_remote",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "copy_local" => Some(PatchOpType::CopyLocal),
            "insert_remote" => Some(PatchOpType::InsertRemote),
            _ => None,
        }
    }

    pub(super) fn matches<T>(self, op: &T) -> bool
    where
        T: PatchOpLike + ?Sized,
    {
        op.op_type() == self.as_str()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PatchOperationArtifact {
    pub data_order: i64,
    pub op_type: String,
    pub dest_start: u64,
    pub length: u64,
    pub target_checksum: String,
    pub source_start: Option<u64>,
    pub source_checksum: Option<String>,
    pub blob_offset: Option<u64>,
}

impl PatchOpLike for PatchOperationArtifact {
    fn op_type(&self) -> &str {
        &self.op_type
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PatchArtifact {
    pub schema_version: u32,
    pub repository_url: String,
    pub file_id: u64,
    pub local_target_path: String,
    pub remote_url: String,
    pub base_file_expected_size: u64,
    pub new_file_expected_size: u64,
    pub new_file_remote_checksum: String,
    pub operations: Vec<PatchOperationArtifact>,
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedPatch {
    pub(crate) artifact: PatchArtifact,
    pub(crate) planned_copy_bytes: u64,
    pub(crate) planned_download_bytes: u64,
}

pub(super) fn keep_patch_artifacts_for_diagnostics() -> bool {
    cfg!(debug_assertions)
}

pub(super) fn normalize_checksum(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

pub(super) fn checksum_matches(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual.trim())
}

pub(super) fn compute_tree_checksum_from_segment_checksums<'a, I>(checksums: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let mut upper_buf = [0u8; 128];
    let mut peekable = checksums.into_iter().peekable();
    let mut hasher = peekable
        .peek()
        .map(|cs| FlexHasher::from_checksum(cs))
        .unwrap_or_else(FlexHasher::new_md5);

    for checksum in peekable {
        let trimmed = checksum.trim();
        let len = trimmed.len().min(upper_buf.len());
        for (i, byte) in trimmed.bytes().take(len).enumerate() {
            upper_buf[i] = byte.to_ascii_uppercase();
        }
        hasher.update(&upper_buf[..len]);
    }
    hasher.finalize_hex()
}

pub(super) fn infer_repository_url(file_remote_url: &str) -> String {
    file_remote_url
        .rsplit_once('/')
        .map(|(base, _)| format!("{}/", base))
        .unwrap_or_default()
}

pub(super) fn expected_remote_end(parts: &[FoxyModFilePart]) -> Option<u64> {
    parts
        .iter()
        .map(|part| part.remote_start.saturating_add(part.remote_length))
        .max()
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct CopySourcePreflightStats {
    pub(super) copy_ops_total: usize,
    pub(super) copy_bytes_total: u64,
    pub(super) checked_ops: usize,
    pub(super) checked_bytes: u64,
    pub(super) mismatch_ops: usize,
    pub(super) mismatch_bytes: u64,
}

pub(super) fn sampled_copy_op_indices(copy_indices: &[usize], max_samples: usize) -> Vec<usize> {
    if copy_indices.is_empty() || max_samples == 0 {
        return Vec::new();
    }
    if copy_indices.len() <= max_samples {
        return copy_indices.to_vec();
    }

    // Keep samples evenly distributed to detect stale baselines early across the file.
    let mut sampled = Vec::with_capacity(max_samples);
    for sample_idx in 0..max_samples {
        let pos = if max_samples == 1 {
            0
        } else {
            sample_idx.saturating_mul(copy_indices.len() - 1) / (max_samples - 1)
        };
        let index = copy_indices[pos];
        if sampled.last().copied() != Some(index) {
            sampled.push(index);
        }
    }
    sampled
}

/// One sequential unit of the apply loop: a run of adjacent copy ops that is
/// contiguous in both the source file and the output, or a single insert op.
/// Every op keeps its own boundary checksum inside a run, so per-part
/// verification and fallback are unchanged; only the I/O is merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ApplySegment {
    CopyRun {
        /// Indices into the op list, in `data_order`.
        op_range: std::ops::Range<usize>,
        source_start: u64,
        dest_start: u64,
        length: u64,
    },
    Insert {
        op_idx: usize,
    },
}

/// Merge adjacent copy ops into sequential runs. A gap in the source, a gap in
/// the output, or an insert op breaks a run. Ops missing the fields their type
/// requires fail here, before any byte is written.
pub(super) fn coalesce_apply_segments(
    ops: &[DownloadPatchOp],
) -> anyhow::Result<Vec<ApplySegment>> {
    let mut segments: Vec<ApplySegment> = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        match PatchOpType::from_str(&op.op_type) {
            Some(PatchOpType::InsertRemote) => {
                if op.blob_offset.is_none() {
                    anyhow::bail!("insert op {} missing blob_offset", op.data_order);
                }
                segments.push(ApplySegment::Insert { op_idx: idx });
            }
            Some(PatchOpType::CopyLocal) => {
                let Some(source_start) = op.source_start else {
                    anyhow::bail!("copy op {} missing source_start", op.data_order);
                };
                if op.source_checksum.is_none() {
                    anyhow::bail!("copy op {} missing source_checksum", op.data_order);
                }
                if let Some(ApplySegment::CopyRun {
                    op_range,
                    source_start: run_source_start,
                    dest_start: run_dest_start,
                    length,
                }) = segments.last_mut()
                    && run_source_start.checked_add(*length) == Some(source_start)
                    && run_dest_start.checked_add(*length) == Some(op.dest_start)
                    && op_range.end == idx
                {
                    op_range.end = idx + 1;
                    *length = length.saturating_add(op.length);
                    continue;
                }
                segments.push(ApplySegment::CopyRun {
                    op_range: idx..idx + 1,
                    source_start,
                    dest_start: op.dest_start,
                    length: op.length,
                });
            }
            None => anyhow::bail!("unsupported patch op type {}", op.op_type),
        }
    }
    Ok(segments)
}

pub(super) fn should_abort_copy_fallback(
    attempted_ops: usize,
    attempted_bytes: u64,
    fallback_ops: usize,
    fallback_bytes: u64,
) -> bool {
    if attempted_ops < PATCH_COPY_FALLBACK_ABORT_MIN_ATTEMPTED_OPS {
        return false;
    }
    if attempted_ops == 0 || fallback_ops == 0 {
        return false;
    }

    let fallback_ops_percent = (fallback_ops as u64).saturating_mul(100) / attempted_ops as u64;
    let fallback_bytes_percent = fallback_bytes
        .saturating_mul(100)
        .checked_div(attempted_bytes)
        .unwrap_or(0);

    fallback_ops_percent >= PATCH_COPY_FALLBACK_ABORT_PERCENT
        || fallback_bytes_percent >= PATCH_COPY_FALLBACK_ABORT_PERCENT
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_checksum ──────────────────────────────────────────────

    #[test]
    fn normalize_checksum_trims_and_uppercases() {
        assert_eq!(normalize_checksum("  abc123  "), "ABC123");
    }

    #[test]
    fn normalize_checksum_empty() {
        assert_eq!(normalize_checksum(""), "");
    }

    // ── checksum_matches ────────────────────────────────────────────────

    #[test]
    fn checksum_matches_case_insensitive() {
        assert!(checksum_matches("abc123", "ABC123"));
        assert!(checksum_matches("ABC123", "abc123"));
    }

    #[test]
    fn checksum_matches_with_whitespace() {
        assert!(checksum_matches(" abc123 ", "  ABC123  "));
    }

    #[test]
    fn checksum_matches_different_values() {
        assert!(!checksum_matches("abc123", "def456"));
    }

    // ── PatchOpType ─────────────────────────────────────────────────────

    #[test]
    fn patch_op_type_round_trip() {
        assert_eq!(
            PatchOpType::from_str(PatchOpType::CopyLocal.as_str()),
            Some(PatchOpType::CopyLocal)
        );
        assert_eq!(
            PatchOpType::from_str(PatchOpType::InsertRemote.as_str()),
            Some(PatchOpType::InsertRemote)
        );
    }

    #[test]
    fn patch_op_type_from_str_unknown() {
        assert_eq!(PatchOpType::from_str("unknown_op"), None);
    }

    // ── infer_repository_url ────────────────────────────────────────────

    #[test]
    fn infer_repository_url_from_file_path() {
        assert_eq!(
            infer_repository_url("http://server.com/repo/@mod/file.pbo"),
            "http://server.com/repo/@mod/"
        );
    }

    #[test]
    fn infer_repository_url_no_slash() {
        assert_eq!(infer_repository_url("file.pbo"), "");
    }

    // ── sampled_copy_op_indices ─────────────────────────────────────────

    #[test]
    fn sampled_copy_op_indices_empty_input() {
        assert!(sampled_copy_op_indices(&[], 10).is_empty());
    }

    #[test]
    fn sampled_copy_op_indices_zero_samples() {
        assert!(sampled_copy_op_indices(&[0, 1, 2], 0).is_empty());
    }

    #[test]
    fn sampled_copy_op_indices_fewer_than_max() {
        let indices = vec![0, 1, 2];
        let sampled = sampled_copy_op_indices(&indices, 10);
        assert_eq!(sampled, indices);
    }

    #[test]
    fn sampled_copy_op_indices_evenly_distributed() {
        let indices: Vec<usize> = (0..100).collect();
        let sampled = sampled_copy_op_indices(&indices, 5);
        // Should include first and last
        assert_eq!(*sampled.first().unwrap(), 0);
        assert_eq!(*sampled.last().unwrap(), 99);
        assert!(sampled.len() <= 5);
    }

    // ── should_abort_copy_fallback ──────────────────────────────────────

    #[test]
    fn should_abort_below_min_attempted() {
        // Below minimum threshold, should never abort
        assert!(!should_abort_copy_fallback(5, 1000, 5, 1000));
    }

    #[test]
    fn should_abort_high_fallback_rate() {
        // 15 attempted, 12 fallback = 80% > 75% threshold
        assert!(should_abort_copy_fallback(15, 1500, 12, 1200));
    }

    #[test]
    fn should_abort_low_fallback_rate() {
        // 15 attempted, 2 fallback = 13% < 75% threshold
        assert!(!should_abort_copy_fallback(15, 1500, 2, 200));
    }

    // ── expected_remote_end ─────────────────────────────────────────────

    #[test]
    fn expected_remote_end_empty() {
        assert_eq!(expected_remote_end(&[]), None);
    }

    #[test]
    fn expected_remote_end_single_part() {
        let part = FoxyModFilePart {
            remote_start: 100,
            remote_length: 50,
            ..Default::default()
        };
        assert_eq!(expected_remote_end(&[part]), Some(150));
    }

    #[test]
    fn expected_remote_end_multiple_parts() {
        let parts = vec![
            FoxyModFilePart {
                remote_start: 0,
                remote_length: 100,
                ..Default::default()
            },
            FoxyModFilePart {
                remote_start: 100,
                remote_length: 200,
                ..Default::default()
            },
        ];
        assert_eq!(expected_remote_end(&parts), Some(300));
    }

    // ── normalize_checksum: additional ─────────────────────────────────

    #[test]
    fn normalize_checksum_mixed_case() {
        assert_eq!(normalize_checksum("aAbBcC"), "AABBCC");
    }

    #[test]
    fn normalize_checksum_already_upper() {
        assert_eq!(normalize_checksum("ABC123"), "ABC123");
    }

    // ── checksum_matches: additional ───────────────────────────────────

    #[test]
    fn checksum_matches_empty_strings() {
        assert!(checksum_matches("", ""));
    }

    #[test]
    fn checksum_matches_one_empty_one_not() {
        assert!(!checksum_matches("abc", ""));
        assert!(!checksum_matches("", "abc"));
    }

    // ── PatchOpType: additional ────────────────────────────────────────

    #[test]
    fn patch_op_type_as_str_values() {
        assert_eq!(PatchOpType::CopyLocal.as_str(), "copy_local");
        assert_eq!(PatchOpType::InsertRemote.as_str(), "insert_remote");
    }

    #[test]
    fn patch_op_type_from_str_empty() {
        assert_eq!(PatchOpType::from_str(""), None);
    }

    #[test]
    fn patch_op_type_matches_artifact() {
        let artifact = PatchOperationArtifact {
            data_order: 0,
            op_type: "copy_local".to_string(),
            dest_start: 0,
            length: 100,
            target_checksum: "ABC".to_string(),
            source_start: Some(0),
            source_checksum: Some("ABC".to_string()),
            blob_offset: None,
        };
        assert!(PatchOpType::CopyLocal.matches(&artifact));
        assert!(!PatchOpType::InsertRemote.matches(&artifact));
    }

    // ── infer_repository_url: additional ───────────────────────────────

    #[test]
    fn infer_repository_url_preserves_protocol() {
        assert_eq!(
            infer_repository_url("https://cdn.example.com/repo/@mod/file.pbo"),
            "https://cdn.example.com/repo/@mod/"
        );
    }

    #[test]
    fn infer_repository_url_single_slash() {
        assert_eq!(infer_repository_url("/file.pbo"), "/");
    }

    // ── sampled_copy_op_indices: additional ────────────────────────────

    #[test]
    fn sampled_copy_op_indices_single_sample() {
        let indices: Vec<usize> = (0..50).collect();
        let sampled = sampled_copy_op_indices(&indices, 1);
        assert_eq!(sampled, vec![0]);
    }

    #[test]
    fn sampled_copy_op_indices_two_samples() {
        let indices: Vec<usize> = (0..100).collect();
        let sampled = sampled_copy_op_indices(&indices, 2);
        assert_eq!(sampled, vec![0, 99]);
    }

    #[test]
    fn sampled_copy_op_indices_exact_count() {
        let indices = vec![10, 20, 30];
        let sampled = sampled_copy_op_indices(&indices, 3);
        assert_eq!(sampled, indices);
    }

    // ── should_abort_copy_fallback: additional ─────────────────────────

    #[test]
    fn should_abort_zero_attempted_returns_false() {
        assert!(!should_abort_copy_fallback(0, 0, 0, 0));
    }

    #[test]
    fn should_abort_high_byte_fallback_rate() {
        // 15 attempted, 1 fallback op (low) but bytes are high
        assert!(should_abort_copy_fallback(15, 1000, 2, 800));
    }

    #[test]
    fn should_abort_exact_threshold() {
        // 20 attempted, 15 fallback = 75% exactly at threshold
        assert!(should_abort_copy_fallback(20, 2000, 15, 1500));
    }

    #[test]
    fn should_abort_just_below_threshold() {
        // 20 attempted, 14 fallback = 70% below 75%
        assert!(!should_abort_copy_fallback(20, 2000, 14, 1400));
    }

    // ── compute_tree_checksum_from_segment_checksums ───────────────────

    #[test]
    fn compute_tree_checksum_empty_iterator() {
        let result = compute_tree_checksum_from_segment_checksums(std::iter::empty());
        // Should produce an MD5 hash of no input
        assert_eq!(result.len(), 32);
    }

    #[test]
    fn compute_tree_checksum_single_md5_segment() {
        let checksums = ["abc123"];
        let result = compute_tree_checksum_from_segment_checksums(checksums);
        assert_eq!(result.len(), 32); // MD5 output
    }

    #[test]
    fn compute_tree_checksum_deterministic() {
        let checksums = ["aaa111", "bbb222", "ccc333"];
        let r1 = compute_tree_checksum_from_segment_checksums(checksums.iter().copied());
        let r2 = compute_tree_checksum_from_segment_checksums(checksums.iter().copied());
        assert_eq!(r1, r2);
    }

    #[test]
    fn compute_tree_checksum_order_matters() {
        let r1 = compute_tree_checksum_from_segment_checksums(["aaa", "bbb"]);
        let r2 = compute_tree_checksum_from_segment_checksums(["bbb", "aaa"]);
        assert_ne!(r1, r2);
    }

    // ── keep_patch_artifacts_for_diagnostics ───────────────────────────

    #[test]
    fn keep_patch_artifacts_for_diagnostics_returns_bool() {
        // In test mode (debug_assertions), should return true
        let result = keep_patch_artifacts_for_diagnostics();
        assert_eq!(result, cfg!(debug_assertions));
    }

    // ── coalesce_apply_segments ────────────────────────────────────────

    fn copy_op(order: i64, dest_start: u64, length: u64, source_start: u64) -> DownloadPatchOp {
        DownloadPatchOp {
            id: order as u64 + 1,
            file_id: 1,
            data_order: order,
            op_type: PatchOpType::CopyLocal.as_str().to_string(),
            dest_start,
            length,
            target_checksum: format!("T{order}"),
            source_start: Some(source_start),
            source_checksum: Some(format!("S{order}")),
            blob_offset: None,
            downloaded_bytes: 0,
            retry_count: 0,
        }
    }

    fn insert_op(order: i64, dest_start: u64, length: u64, blob_offset: u64) -> DownloadPatchOp {
        DownloadPatchOp {
            id: order as u64 + 1,
            file_id: 1,
            data_order: order,
            op_type: PatchOpType::InsertRemote.as_str().to_string(),
            dest_start,
            length,
            target_checksum: format!("T{order}"),
            source_start: None,
            source_checksum: None,
            blob_offset: Some(blob_offset),
            downloaded_bytes: 0,
            retry_count: 0,
        }
    }

    #[test]
    fn coalesce_merges_adjacent_copy_ops_into_one_run() {
        let ops = vec![copy_op(0, 0, 10, 0), copy_op(1, 10, 10, 10)];
        let segments = coalesce_apply_segments(&ops).unwrap();
        assert_eq!(
            segments,
            vec![ApplySegment::CopyRun {
                op_range: 0..2,
                source_start: 0,
                dest_start: 0,
                length: 20,
            }]
        );
    }

    #[test]
    fn coalesce_breaks_run_on_source_gap() {
        let ops = vec![copy_op(0, 0, 10, 0), copy_op(1, 10, 10, 15)];
        let segments = coalesce_apply_segments(&ops).unwrap();
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn coalesce_breaks_run_on_dest_gap() {
        let ops = vec![copy_op(0, 0, 10, 0), copy_op(1, 12, 10, 10)];
        let segments = coalesce_apply_segments(&ops).unwrap();
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn coalesce_breaks_run_on_insert_op() {
        let ops = vec![
            copy_op(0, 0, 10, 0),
            insert_op(1, 10, 5, 0),
            copy_op(2, 15, 10, 10),
        ];
        let segments = coalesce_apply_segments(&ops).unwrap();
        assert_eq!(
            segments,
            vec![
                ApplySegment::CopyRun {
                    op_range: 0..1,
                    source_start: 0,
                    dest_start: 0,
                    length: 10,
                },
                ApplySegment::Insert { op_idx: 1 },
                ApplySegment::CopyRun {
                    op_range: 2..3,
                    source_start: 10,
                    dest_start: 15,
                    length: 10,
                },
            ]
        );
    }

    #[test]
    fn coalesce_rejects_copy_op_without_source() {
        let mut op = copy_op(0, 0, 10, 0);
        op.source_start = None;
        assert!(coalesce_apply_segments(&[op]).is_err());
    }

    #[test]
    fn coalesce_rejects_insert_op_without_blob_offset() {
        let mut op = insert_op(0, 0, 10, 0);
        op.blob_offset = None;
        assert!(coalesce_apply_segments(&[op]).is_err());
    }

    // ── PatchArtifact serialization ────────────────────────────────────

    #[test]
    fn patch_artifact_serde_round_trip() {
        let artifact = PatchArtifact {
            schema_version: PATCH_SCHEMA_VERSION,
            repository_url: "https://example.com/repo/".to_string(),
            file_id: 42,
            local_target_path: "/mods/@test/file.pbo".to_string(),
            remote_url: "https://example.com/repo/@test/file.pbo".to_string(),
            base_file_expected_size: 1000,
            new_file_expected_size: 1200,
            new_file_remote_checksum: "ABC123".to_string(),
            operations: vec![PatchOperationArtifact {
                data_order: 0,
                op_type: PatchOpType::CopyLocal.as_str().to_string(),
                dest_start: 0,
                length: 800,
                target_checksum: "DEF456".to_string(),
                source_start: Some(0),
                source_checksum: Some("DEF456".to_string()),
                blob_offset: None,
            }],
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let deserialized: PatchArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file_id, 42);
        assert_eq!(deserialized.operations.len(), 1);
        assert_eq!(deserialized.operations[0].length, 800);
    }
}
