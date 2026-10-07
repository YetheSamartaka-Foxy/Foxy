use super::persistence::{persist_file_checksums, persist_mod_checksums, persist_part_checksums};
use super::propagation::update_mod_hashes_for_mods;
use super::*;

/// One part of a delta-patched file as the apply wrote it: verified against
/// the remote part checksum before the temp file was promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchedSegment {
    pub(crate) dest_start: u64,
    pub(crate) length: u64,
    pub(crate) checksum: String,
}

/// The per-part checksums a successful delta-patch apply produced, in
/// `data_order`. They describe bytes the apply just wrote, so they can stand
/// in for a from-disk re-read of the file when they match the remote layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchedFileSegments {
    pub(crate) file_id: u64,
    pub(crate) parts: Vec<PatchedSegment>,
    /// Fingerprint of the promoted output taken while it was still in the
    /// page cache, so the content-hash refresh need not sample it cold.
    pub(crate) content_hash: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct SegmentVerifiedOutcome {
    pub(crate) accepted: HashSet<u64>,
    pub(crate) rejected: HashSet<u64>,
}

/// Whether `segments` can be trusted as the file's local part state: every
/// part must sit at the remote offset with the remote length and checksum, and
/// the ordered rollup must equal the file's remote checksum. Anything else
/// falls back to the from-disk re-read.
pub(crate) fn segments_match_remote_layout(
    tree: &Tree,
    file_idx: usize,
    segments: &PatchedFileSegments,
) -> bool {
    let Some(file) = tree.files.get(file_idx) else {
        return false;
    };
    let Some(file_node) = tree.file_nodes.get(file_idx) else {
        return false;
    };
    if file.id != segments.file_id
        || file.remote_checksum.trim().is_empty()
        || file_node.parts.is_empty()
        || file_node.parts.len() != segments.parts.len()
    {
        return false;
    }

    let mut rollup_parts: Vec<FoxyModFilePart> = Vec::with_capacity(file_node.parts.len());
    for (&part_idx, segment) in file_node.parts.iter().zip(&segments.parts) {
        let Some(part) = tree.parts.get(part_idx) else {
            return false;
        };
        if !FoxyModFilePart::id_is_persisted_rowid(part.id)
            || part.remote_start != segment.dest_start
            || part.remote_length != segment.length
            || part.remote_checksum.trim().is_empty()
            || !part
                .remote_checksum
                .trim()
                .eq_ignore_ascii_case(segment.checksum.trim())
        {
            return false;
        }
        let mut verified = part.clone();
        verified.local_checksum = part.remote_checksum.clone();
        rollup_parts.push(verified);
    }

    calculate_hash_from_items(&mut rollup_parts).eq_ignore_ascii_case(file.remote_checksum.trim())
}

