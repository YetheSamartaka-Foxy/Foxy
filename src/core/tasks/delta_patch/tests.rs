use super::apply::{RunPart, RunStop, apply_patch_to_temp_file, copy_run_blocking};
use super::planning::{plan_savings_meet_threshold, validate_plan_coverage};
use super::types::{
    PatchArtifact, PatchOpType, PatchOperationArtifact, checksum_matches,
    compute_tree_checksum_from_segment_checksums, normalize_checksum, sampled_copy_op_indices,
    should_abort_copy_fallback,
};
use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::DownloadPatchFile;
use crate::core::models::download_patch_op::DownloadPatchOp;
use crate::core::models::modification_file_part::FoxyModFilePart;
use md5::{Digest, Md5};
use std::collections::HashMap;
use std::sync::Arc;

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

fn md5_upper(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    hex::encode_upper(hasher.finalize())
}

fn run_parts(lengths: &[u64], source: &[u8], source_start: u64) -> Vec<RunPart> {
    let mut cursor = source_start as usize;
    lengths
        .iter()
        .enumerate()
        .map(|(op_idx, &length)| {
            let part = RunPart {
                op_idx,
                length,
                expected_checksum: md5_upper(&source[cursor..cursor + length as usize]),
            };
            cursor += length as usize;
            part
        })
        .collect()
}

#[test]
fn copy_run_hashes_every_part_boundary_inside_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let source_bytes: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
    let source_path = dir.path().join("source.bin");
    let output_path = dir.path().join("output.bin");
    std::fs::write(&source_path, &source_bytes).unwrap();
    let source = std::fs::File::open(&source_path).unwrap();
    let output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&output_path)
        .unwrap();
    output.set_len(4096 + 100).unwrap();

    let parts = run_parts(&[704, 1, 1500, 891, 1000], &source_bytes, 0);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let (_pause_tx, pause_rx) = tokio::sync::watch::channel(false);
    let outcome =
        copy_run_blocking(&source, &output, 0, 100, &parts, 0, &cancel_rx, &pause_rx).unwrap();

    assert_eq!(outcome.stop, None);
    assert_eq!(outcome.hashes.len(), parts.len());
    for (run_idx, checksum) in &outcome.hashes {
        assert!(checksum_matches(
            &parts[*run_idx].expected_checksum,
            checksum
        ));
    }
    let written = std::fs::read(&output_path).unwrap();
    assert_eq!(&written[100..], &source_bytes[..]);
    assert!(written[..100].iter().all(|byte| *byte == 0));
}

#[test]
fn copy_run_pauses_between_chunks_and_resumes_the_partial_part() {
    let dir = tempfile::tempdir().unwrap();
    // Two chunks of the stream buffer, so the pause check runs mid-run.
    let total = super::types::RUN_COPY_BUFFER_SIZE as u64 + 4096;
    let source_bytes: Vec<u8> = (0..total).map(|i| (i % 253) as u8).collect();
    let source_path = dir.path().join("source.bin");
    let output_path = dir.path().join("output.bin");
    std::fs::write(&source_path, &source_bytes).unwrap();
    let source = std::fs::File::open(&source_path).unwrap();
    let output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&output_path)
        .unwrap();
    output.set_len(total).unwrap();

    let first_len = 1024u64;
    let parts = run_parts(&[first_len, total - first_len], &source_bytes, 0);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let (pause_tx, pause_rx) = tokio::sync::watch::channel(true);
    let paused =
        copy_run_blocking(&source, &output, 0, 0, &parts, 0, &cancel_rx, &pause_rx).unwrap();
    assert_eq!(paused.stop, Some(RunStop::Paused { next_part: 1 }));
    assert_eq!(paused.hashes.len(), 1);
    assert_eq!(paused.hashes[0].0, 0);

    pause_tx.send(false).unwrap();
    let resumed =
        copy_run_blocking(&source, &output, 0, 0, &parts, 1, &cancel_rx, &pause_rx).unwrap();
    assert_eq!(resumed.stop, None);
    assert_eq!(resumed.hashes.len(), 1);
    assert_eq!(resumed.hashes[0].0, 1);
    assert!(checksum_matches(
        &parts[1].expected_checksum,
        &resumed.hashes[0].1
    ));
    assert_eq!(std::fs::read(&output_path).unwrap(), source_bytes);
}

