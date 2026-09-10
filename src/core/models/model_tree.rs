use crate::core::db::{DbErr, DbRow, DbTxn, DbValue, params};
use crate::core::models::context::FoxyContext;
use crate::core::models::modification::{self, FoxyMod};
use crate::core::models::modification_file::{self, FoxyModFile};
use crate::core::models::modification_file_part::{self, FoxyModFilePart};
use crate::core::models::repository::{
    self, FoxyRepository, normalize_repository_local_path_identity,
};
use crate::core::tasks::init_database::read_chunk_ids;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Raw tables loaded inside the read transaction, before in-memory tree assembly.
#[derive(Default)]
struct RawTreeData {
    repositories: Vec<FoxyRepository>,
    mods: Vec<FoxyMod>,
    files: Vec<FoxyModFile>,
    parts: Vec<FoxyModFilePart>,
    repository_addons: Vec<(i64, i64)>,
    addon_files: Vec<(i64, i64)>,
}

/// Run `SELECT … WHERE col IN (…)` over `ids` in bind-variable-safe chunks,
/// applying `suffix` (e.g. an ORDER BY) to each chunk, and hand every row to
/// `visit` as it arrives.
///
/// Streaming rather than returning rows is what keeps a large load from holding
/// the raw rows and the structs built from them at the same time; the parts of
/// a 141k-part repository are both representations of the same data, and only
/// one of them is wanted.
async fn for_each_id_chunk<F>(
    tx: &DbTxn<'_>,
    prefix: &str,
    suffix: &str,
    ids: &[i64],
    chunk_size: usize,
    mut visit: F,
) -> Result<(), DbErr>
where
    F: FnMut(&DbRow) -> Result<(), DbErr>,
{
    let mut idx = 0;
    while idx < ids.len() {
        let end = (idx + chunk_size).min(ids.len());
        let chunk = &ids[idx..end];
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!("{prefix}({placeholders}){suffix}");
        let chunk_params: Vec<DbValue> = chunk.iter().map(|id| DbValue::from(*id)).collect();
        tx.query_each(&sql, chunk_params, &mut visit).await?;
        idx = end;
    }
    Ok(())
}

/// Collect `SELECT … WHERE col IN (…)` rows through a per-row mapper.
async fn map_id_chunks<T, F>(
    tx: &DbTxn<'_>,
    prefix: &str,
    suffix: &str,
    ids: &[i64],
    chunk_size: usize,
    map: F,
) -> Result<Vec<T>, DbErr>
where
    F: Fn(&DbRow) -> Result<T, DbErr>,
{
    let mut out = Vec::new();
    for_each_id_chunk(tx, prefix, suffix, ids, chunk_size, |row| {
        out.push(map(row)?);
        Ok(())
    })
    .await?;
    Ok(out)
}

fn sort_parts_for_tree_load(parts: &mut [FoxyModFilePart]) {
    parts.sort_unstable_by_key(|part| (part.file_id, part.data_order, part.id));
}

/// Bucket part indices under the file each part belongs to.
///
/// `parts` arrives grouped by file and ascending in `data_order` within a file
/// (`sort_parts_for_tree_load`, plus deferred rows attached in the same order),
/// which is the order a file's bucket needs, so this is a scan. The pair list,
/// part-id map and global re-sort it replaces produced the same buckets and cost
/// more than the part rows they indexed.
fn assign_parts_to_files(
    parts: &[FoxyModFilePart],
    file_id_to_index: &HashMap<u64, usize>,
    file_nodes: &mut [FileNode],
) {
    for (index, part) in parts.iter().enumerate() {
        if let Some(&file_index) = file_id_to_index.get(&part.file_id) {
            file_nodes[file_index].parts.push(index);
        }
    }
}

fn deferred_part_synthetic_id(attached: usize) -> u64 {
    u64::MAX - attached as u64
}

