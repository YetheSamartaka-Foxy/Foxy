use super::planning::{plan_savings_meet_threshold, validate_plan_coverage};
use super::types::{
    PatchOperationArtifact, checksum_matches, compute_tree_checksum_from_segment_checksums,
    normalize_checksum, sampled_copy_op_indices, should_abort_copy_fallback,
};
use crate::core::models::modification_file_part::FoxyModFilePart;
use md5::{Digest, Md5};
use std::collections::HashMap;

fn part(
    file_id: u64,
    data_order: i64,
    path: &str,
    start: u64,
    length: u64,
    checksum: &str,
) -> FoxyModFilePart {
    FoxyModFilePart {
        id: data_order as u64 + 1,
        file_id,
        path: path.to_string(),
        remote_length: length,
        local_length: length,
        remote_start: start,
        local_start: start,
        remote_checksum: checksum.to_string(),
        local_checksum: checksum.to_string(),
        data_order,
    }
}

#[test]
fn validate_contiguous_plan_coverage() {
    let ops = vec![
        PatchOperationArtifact {
            data_order: 0,
            op_type: "copy_local".to_string(),
            dest_start: 0,
            length: 10,
            target_checksum: "A".to_string(),
            source_start: Some(0),
            source_checksum: Some("A".to_string()),
            blob_offset: None,
        },
        PatchOperationArtifact {
            data_order: 1,
            op_type: "insert_remote".to_string(),
            dest_start: 10,
            length: 5,
            target_checksum: "B".to_string(),
            source_start: None,
            source_checksum: None,
            blob_offset: Some(0),
        },
    ];
    assert!(validate_plan_coverage(&ops, 15).is_ok());
    assert!(validate_plan_coverage(&ops, 16).is_err());
}

#[test]
fn path_match_has_precedence_over_checksum_pool() {
    let old = [
        part(1, 0, "x/a", 0, 10, "AA"),
        part(1, 1, "x/b", 10, 10, "AA"),
    ];
    let new = [part(1, 0, "x/b", 0, 10, "AA")];

    let old_parts_for_match: Vec<FoxyModFilePart> = old
        .iter()
        .filter(|part| !part.local_checksum.trim().is_empty() && part.local_length > 0)
        .cloned()
        .collect();
    let mut old_by_path: HashMap<String, Vec<usize>> = HashMap::new();
    let mut old_by_checksum_len: HashMap<(String, u64), Vec<usize>> = HashMap::new();
    for (idx, part) in old_parts_for_match.iter().enumerate() {
        old_by_path.entry(part.path.clone()).or_default().push(idx);
        old_by_checksum_len
            .entry((normalize_checksum(&part.local_checksum), part.local_length))
            .or_default()
            .push(idx);
    }

    let new_part = &new[0];
    let mut matched_idx = None;
    if let Some(candidates) = old_by_path.get(&new_part.path) {
        for idx in candidates {
            let candidate = &old_parts_for_match[*idx];
            if candidate.local_length == new_part.remote_length
                && checksum_matches(&candidate.local_checksum, &new_part.remote_checksum)
            {
                matched_idx = Some(*idx);
                break;
            }
        }
    }
    assert_eq!(matched_idx, Some(1));
}

#[test]
fn drifted_offsets_keep_copy_source_and_dest_separate() {
    let old = [
        part(1, 0, "a", 0, 10, "AA"),
        part(1, 1, "b", 10, 10, "BB"),
        part(1, 2, "c", 20, 10, "CC"),
    ];
    let new = vec![
        part(1, 0, "a", 0, 12, "A2"),
        part(1, 1, "b", 12, 10, "BB"),
        part(1, 2, "c", 22, 10, "CC"),
    ];
    let mut ops = Vec::new();
    let mut old_by_checksum_len: HashMap<(String, u64), Vec<usize>> = HashMap::new();
    for (idx, part) in old.iter().enumerate() {
        old_by_checksum_len
            .entry((normalize_checksum(&part.local_checksum), part.local_length))
            .or_default()
            .push(idx);
    }
    for np in &new {
        let key = (normalize_checksum(&np.remote_checksum), np.remote_length);
        if let Some(pool) = old_by_checksum_len.get(&key)
            && let Some(idx) = pool.first()
        {
            let source = &old[*idx];
            ops.push((np.path.clone(), source.remote_start, np.remote_start));
            continue;
        }
        ops.push((np.path.clone(), u64::MAX, np.remote_start));
    }

    assert_eq!(ops[0].0, "a");
    assert_eq!(ops[0].1, u64::MAX);
    assert_eq!(ops[1].0, "b");
    assert_eq!(ops[1].1, 10);
    assert_eq!(ops[1].2, 12);
    assert_eq!(ops[2].0, "c");
    assert_eq!(ops[2].1, 20);
    assert_eq!(ops[2].2, 22);
}

#[test]
fn tiny_delta_savings_are_allowed() {
    assert!(plan_savings_meet_threshold(99, 100));
    assert!(!plan_savings_meet_threshold(100, 100));
    assert!(!plan_savings_meet_threshold(101, 100));
}

#[test]
fn tree_checksum_rollup_uses_segment_checksums() {
    let checksums = ["AA", "bb", "Cc"];
    let actual = compute_tree_checksum_from_segment_checksums(checksums);

    let expected = {
        let mut hasher = Md5::new();
        hasher.update("AA".as_bytes());
        hasher.update("BB".as_bytes());
        hasher.update("CC".as_bytes());
        hex::encode_upper(hasher.finalize())
    };

    assert_eq!(actual, expected);
}

#[test]
fn sampled_copy_indices_are_evenly_spaced() {
    let indices: Vec<usize> = (0..100).collect();
    let sampled = sampled_copy_op_indices(&indices, 5);
    assert_eq!(sampled, vec![0, 24, 49, 74, 99]);
}

#[test]
fn sampled_copy_indices_return_all_when_small() {
    let indices = vec![3, 8, 15];
    assert_eq!(sampled_copy_op_indices(&indices, 10), indices);
}

#[test]
fn fallback_abort_respects_thresholds() {
    assert!(!should_abort_copy_fallback(11, 1_100, 11, 1_100));
    assert!(should_abort_copy_fallback(12, 1_200, 9, 900));
    assert!(should_abort_copy_fallback(12, 1_000, 1, 800));
    assert!(!should_abort_copy_fallback(12, 1_000, 8, 700));
}