/// Record the delta-patched files whose segments pass
/// [`segments_match_remote_layout`] as verified in `tree` and in the database:
/// part local state, file checksum, and the owning addons' rollups. Rejected
/// files are left untouched for the from-disk hash.
pub(crate) async fn apply_segment_verified_files(
    context: Arc<FoxyContext>,
    tree: &mut Tree,
    files: &[PatchedFileSegments],
) -> SegmentVerifiedOutcome {
    let mut outcome = SegmentVerifiedOutcome::default();
    if files.is_empty() {
        return outcome;
    }

    let mut part_updates: Vec<FoxyModFilePart> = Vec::new();
    let mut file_updates: Vec<FoxyModFile> = Vec::new();
    let mut accepted_file_indices: HashSet<usize> = HashSet::new();
    let mut fresh_content_hashes: Vec<(u64, String)> = Vec::new();
    for segments in files {
        let Some(&file_idx) = tree.file_id_to_index.get(&segments.file_id) else {
            outcome.rejected.insert(segments.file_id);
            continue;
        };
        if !segments_match_remote_layout(tree, file_idx, segments) {
            outcome.rejected.insert(segments.file_id);
            continue;
        }
        let part_indices = tree.file_nodes[file_idx].parts.clone();
        for part_idx in part_indices {
            let part = &mut tree.parts[part_idx];
            let changed = part.local_checksum != part.remote_checksum
                || part.local_start != part.remote_start
                || part.local_length != part.remote_length;
            part.local_checksum = part.remote_checksum.clone();
            part.local_start = part.remote_start;
            part.local_length = part.remote_length;
            if changed {
                part_updates.push(part.clone());
            }
        }
        let file = &mut tree.files[file_idx];
        if file.local_checksum != file.remote_checksum {
            file.local_checksum = file.remote_checksum.clone();
            file_updates.push(file.clone());
        }
        accepted_file_indices.insert(file_idx);
        outcome.accepted.insert(segments.file_id);
        if let Some(content_hash) = segments.content_hash.clone() {
            fresh_content_hashes.push((segments.file_id, content_hash));
        }
    }

    if accepted_file_indices.is_empty() {
        return outcome;
    }
    context.record_fresh_file_content_hashes(fresh_content_hashes);

    let mod_indices: HashSet<usize> = tree
        .mod_nodes
        .iter()
        .filter(|node| {
            node.files
                .iter()
                .any(|file_idx| accepted_file_indices.contains(file_idx))
        })
        .map(|node| node.mod_idx)
        .collect();
    let updated_mods = update_mod_hashes_for_mods(tree, Some(&mod_indices));
    let mod_updates: Vec<FoxyMod> = updated_mods
        .iter()
        .filter_map(|&mod_idx| tree.mods.get(mod_idx).cloned())
        .collect();

    let db = context.db();
    part_updates.sort_by_key(|part| part.id);
    persist_part_checksums(&db, &part_updates, |_| {}).await;
    persist_file_checksums(&db, &file_updates, |_| {}).await;
    persist_mod_checksums(&db, &mod_updates, |_| {}).await;

    info!(
        "Segment-verified hash state persisted: files={} parts={} addons={} rejected={}",
        outcome.accepted.len(),
        part_updates.len(),
        mod_updates.len(),
        outcome.rejected.len()
    );
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::model_tree::{FileNode, ModNode};

    fn part(
        id: u64,
        file_id: u64,
        order: i64,
        start: u64,
        length: u64,
        checksum: &str,
    ) -> FoxyModFilePart {
        FoxyModFilePart {
            id,
            file_id,
            data_order: order,
            remote_start: start,
            remote_length: length,
            remote_checksum: checksum.to_string(),
            ..Default::default()
        }
    }

    fn md5_rollup(checksums: &[&str]) -> String {
        let mut parts: Vec<FoxyModFilePart> = checksums
            .iter()
            .enumerate()
            .map(|(idx, checksum)| {
                let mut part = part(idx as u64 + 1, 1, idx as i64, 0, 0, checksum);
                part.local_checksum = checksum.to_string();
                part
            })
            .collect();
        calculate_hash_from_items(&mut parts)
    }

    fn tree_with_file(part_checksums: &[&str]) -> Tree {
        let mut tree = Tree::default();
        tree.mods.push(FoxyMod {
            id: 5,
            remote_checksum: "MOD_REMOTE".to_string(),
            ..Default::default()
        });
        tree.files.push(FoxyModFile {
            id: 1,
            remote_checksum: md5_rollup(part_checksums),
            ..Default::default()
        });
        let mut start = 0;
        let mut part_indices = Vec::new();
        for (idx, checksum) in part_checksums.iter().enumerate() {
            part_indices.push(tree.parts.len());
            tree.parts
                .push(part(idx as u64 + 1, 1, idx as i64, start, 10, checksum));
            start += 10;
        }
        tree.file_nodes.push(FileNode {
            file_idx: 0,
            parts: part_indices,
        });
        tree.mod_nodes.push(ModNode {
            mod_idx: 0,
            files: vec![0],
        });
        tree.file_id_to_index.insert(1, 0);
        tree
    }

    fn segments(checksums: &[&str]) -> PatchedFileSegments {
        PatchedFileSegments {
            file_id: 1,
            parts: checksums
                .iter()
                .enumerate()
                .map(|(idx, checksum)| PatchedSegment {
                    dest_start: idx as u64 * 10,
                    length: 10,
                    checksum: checksum.to_string(),
                })
                .collect(),
            content_hash: None,
        }
    }

    #[test]
    fn segments_matching_the_remote_layout_are_accepted() {
        let tree = tree_with_file(&["AA", "BB", "CC"]);
        assert!(segments_match_remote_layout(
            &tree,
            0,
            &segments(&["aa", "bb", "cc"])
        ));
    }

    #[test]
    fn segments_with_a_wrong_part_checksum_are_rejected() {
        let tree = tree_with_file(&["AA", "BB", "CC"]);
        assert!(!segments_match_remote_layout(
            &tree,
            0,
            &segments(&["AA", "XX", "CC"])
        ));
    }

    #[test]
    fn segments_with_a_different_part_count_are_rejected() {
        let tree = tree_with_file(&["AA", "BB", "CC"]);
        assert!(!segments_match_remote_layout(
            &tree,
            0,
            &segments(&["AA", "BB"])
        ));
    }

    #[test]
    fn segments_at_a_shifted_offset_are_rejected() {
        let tree = tree_with_file(&["AA", "BB"]);
        let mut shifted = segments(&["AA", "BB"]);
        shifted.parts[1].dest_start = 11;
        assert!(!segments_match_remote_layout(&tree, 0, &shifted));
    }

    #[test]
    fn segments_whose_rollup_disagrees_with_the_file_are_rejected() {
        let mut tree = tree_with_file(&["AA", "BB"]);
        tree.files[0].remote_checksum = "STALE_FILE_ROLLUP".to_string();
        assert!(!segments_match_remote_layout(
            &tree,
            0,
            &segments(&["AA", "BB"])
        ));
    }

    #[tokio::test]
    async fn accepted_segments_roll_up_to_the_same_state_as_a_hash_pass() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
        let mut tree = tree_with_file(&["AA", "BB", "CC"]);
        let expected_file = tree.files[0].remote_checksum.clone();

        let outcome = apply_segment_verified_files(
            context,
            &mut tree,
            &[
                segments(&["AA", "BB", "CC"]),
                PatchedFileSegments {
                    file_id: 99,
                    parts: Vec::new(),
                    content_hash: None,
                },
            ],
        )
        .await;

        assert_eq!(outcome.accepted, HashSet::from([1]));
        assert_eq!(outcome.rejected, HashSet::from([99]));
        assert_eq!(tree.files[0].local_checksum, expected_file);
        assert!(tree.parts.iter().all(|part| {
            part.local_checksum == part.remote_checksum
                && part.local_start == part.remote_start
                && part.local_length == part.remote_length
        }));
        // The only file matches remote, so the addon rollup takes the remote value.
        assert_eq!(tree.mods[0].local_checksum, "MOD_REMOTE");
    }

    #[tokio::test]
    async fn accepted_segments_hand_their_fingerprint_to_the_refresh() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
        let mut tree = tree_with_file(&["AA", "BB"]);
        let mut accepted = segments(&["AA", "BB"]);
        accepted.content_hash = Some("fp-accepted".to_string());
        let mut rejected = segments(&["AA", "XX"]);
        rejected.content_hash = Some("fp-rejected".to_string());

        let outcome =
            apply_segment_verified_files(context.clone(), &mut tree, &[accepted, rejected]).await;

        assert_eq!(outcome.accepted, HashSet::from([1]));
        assert_eq!(
            context.take_fresh_file_content_hash(1).as_deref(),
            Some("fp-accepted")
        );
        assert!(context.take_fresh_file_content_hash(1).is_none());
    }
}