/// Runtime tree node types (link indices to children)
#[derive(Debug, Clone)]
pub struct RepositoryNode {
    pub repo_idx: usize,
    pub mods: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct ModNode {
    pub mod_idx: usize,
    pub files: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct FileNode {
    pub file_idx: usize,
    pub parts: Vec<usize>,
}

/// The complete in-memory model tree
#[derive(Debug, Default)]
pub struct Tree {
    pub repositories: Vec<FoxyRepository>,
    pub mods: Vec<FoxyMod>,
    pub files: Vec<FoxyModFile>,
    pub parts: Vec<FoxyModFilePart>,

    pub repo_nodes: Vec<RepositoryNode>,
    pub mod_nodes: Vec<ModNode>,
    pub file_nodes: Vec<FileNode>,

    pub file_id_to_index: HashMap<u64, usize>,
}

impl Tree {
    /// Loads all tables in a single transaction and builds the ordered tree
    pub async fn load(context: Arc<FoxyContext>, remote_repository_url: &str) -> Result<Self> {
        Self::load_scoped(context, remote_repository_url, None, None, false).await
    }

    /// Loads the repository tree while restricting file/part rows to the named addons.
    pub async fn load_for_mod_names(
        context: Arc<FoxyContext>,
        remote_repository_url: &str,
        mod_names: &HashSet<String>,
    ) -> Result<Self> {
        Self::load_scoped(context, remote_repository_url, Some(mod_names), None, false).await
    }

    /// Loads all repository/addon rows but restricts file/part rows to addons
    /// containing the requested files. This keeps repository rollups correct
    /// without loading unrelated subfile rows.
    pub async fn load_for_files(
        context: Arc<FoxyContext>,
        remote_repository_url: &str,
        file_ids: &HashSet<u64>,
    ) -> Result<Self> {
        Self::load_scoped(context, remote_repository_url, None, Some(file_ids), true).await
    }

    async fn load_scoped(
        context: Arc<FoxyContext>,
        remote_repository_url: &str,
        mod_name_filter: Option<&HashSet<String>>,
        file_id_filter: Option<&HashSet<u64>>,
        include_all_repo_mods: bool,
    ) -> Result<Self> {
        let chunk_size = read_chunk_ids();
        let target_local_path = context.target_local_path.clone();
        // Own the borrowed inputs so the read-transaction closure's future is not
        // tied to the caller's lifetimes (the seam's `for<'a>` bound rejects that).
        let remote_repository_url = remote_repository_url.to_string();
        let mod_name_filter = mod_name_filter.cloned();
        let file_id_filter = file_id_filter.cloned();

        // Snapshot read: load every table in one consistent transaction (no write
        // permit, no retry) before assembling the tree in memory.
        let raw: RawTreeData = context
            .db()
            .read_transaction(move |tx| {
                Box::pin(async move {
                    let mut repositories: Vec<FoxyRepository> = tx
                        .query_all(
                            &format!(
                                "SELECT {} FROM repositories WHERE remote_url = ? ORDER BY id ASC",
                                repository::REPOSITORY_COLUMNS
                            ),
                            params![remote_repository_url],
                        )
                        .await?
                        .iter()
                        .map(repository::repository_from_row)
                        .collect::<Result<_, DbErr>>()?;

                    if let Some(target) = target_local_path.as_deref() {
                        let target_key = normalize_repository_local_path_identity(target);
                        repositories.retain(|repo| {
                            normalize_repository_local_path_identity(&repo.local_path) == target_key
                        });
                    }

                    if repositories.is_empty() {
                        return Ok(RawTreeData::default());
                    }

                    let repo_ids: Vec<i64> = repositories.iter().map(|r| r.id as i64).collect();

                    let repository_addons: Vec<(i64, i64)> = map_id_chunks(
                        tx,
                        "SELECT repository_id, addon_id FROM repository_addons WHERE repository_id IN ",
                        "",
                        &repo_ids,
                        chunk_size,
                        |row| Ok((row.get_i64("repository_id")?, row.get_i64("addon_id")?)),
                    )
                    .await?;

                    let linked_mod_ids: HashSet<i64> =
                        repository_addons.iter().map(|(_, addon_id)| *addon_id).collect();

                    let mut mods: Vec<FoxyMod> = if linked_mod_ids.is_empty() {
                        Vec::new()
                    } else {
                        let mut ids: Vec<i64> = linked_mod_ids.iter().copied().collect();
                        ids.sort_unstable();
                        map_id_chunks(
                            tx,
                            &format!(
                                "SELECT {} FROM addons WHERE id IN ",
                                modification::ADDON_COLUMNS
                            ),
                            " ORDER BY data_order ASC, id ASC",
                            &ids,
                            chunk_size,
                            FoxyMod::from_row,
                        )
                        .await?
                    };

                    let scoped_mod_ids: HashSet<i64> = if let Some(filter) =
                        mod_name_filter.as_ref()
                    {
                        mods.iter()
                            .filter(|m| filter.contains(&m.name.to_lowercase()))
                            .map(|m| m.id as i64)
                            .collect()
                    } else if let Some(file_ids) = file_id_filter.as_ref() {
                        let mut scoped = HashSet::new();
                        if !file_ids.is_empty() && !linked_mod_ids.is_empty() {
                            let mut ids: Vec<i64> =
                                file_ids.iter().map(|id| *id as i64).collect();
                            ids.sort_unstable();
                            for_each_id_chunk(
                                tx,
                                "SELECT addon_id, file_id FROM addon_files WHERE file_id IN ",
                                "",
                                &ids,
                                chunk_size,
                                |row| {
                                    let addon_id = row.get_i64("addon_id")?;
                                    if linked_mod_ids.contains(&addon_id) {
                                        scoped.insert(addon_id);
                                    }
                                    Ok(())
                                },
                            )
                            .await?;
                        }
                        scoped
                    } else {
                        mods.iter().map(|m| m.id as i64).collect()
                    };

                    if (mod_name_filter.is_some() || file_id_filter.is_some())
                        && !include_all_repo_mods
                    {
                        mods.retain(|m| scoped_mod_ids.contains(&(m.id as i64)));
                    }

                    let addon_file_mod_ids: Vec<i64> =
                        if mod_name_filter.is_some() || file_id_filter.is_some() {
                            scoped_mod_ids.iter().copied().collect()
                        } else {
                            mods.iter().map(|m| m.id as i64).collect()
                        };

                    let addon_files: Vec<(i64, i64)> = if addon_file_mod_ids.is_empty() {
                        Vec::new()
                    } else {
                        let mut ids = addon_file_mod_ids;
                        ids.sort_unstable();
                        map_id_chunks(
                            tx,
                            "SELECT addon_id, file_id FROM addon_files WHERE addon_id IN ",
                            "",
                            &ids,
                            chunk_size,
                            |row| Ok((row.get_i64("addon_id")?, row.get_i64("file_id")?)),
                        )
                        .await?
                    };

                    let file_ids: HashSet<i64> =
                        addon_files.iter().map(|(_, file_id)| *file_id).collect();

                    let files: Vec<FoxyModFile> = if file_ids.is_empty() {
                        Vec::new()
                    } else {
                        let mut ids: Vec<i64> = file_ids.into_iter().collect();
                        ids.sort_unstable();
                        map_id_chunks(
                            tx,
                            &format!(
                                "SELECT {} FROM files WHERE id IN ",
                                modification_file::FILE_COLUMNS
                            ),
                            " ORDER BY data_order ASC, id ASC",
                            &ids,
                            chunk_size,
                            FoxyModFile::from_row,
                        )
                        .await?
                    };

                    // Load parts directly by file_id. ORDER BY is applied in process:
                    // the matching index does not pay for a ten-column ordered scan.
                    let mut parts: Vec<FoxyModFilePart> = Vec::new();
                    if !files.is_empty() {
                        let mut ids: Vec<i64> = files.iter().map(|f| f.id as i64).collect();
                        ids.sort_unstable();
                        parts = map_id_chunks(
                            tx,
                            &format!(
                                "SELECT {} FROM subfiles WHERE file_id IN ",
                                modification_file_part::SUBFILE_COLUMNS
                            ),
                            "",
                            &ids,
                            chunk_size,
                            FoxyModFilePart::from_row,
                        )
                        .await?;
                        sort_parts_for_tree_load(&mut parts);
                    }

                    Ok(RawTreeData {
                        repositories,
                        mods,
                        files,
                        parts,
                        repository_addons,
                        addon_files,
                    })
                })
            })
            .await?;

        if raw.repositories.is_empty() {
            return Ok(Tree::default());
        }

        let RawTreeData {
            repositories,
            mods,
            files,
            mut parts,
            mut repository_addons,
            mut addon_files,
        } = raw;

        let deferred_parts = context.deferred_parts_snapshot();
        if !deferred_parts.is_empty() {
            let file_ids: HashSet<i64> = files.iter().map(|file| file.id as i64).collect();
            let mut attached = 0usize;
            // Attach in each file's part order: the persisted rows above are
            // already sorted that way, and a file's parts are read positionally.
            let mut deferred: Vec<_> = deferred_parts
                .into_iter()
                .filter(|row| file_ids.contains(&row.file_id))
                .collect();
            deferred.sort_by_key(|row| (row.file_id, row.data_order));
            for row in deferred {
                let file_id = row.file_id;
                let synthetic_id = deferred_part_synthetic_id(attached);
                parts.push(FoxyModFilePart {
                    id: synthetic_id,
                    file_id: file_id as u64,
                    path: row.path,
                    remote_length: row.remote_length as u64,
                    local_length: 0,
                    remote_start: row.remote_start as u64,
                    local_start: 0,
                    remote_checksum: row.remote_checksum,
                    local_checksum: String::new(),
                    data_order: row.data_order,
                });
                attached += 1;
            }
            if attached > 0 {
                log::info!(
                    "Attached {} deferred manifest part rows to in-memory tree without pre-hash DB insert",
                    attached
                );
            }
        }

        let mod_order: HashMap<i64, i64> =
            mods.iter().map(|m| (m.id as i64, m.data_order)).collect();
        let file_order: HashMap<i64, i64> =
            files.iter().map(|f| (f.id as i64, f.data_order)).collect();

        repository_addons
            .sort_by_key(|(_, mod_id)| mod_order.get(mod_id).cloned().unwrap_or_default());

        addon_files
            .sort_by_key(|(_, file_id)| file_order.get(file_id).cloned().unwrap_or_default());

        let mut repo_id_to_index = HashMap::with_capacity(repositories.len());
        for (i, r) in repositories.iter().enumerate() {
            repo_id_to_index.insert(r.id, i);
        }

        let mut mod_id_to_index = HashMap::with_capacity(mods.len());
        for (i, m) in mods.iter().enumerate() {
            mod_id_to_index.insert(m.id, i);
        }

        let mut file_id_to_index = HashMap::with_capacity(files.len());
        for (i, f) in files.iter().enumerate() {
            file_id_to_index.insert(f.id, i);
        }

        let mut repo_nodes = Vec::with_capacity(repositories.len());
        let mut mod_nodes = Vec::with_capacity(mods.len());
        let mut file_nodes = Vec::with_capacity(files.len());

        for i in 0..repositories.len() {
            repo_nodes.push(RepositoryNode {
                repo_idx: i,
                mods: Vec::new(),
            });
        }
        for i in 0..mods.len() {
            mod_nodes.push(ModNode {
                mod_idx: i,
                files: Vec::new(),
            });
        }
        for i in 0..files.len() {
            file_nodes.push(FileNode {
                file_idx: i,
                parts: Vec::new(),
            });
        }

        for (repo_id, mod_id) in repository_addons {
            if let (Some(&ridx), Some(&midx)) = (
                repo_id_to_index.get(&(repo_id as u64)),
                mod_id_to_index.get(&(mod_id as u64)),
            ) {
                repo_nodes[ridx].mods.push(midx);
            }
        }
        for (mod_id, file_id) in addon_files {
            if let (Some(&midx), Some(&fidx)) = (
                mod_id_to_index.get(&(mod_id as u64)),
                file_id_to_index.get(&(file_id as u64)),
            ) {
                mod_nodes[midx].files.push(fidx);
            }
        }
        assign_parts_to_files(&parts, &file_id_to_index, &mut file_nodes);

        let mut tree = Tree {
            repositories,
            mods,
            files,
            parts,
            repo_nodes,
            mod_nodes,
            file_nodes,
            file_id_to_index,
        };
        tree.apply_derived_clean_part_local_state();
        Ok(tree)
    }

    pub(crate) fn apply_derived_clean_part_local_state(&mut self) -> usize {
        let mut projected = 0usize;
        for file_idx in 0..self.file_nodes.len() {
            let Some(file) = self.files.get(file_idx) else {
                continue;
            };
            let local_checksum = file.local_checksum.clone();
            let remote_checksum = file.remote_checksum.clone();
            if !FoxyModFilePart::file_checksums_are_clean(&local_checksum, &remote_checksum) {
                continue;
            }
            let part_indices = self.file_nodes[file_idx].parts.clone();
            for part_idx in part_indices {
                if self.parts.get_mut(part_idx).is_some_and(|part| {
                    part.apply_derived_clean_local_state(&local_checksum, &remote_checksum)
                }) {
                    projected += 1;
                }
            }
        }
        projected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_file_projects_part_local_state_from_remote() {
        let mut tree = Tree {
            files: vec![FoxyModFile {
                id: 10,
                local_checksum: "FILE".to_string(),
                remote_checksum: "FILE".to_string(),
                ..Default::default()
            }],
            parts: vec![FoxyModFilePart {
                id: 1,
                file_id: 10,
                remote_checksum: "PART".to_string(),
                remote_length: 12,
                remote_start: 4,
                ..Default::default()
            }],
            file_nodes: vec![FileNode {
                file_idx: 0,
                parts: vec![0],
            }],
            ..Default::default()
        };

        assert_eq!(tree.apply_derived_clean_part_local_state(), 1);
        assert_eq!(tree.parts[0].local_checksum, "PART");
        assert_eq!(tree.parts[0].local_length, 12);
        assert_eq!(tree.parts[0].local_start, 4);
    }

    #[test]
    fn dirty_file_keeps_missing_part_local_state() {
        let mut tree = Tree {
            files: vec![FoxyModFile {
                id: 10,
                local_checksum: "OLD".to_string(),
                remote_checksum: "NEW".to_string(),
                ..Default::default()
            }],
            parts: vec![FoxyModFilePart {
                id: 1,
                file_id: 10,
                remote_checksum: "PART".to_string(),
                remote_length: 12,
                remote_start: 4,
                ..Default::default()
            }],
            file_nodes: vec![FileNode {
                file_idx: 0,
                parts: vec![0],
            }],
            ..Default::default()
        };

        assert_eq!(tree.apply_derived_clean_part_local_state(), 0);
        assert!(tree.parts[0].local_checksum.is_empty());
        assert_eq!(tree.parts[0].local_length, 0);
        assert_eq!(tree.parts[0].local_start, 0);
    }

    #[test]
    fn part_reload_sort_matches_query_order() {
        let mut parts = vec![
            FoxyModFilePart {
                id: 3,
                file_id: 2,
                data_order: 0,
                ..Default::default()
            },
            FoxyModFilePart {
                id: 1,
                file_id: 1,
                data_order: 1,
                ..Default::default()
            },
            FoxyModFilePart {
                id: 2,
                file_id: 1,
                data_order: 0,
                ..Default::default()
            },
        ];
        sort_parts_for_tree_load(&mut parts);
        let keys: Vec<(u64, i64, u64)> = parts
            .iter()
            .map(|part| (part.file_id, part.data_order, part.id))
            .collect();
        assert_eq!(keys, vec![(1, 0, 2), (1, 1, 1), (2, 0, 3)]);
    }

    #[test]
    fn deferred_part_synthetic_ids_are_outside_rowid_range() {
        assert!(!FoxyModFilePart::id_is_persisted_rowid(
            deferred_part_synthetic_id(0)
        ));
        assert!(!FoxyModFilePart::id_is_persisted_rowid(
            deferred_part_synthetic_id(12)
        ));
        assert_ne!(deferred_part_synthetic_id(0), deferred_part_synthetic_id(1));
    }

    fn part(id: u64, file_id: u64, data_order: i64) -> FoxyModFilePart {
        FoxyModFilePart {
            id,
            file_id,
            data_order,
            ..Default::default()
        }
    }

    fn buckets(parts: &[FoxyModFilePart], file_ids: &[u64]) -> Vec<Vec<u64>> {
        let index: HashMap<u64, usize> = file_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        let mut nodes: Vec<FileNode> = (0..file_ids.len())
            .map(|file_idx| FileNode {
                file_idx,
                parts: Vec::new(),
            })
            .collect();
        assign_parts_to_files(parts, &index, &mut nodes);
        nodes
            .iter()
            .map(|node| node.parts.iter().map(|i| parts[*i].id).collect())
            .collect()
    }

    /// The order a bucket ends up in is the order the old pair-list build
    /// produced: each file's parts ascending in `data_order`.
    #[test]
    fn file_buckets_hold_each_file_s_parts_in_part_order() {
        let mut parts = vec![part(7, 2, 1), part(5, 1, 1), part(6, 2, 0), part(4, 1, 0)];
        sort_parts_for_tree_load(&mut parts);
        assert_eq!(buckets(&parts, &[1, 2]), vec![vec![4, 5], vec![6, 7]]);
    }

    #[test]
    fn parts_of_an_unloaded_file_are_dropped_rather_than_misfiled() {
        let parts = vec![part(1, 1, 0), part(2, 99, 0)];
        assert_eq!(buckets(&parts, &[1]), vec![vec![1]]);
    }

    /// Deferred manifest rows carry synthetic ids outside the rowid range, so
    /// they cannot be keyed by id; they are placed by position, after the
    /// persisted rows of the same file.
    #[test]
    fn deferred_rows_append_to_their_file_s_bucket() {
        let mut parts = vec![part(1, 1, 0), part(2, 1, 1)];
        sort_parts_for_tree_load(&mut parts);
        parts.push(part(deferred_part_synthetic_id(0), 1, 2));
        let bucket = &buckets(&parts, &[1])[0];
        assert_eq!(bucket.len(), 3);
        assert_eq!(bucket[2], deferred_part_synthetic_id(0));
    }
}