#[test]
fn copy_run_stops_on_cancel_between_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let total = super::types::RUN_COPY_BUFFER_SIZE as u64 + 10;
    let source_bytes = vec![3u8; total as usize];
    let source_path = dir.path().join("source.bin");
    std::fs::write(&source_path, &source_bytes).unwrap();
    let source = std::fs::File::open(&source_path).unwrap();
    let output = tempfile::tempfile().unwrap();
    output.set_len(total).unwrap();
    let parts = run_parts(&[total], &source_bytes, 0);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(true);
    let (_pause_tx, pause_rx) = tokio::sync::watch::channel(false);

    let outcome =
        copy_run_blocking(&source, &output, 0, 0, &parts, 0, &cancel_rx, &pause_rx).unwrap();
    assert_eq!(outcome.stop, Some(RunStop::Cancelled));
    assert!(outcome.hashes.is_empty());
}

fn patch_op(
    order: i64,
    op_type: PatchOpType,
    dest_start: u64,
    length: u64,
    target_checksum: String,
    source_start: Option<u64>,
    blob_offset: Option<u64>,
) -> DownloadPatchOp {
    DownloadPatchOp {
        id: order as u64 + 1,
        file_id: 7,
        data_order: order,
        op_type: op_type.as_str().to_string(),
        dest_start,
        length,
        target_checksum: target_checksum.clone(),
        source_start,
        source_checksum: source_start.map(|_| target_checksum),
        blob_offset,
        downloaded_bytes: 0,
        retry_count: 0,
    }
}

/// End to end: parts reordered so the plan holds two coalescable runs, one
/// out-of-order copy and one insert from the blob. The output must equal the
/// planned bytes and the per-part checksums must be the ops' targets, with no
/// network fallback involved.
#[tokio::test]
async fn coalesced_apply_produces_planned_bytes_and_segment_checksums() {
    let dir = tempfile::tempdir().unwrap();
    let old: Vec<u8> = (0..6000u32).map(|i| (i * 13 % 199) as u8).collect();
    let target_path = dir.path().join("file.pbo");
    std::fs::write(&target_path, &old).unwrap();
    let blob_bytes: Vec<u8> = (0..500u32).map(|i| (i * 3 % 127) as u8).collect();
    let blob_path = dir.path().join("file.blob");
    std::fs::write(&blob_path, &blob_bytes).unwrap();

    // New layout: old[0..1000] + old[1000..2500] (run of two), blob[0..500],
    // old[4000..6000] (out of order, own run), old[2500..4000] (own run).
    type PlannedSegment<'a> = (PatchOpType, &'a [u8], Option<u64>, Option<u64>);
    let segments: Vec<PlannedSegment<'_>> = vec![
        (PatchOpType::CopyLocal, &old[0..1000], Some(0), None),
        (PatchOpType::CopyLocal, &old[1000..2500], Some(1000), None),
        (PatchOpType::InsertRemote, &blob_bytes[..], None, Some(0)),
        (PatchOpType::CopyLocal, &old[4000..6000], Some(4000), None),
        (PatchOpType::CopyLocal, &old[2500..4000], Some(2500), None),
    ];
    let mut expected_output = Vec::new();
    let mut ops = Vec::new();
    let mut dest = 0u64;
    for (order, (op_type, bytes, source_start, blob_offset)) in segments.iter().enumerate() {
        ops.push(patch_op(
            order as i64,
            *op_type,
            dest,
            bytes.len() as u64,
            md5_upper(bytes),
            *source_start,
            *blob_offset,
        ));
        expected_output.extend_from_slice(bytes);
        dest += bytes.len() as u64;
    }
    let coalesced = super::types::coalesce_apply_segments(&ops).unwrap();
    assert_eq!(coalesced.len(), 4, "two adjacent copies must share one run");

    let artifact = PatchArtifact {
        schema_version: super::types::PATCH_SCHEMA_VERSION,
        repository_url: "http://127.0.0.1:1/repo/".to_string(),
        file_id: 7,
        local_target_path: target_path.to_string_lossy().to_string(),
        remote_url: "http://127.0.0.1:1/repo/@a/file.pbo".to_string(),
        base_file_expected_size: old.len() as u64,
        new_file_expected_size: expected_output.len() as u64,
        new_file_remote_checksum: compute_tree_checksum_from_segment_checksums(
            ops.iter().map(|op| op.target_checksum.as_str()),
        ),
        operations: Vec::new(),
    };
    let patch_file = DownloadPatchFile {
        file_id: 7,
        patch_json_path: String::new(),
        patch_blob_path: blob_path.to_string_lossy().to_string(),
        planned_copy_bytes: 0,
        planned_download_bytes: 0,
        status: String::new(),
        last_error: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let db = crate::core::tasks::db_turso::build_test_database().await;
    let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
    let (_pause_tx, pause_rx) = tokio::sync::watch::channel(false);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let (tmp_path, segment_checksums) =
        apply_patch_to_temp_file(context, &artifact, &patch_file, &ops, pause_rx, cancel_rx)
            .await
            .expect("apply");

    assert_eq!(std::fs::read(&tmp_path).unwrap(), expected_output);
    let expected_checksums: Vec<String> = ops.iter().map(|op| op.target_checksum.clone()).collect();
    assert_eq!(segment_checksums, expected_checksums);
    assert!(checksum_matches(
        &artifact.new_file_remote_checksum,
        &compute_tree_checksum_from_segment_checksums(segment_checksums.iter().map(|s| s.as_str()))
    ));
}

