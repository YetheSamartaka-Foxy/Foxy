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
    file_part_pairs: Vec<(i64, i64)>,
}

/// Run `SELECT … WHERE col IN (…)` over `ids` in bind-variable-safe chunks,
/// applying `suffix` (e.g. an ORDER BY) to each chunk. Returns all rows.
async fn query_ids_in_chunks(
    tx: &DbTxn<'_>,
    prefix: &str,
    suffix: &str,
    ids: &[i64],
    chunk_size: usize,
) -> Result<Vec<DbRow>, DbErr> {
    let mut out = Vec::new();
    let mut idx = 0;
    while idx < ids.len() {
        let end = (idx + chunk_size).min(ids.len());
        let chunk = &ids[idx..end];
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!("{prefix}({placeholders}){suffix}");
        let chunk_params: Vec<DbValue> = chunk.iter().map(|id| DbValue::from(*id)).collect();
        out.extend(tx.query_all(&sql, chunk_params).await?);
        idx = end;
    }
    Ok(out)
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

                    let repository_addons: Vec<(i64, i64)> = query_ids_in_chunks(
                        tx,
                        "SELECT repository_id, addon_id FROM repository_addons WHERE repository_id IN ",
                        "",
                        &repo_ids,
                        chunk_size,
                    )
                    .await?
                    .iter()
                    .map(|row| {
                        Ok::<_, DbErr>((row.get_i64("repository_id")?, row.get_i64("addon_id")?))
                    })
                    .collect::<Result<_, DbErr>>()?;

                    let linked_mod_ids: HashSet<i64> =
                        repository_addons.iter().map(|(_, addon_id)| *addon_id).collect();

                    let mut mods: Vec<FoxyMod> = if linked_mod_ids.is_empty() {
                        Vec::new()
                    } else {
                        let mut ids: Vec<i64> = linked_mod_ids.iter().copied().collect();
                        ids.sort_unstable();
                        query_ids_in_chunks(
                            tx,
                            &format!(
                                "SELECT {} FROM addons WHERE id IN ",
                                modification::ADDON_COLUMNS
                            ),
                            " ORDER BY data_order ASC, id ASC",
                            &ids,
                            chunk_size,
                        )
                        .await?
                        .iter()
                        .map(FoxyMod::from_row)
                        .collect::<Result<_, DbErr>>()?
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
                            let rows = query_ids_in_chunks(
                                tx,
                                "SELECT addon_id, file_id FROM addon_files WHERE file_id IN ",
                                "",
                                &ids,
                                chunk_size,
                            )
                            .await?;
                            for row in &rows {
                                let addon_id = row.get_i64("addon_id")?;
                                if linked_mod_ids.contains(&addon_id) {
                                    scoped.insert(addon_id);
                                }
                            }
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
                        query_ids_in_chunks(
                            tx,
                            "SELECT addon_id, file_id FROM addon_files WHERE addon_id IN ",
                            "",
                            &ids,
                            chunk_size,
                        )
                        .await?
                        .iter()
                        .map(|row| {
                            Ok::<_, DbErr>((row.get_i64("addon_id")?, row.get_i64("file_id")?))
                        })
                        .collect::<Result<_, DbErr>>()?
                    };

                    let file_ids: HashSet<i64> =
                        addon_files.iter().map(|(_, file_id)| *file_id).collect();

                    let files: Vec<FoxyModFile> = if file_ids.is_empty() {
                        Vec::new()
                    } else {
                        let mut ids: Vec<i64> = file_ids.into_iter().collect();
                        ids.sort_unstable();
                        query_ids_in_chunks(
                            tx,
                            &format!(
                                "SELECT {} FROM files WHERE id IN ",
                                modification_file::FILE_COLUMNS
                            ),
                            " ORDER BY data_order ASC, id ASC",
                            &ids,
                            chunk_size,
                        )
                        .await?
                        .iter()
                        .map(FoxyModFile::from_row)
                        .collect::<Result<_, DbErr>>()?
                    };

                    // Load parts directly by file_id using the covering index
                    // (idx_subfiles_file_id_data_order) instead of the two-step join
                    // through file_subfiles. This eliminates an entire link-table scan
                    // and lets the engine satisfy the query from the index alone.
                    let mut parts: Vec<FoxyModFilePart> = Vec::new();
                    let mut file_part_pairs: Vec<(i64, i64)> = Vec::new();
                    if !files.is_empty() {
                        let mut ids: Vec<i64> = files.iter().map(|f| f.id as i64).collect();
                        ids.sort_unstable();
                        let rows = query_ids_in_chunks(
                            tx,
                            &format!(
                                "SELECT {} FROM subfiles WHERE file_id IN ",
                                modification_file_part::SUBFILE_COLUMNS
                            ),
                            " ORDER BY file_id ASC, data_order ASC, id ASC",
                            &ids,
                            chunk_size,
                        )
                        .await?;
                        for row in &rows {
                            let part = FoxyModFilePart::from_row(row)?;
                            file_part_pairs.push((part.file_id as i64, part.id as i64));
                            parts.push(part);
                        }
                    }

                    Ok(RawTreeData {
                        repositories,
                        mods,
                        files,
                        parts,
                        repository_addons,
                        addon_files,
                        file_part_pairs,
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
            mut file_part_pairs,
        } = raw;

        let deferred_parts = context.deferred_parts_snapshot();
        if !deferred_parts.is_empty() {
            let file_ids: HashSet<i64> = files.iter().map(|file| file.id as i64).collect();
            let mut attached = 0usize;
            for row in deferred_parts
                .into_iter()
                .filter(|row| file_ids.contains(&row.file_id))
            {
                let file_id = row.file_id;
                let synthetic_id = parts.len() as u64 + 1;
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
                file_part_pairs.push((file_id, synthetic_id as i64));
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
        let part_order: HashMap<i64, i64> =
            parts.iter().map(|p| (p.id as i64, p.data_order)).collect();

        repository_addons
            .sort_by_key(|(_, mod_id)| mod_order.get(mod_id).cloned().unwrap_or_default());

        addon_files
            .sort_by_key(|(_, file_id)| file_order.get(file_id).cloned().unwrap_or_default());

        file_part_pairs
            .sort_by_key(|(_, part_id)| part_order.get(part_id).cloned().unwrap_or_default());

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

        let mut part_id_to_index = HashMap::with_capacity(parts.len());
        for (i, p) in parts.iter().enumerate() {
            part_id_to_index.insert(p.id, i);
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

        {
            let mut tmp_repo_mods: HashMap<usize, Vec<usize>> =
                HashMap::with_capacity(repository_addons.len());
            let mut tmp_mod_files: HashMap<usize, Vec<usize>> =
                HashMap::with_capacity(addon_files.len());
            let mut tmp_file_parts: HashMap<usize, Vec<usize>> =
                HashMap::with_capacity(file_part_pairs.len());

            for (repo_id, mod_id) in repository_addons {
                if let (Some(&ridx), Some(&midx)) = (
                    repo_id_to_index.get(&(repo_id as u64)),
                    mod_id_to_index.get(&(mod_id as u64)),
                ) {
                    tmp_repo_mods.entry(ridx).or_default().push(midx);
                }
            }
            for (mod_id, file_id) in addon_files {
                if let (Some(&midx), Some(&fidx)) = (
                    mod_id_to_index.get(&(mod_id as u64)),
                    file_id_to_index.get(&(file_id as u64)),
                ) {
                    tmp_mod_files.entry(midx).or_default().push(fidx);
                }
            }
            for (file_id, part_id) in file_part_pairs {
                if let (Some(&fidx), Some(&pidx)) = (
                    file_id_to_index.get(&(file_id as u64)),
                    part_id_to_index.get(&(part_id as u64)),
                ) {
                    tmp_file_parts.entry(fidx).or_default().push(pidx);
                }
            }

            for (ridx, mods) in tmp_repo_mods {
                repo_nodes[ridx].mods = mods;
            }
            for (midx, files) in tmp_mod_files {
                mod_nodes[midx].files = files;
            }
            for (fidx, parts) in tmp_file_parts {
                file_nodes[fidx].parts = parts;
            }
        }

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
}