/// One batch: a run whose last op runs past the end of the source, then an
/// insert. The readable prefix is hashed from the buffer, the truncated op is
/// reported unread, and the batch lands in the output with one write.
#[test]
fn apply_batch_hashes_readable_prefix_and_reports_truncated_ops() {
    let dir = tempfile::tempdir().unwrap();
    let old: Vec<u8> = (0..300u32).map(|i| (i * 7 % 251) as u8).collect();
    let blob: Vec<u8> = (0..50u32).map(|i| (i * 11 % 251) as u8).collect();
    std::fs::write(dir.path().join("old"), &old).unwrap();
    std::fs::write(dir.path().join("blob"), &blob).unwrap();
    let source = std::fs::File::open(dir.path().join("old")).unwrap();
    let blob_file = std::fs::File::open(dir.path().join("blob")).unwrap();
    let output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(dir.path().join("out"))
        .unwrap();

    // Copy run 200..350 of a 300-byte source: ops of 100, 50 (readable) and
    // 50 (past the end); then a 50-byte insert.
    let ops = vec![
        patch_op(
            0,
            PatchOpType::CopyLocal,
            0,
            100,
            md5_upper(&old[200..300]),
            Some(200),
            None,
        ),
        patch_op(
            1,
            PatchOpType::CopyLocal,
            100,
            50,
            "AA".into(),
            Some(300),
            None,
        ),
        patch_op(
            2,
            PatchOpType::CopyLocal,
            150,
            50,
            "BB".into(),
            Some(350),
            None,
        ),
        patch_op(
            3,
            PatchOpType::InsertRemote,
            200,
            50,
            md5_upper(&blob),
            None,
            Some(0),
        ),
    ];
    let segments = super::types::coalesce_apply_segments(&ops).unwrap();
    assert_eq!(segments.len(), 2);
    let batches = super::types::plan_apply_batches(&segments, &ops, 1 << 20);
    assert_eq!(batches.len(), 1);
    output.set_len(250).unwrap();

    let mut buffer = Vec::new();
    let streamed = super::apply::apply_batch_blocking(
        &source,
        old.len() as u64,
        &blob_file,
        &output,
        &ops,
        &segments,
        &batches[0],
        &mut buffer,
    )
    .unwrap();

    assert_eq!(streamed[0], (0, Some(md5_upper(&old[200..300]))));
    assert_eq!(streamed[1], (1, None));
    assert_eq!(streamed[2], (2, None));
    assert_eq!(streamed[3], (3, Some(md5_upper(&blob))));
    let written = std::fs::read(dir.path().join("out")).unwrap();
    assert_eq!(&written[..100], &old[200..300]);
    assert_eq!(&written[200..250], &blob[..]);
}

/// Thousands of small alternating ops spanning several batches, with one run
/// above the batch budget in the middle so the streamed path is exercised
/// too. Output bytes and per-op checksums must be exactly the plan's.
#[tokio::test]
async fn batched_apply_matches_plan_across_batch_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let big_run = super::types::APPLY_BATCH_BYTES as usize + 4096;
    let small_ops = 3000usize;
    let small = 4096usize;
    let old_len = big_run + small_ops * small;
    let old: Vec<u8> = (0..old_len)
        .map(|i| (i.wrapping_mul(31) % 253) as u8)
        .collect();
    let target_path = dir.path().join("file.pbo");
    std::fs::write(&target_path, &old).unwrap();
    let blob: Vec<u8> = (0..small_ops / 2 * small)
        .map(|i| (i.wrapping_mul(17) % 241) as u8)
        .collect();
    let blob_path = dir.path().join("file.blob");
    std::fs::write(&blob_path, &blob).unwrap();

    let mut ops = Vec::new();
    let mut expected = Vec::new();
    let mut dest = 0u64;
    let mut blob_offset = 0u64;
    let mut order = 0i64;
    let mut push = |op_type: PatchOpType, bytes: &[u8], source_start: Option<u64>| {
        let blob_at = (op_type == PatchOpType::InsertRemote).then_some(blob_offset);
        ops.push(patch_op(
            order,
            op_type,
            dest,
            bytes.len() as u64,
            md5_upper(bytes),
            source_start,
            blob_at,
        ));
        expected.extend_from_slice(bytes);
        dest += bytes.len() as u64;
        if blob_at.is_some() {
            blob_offset += bytes.len() as u64;
        }
        order += 1;
    };
    for i in 0..small_ops / 2 {
        let src = big_run + i * small;
        push(
            PatchOpType::CopyLocal,
            &old[src..src + small],
            Some(src as u64),
        );
        let at = i * small;
        push(PatchOpType::InsertRemote, &blob[at..at + small], None);
        if i == small_ops / 4 {
            push(PatchOpType::CopyLocal, &old[..big_run], Some(0));
        }
    }
    let segments = super::types::coalesce_apply_segments(&ops).unwrap();
    let batches =
        super::types::plan_apply_batches(&segments, &ops, super::types::APPLY_BATCH_BYTES);
    assert!(batches.len() >= 3, "batches={}", batches.len());
    assert!(
        batches
            .iter()
            .any(|b| b.is_oversized(super::types::APPLY_BATCH_BYTES))
    );

    let artifact = PatchArtifact {
        schema_version: super::types::PATCH_SCHEMA_VERSION,
        repository_url: "http://127.0.0.1:1/repo/".to_string(),
        file_id: 7,
        local_target_path: target_path.to_string_lossy().to_string(),
        remote_url: "http://127.0.0.1:1/repo/@a/file.pbo".to_string(),
        base_file_expected_size: old.len() as u64,
        new_file_expected_size: expected.len() as u64,
        new_file_remote_checksum: compute_tree_checksum_from_segment_checksums(
            ops.iter().map(|op| op.target_checksum.as_str()),
        ),
        operations: Vec::new(),
    };
    let patch_file = DownloadPatchFile {
        file_id: 7,
        patch_json_path: String::new(),
        patch_blob_path: blob_path.to_string_lossy().to_string(),
        planned_copy_bytes: 0,
        planned_download_bytes: 0,
        status: String::new(),
        last_error: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let db = crate::core::tasks::db_turso::build_test_database().await;
    let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
    let (_pause_tx, pause_rx) = tokio::sync::watch::channel(false);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let (tmp_path, segment_checksums) =
        apply_patch_to_temp_file(context, &artifact, &patch_file, &ops, pause_rx, cancel_rx)
            .await
            .expect("apply");

    assert_eq!(std::fs::read(&tmp_path).unwrap(), expected);
    let expected_checksums: Vec<String> = ops.iter().map(|op| op.target_checksum.clone()).collect();
    assert_eq!(segment_checksums, expected_checksums);
}

/// Minimal HTTP/1.1 range server over a fixed byte buffer, enough for the
/// delta transfer's `Range`/`Content-Range` contract. Counts requests so a
/// test can assert how many round trips a plan cost.
fn spawn_range_server(bytes: Arc<Vec<u8>>) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/file.pbo", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let bytes = bytes.clone();
            let counter = counter.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut range: Option<(u64, u64)> = None;
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 {
                            return;
                        }
                        let header = header.trim();
                        if header.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(": ")
                            && name.eq_ignore_ascii_case("range")
                            && let Some((start, end)) =
                                value.trim_start_matches("bytes=").split_once('-')
                        {
                            range = Some((start.parse().unwrap(), end.parse().unwrap()));
                        }
                    }
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((start, end)) = range else {
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK
Content-Length: 0

",
                        );
                        continue;
                    };
                    let body = &bytes[start as usize..=end as usize];
                    let head = format!(
                        "HTTP/1.1 206 Partial Content
Content-Range: bytes {}-{}/{}
Content-Length: {}

",
                        start,
                        end,
                        bytes.len(),
                        body.len()
                    );
                    if stream.write_all(head.as_bytes()).is_err() || stream.write_all(body).is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    (url, requests)
}

/// Insert ops adjacent in the output, one small copy gap and one large gap.
/// Nothing has measured the link yet, so the floor budget applies: the small
/// gap is bridged, the large one is not, the blob must hold exactly the
/// planned insert bytes and the server must see two requests, not four.
#[tokio::test]
async fn blob_download_coalesces_adjacent_inserts_into_few_requests() {
    let remote: Vec<u8> = (0..450_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let remote = Arc::new(remote);
    let (url, requests) = spawn_range_server(remote.clone());
    let dir = tempfile::tempdir().unwrap();
    let blob_path = dir.path().join("file.blob");

    // Layout: insert 0..10k, insert 10k..30k, copy 30k..31k (bridged gap),
    // insert 31k..50k, copy 50k..400k (too large to bridge), insert 400k..450k.
    let layout: Vec<(PatchOpType, u64, u64)> = vec![
        (PatchOpType::InsertRemote, 0, 10_000),
        (PatchOpType::InsertRemote, 10_000, 20_000),
        (PatchOpType::CopyLocal, 30_000, 1_000),
        (PatchOpType::InsertRemote, 31_000, 19_000),
        (PatchOpType::CopyLocal, 50_000, 350_000),
        (PatchOpType::InsertRemote, 400_000, 50_000),
    ];
    let mut ops = Vec::new();
    let mut blob_offset = 0u64;
    let mut expected_blob = Vec::new();
    for (order, (op_type, dest, len)) in layout.iter().enumerate() {
        let bytes = &remote[*dest as usize..(*dest + *len) as usize];
        let blob_at = (*op_type == PatchOpType::InsertRemote).then_some(blob_offset);
        ops.push(patch_op(
            order as i64,
            *op_type,
            *dest,
            *len,
            md5_upper(bytes),
            (*op_type == PatchOpType::CopyLocal).then_some(*dest),
            blob_at,
        ));
        if blob_at.is_some() {
            expected_blob.extend_from_slice(bytes);
            blob_offset += *len;
        }
    }
    std::fs::write(&blob_path, vec![0u8; expected_blob.len()]).unwrap();
    let runs = super::types::plan_insert_runs(
        &ops,
        super::types::insert_run_gap_budget(0),
        super::types::INSERT_RUN_MAX_BYTES,
    );
    assert_eq!(runs.len(), 2);

    let artifact = PatchArtifact {
        schema_version: super::types::PATCH_SCHEMA_VERSION,
        repository_url: "http://127.0.0.1:1/repo/".to_string(),
        file_id: 7,
        local_target_path: dir.path().join("file.pbo").to_string_lossy().to_string(),
        remote_url: url,
        base_file_expected_size: remote.len() as u64,
        new_file_expected_size: remote.len() as u64,
        new_file_remote_checksum: String::new(),
        operations: Vec::new(),
    };
    let patch_file = DownloadPatchFile {
        file_id: 7,
        patch_json_path: String::new(),
        patch_blob_path: blob_path.to_string_lossy().to_string(),
        planned_copy_bytes: 0,
        planned_download_bytes: expected_blob.len() as u64,
        status: String::new(),
        last_error: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let db = crate::core::tasks::db_turso::build_test_database().await;
    let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
    let (_pause_tx, pause_rx) = tokio::sync::watch::channel(false);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let metrics = Arc::new(crate::core::tasks::download_files::DownloadMetrics::new());

    super::transfer::download_patch_blob_ranges_parallel(
        context,
        &artifact,
        &patch_file,
        &mut ops,
        pause_rx,
        cancel_rx,
        4,
        Arc::new(crate::core::tasks::download_files::AdaptiveBandwidthLimiter::Unlimited),
        metrics,
    )
    .await
    .expect("blob download");

    assert_eq!(std::fs::read(&blob_path).unwrap(), expected_blob);
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    for op in ops
        .iter()
        .filter(|op| PatchOpType::from_str(&op.op_type) == Some(PatchOpType::InsertRemote))
    {
        assert_eq!(op.downloaded_bytes, op.length);
    }
}
