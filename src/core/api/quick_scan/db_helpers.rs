use super::super::*;
#[cfg(test)]
use crate::core::db::params;
use crate::core::db::{DbValue, FoxyDb};
use crate::core::models::modification_file::FILE_COLUMNS;

/// Check if all mods, files, and parts under a repository have non-empty `remote_checksum`
/// using JOIN queries instead of sequential chunked count queries.
/// Returns `Some(true)` if all are ready, `Some(false)` if any are missing, `None` on DB error.
#[cfg(test)]
pub(crate) async fn remote_checksum_state_ready_joined(
    db: &FoxyDb,
    repository_id: i64,
    purpose: &str,
) -> Option<bool> {
    // Query 1: check mods and files for missing remote_checksum in a single JOIN
    let row = match db
        .query_one(
            r#"SELECT
                COUNT(DISTINCT ra.addon_id) AS mod_count,
                COUNT(DISTINCT af.file_id) AS file_count,
                SUM(CASE WHEN a.remote_checksum = '' THEN 1 ELSE 0 END) AS missing_mod_checksums,
                SUM(CASE WHEN f.remote_checksum = '' THEN 1 ELSE 0 END) AS missing_file_checksums
            FROM repository_addons ra
            JOIN addons a ON a.id = ra.addon_id
            JOIN addon_files af ON af.addon_id = ra.addon_id
            JOIN files f ON f.id = af.file_id
            WHERE ra.repository_id = ?"#,
            params![repository_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return Some(false),
        Err(err) => {
            warn!(
                "Failed to check remote checksum state via JOIN for {}: {}",
                purpose, err
            );
            return None;
        }
    };

    let mod_count: i64 = row.get_i64("mod_count").unwrap_or(0);
    let file_count: i64 = row.get_i64("file_count").unwrap_or(0);
    let missing_mod: i64 = row.get_i64("missing_mod_checksums").unwrap_or(0);
    let missing_file: i64 = row.get_i64("missing_file_checksums").unwrap_or(0);

    if mod_count == 0 || file_count == 0 {
        return Some(false);
    }
    if missing_mod > 0 || missing_file > 0 {
        return Some(false);
    }

    // Query 2: check parts (optional - if none exist, that's fine)
    let part_row = match db
        .query_one(
            r#"SELECT
                COUNT(*) AS part_count,
                SUM(CASE WHEN sf.remote_checksum = '' THEN 1 ELSE 0 END) AS missing_part_checksums
            FROM repository_addons ra
            JOIN addon_files af ON af.addon_id = ra.addon_id
            JOIN subfiles sf ON sf.file_id = af.file_id
            WHERE ra.repository_id = ?"#,
            params![repository_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return Some(true),
        Err(err) => {
            warn!(
                "Failed to check part remote checksum state via JOIN for {}: {}",
                purpose, err
            );
            return None;
        }
    };

    let part_count: i64 = part_row.get_i64("part_count").unwrap_or(0);
    let missing_part: i64 = part_row.get_i64("missing_part_checksums").unwrap_or(0);

    if part_count == 0 {
        return Some(true);
    }
    Some(missing_part == 0)
}

/// Check if all mods and files under a repository have non-empty `local_content_hash`
/// using a single JOIN query instead of sequential chunked count queries.
/// Returns `Some(true)` if baseline is ready, `Some(false)` if any are missing, `None` on DB error.
#[cfg(test)]
pub(crate) async fn content_hash_baseline_ready_joined(
    db: &FoxyDb,
    repository_id: i64,
    purpose: &str,
) -> Option<bool> {
    let row = match db
        .query_one(
            r#"SELECT
                COUNT(DISTINCT ra.addon_id) AS mod_count,
                COUNT(DISTINCT af.file_id) AS file_count,
                SUM(CASE WHEN a.local_content_hash = '' THEN 1 ELSE 0 END) AS missing_mod_content,
                SUM(CASE WHEN f.local_content_hash = '' THEN 1 ELSE 0 END) AS missing_file_content
            FROM repository_addons ra
            JOIN addons a ON a.id = ra.addon_id
            JOIN addon_files af ON af.addon_id = ra.addon_id
            JOIN files f ON f.id = af.file_id
            WHERE ra.repository_id = ?"#,
            params![repository_id],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return Some(false),
        Err(err) => {
            warn!(
                "Failed to check content hash baseline via JOIN for {}: {}",
                purpose, err
            );
            return None;
        }
    };

    let mod_count: i64 = row.get_i64("mod_count").unwrap_or(0);
    let file_count: i64 = row.get_i64("file_count").unwrap_or(0);
    let missing_mod: i64 = row.get_i64("missing_mod_content").unwrap_or(0);
    let missing_file: i64 = row.get_i64("missing_file_content").unwrap_or(0);

    if mod_count == 0 || file_count == 0 {
        return Some(false);
    }
    Some(missing_mod == 0 && missing_file == 0)
}

pub(super) async fn refresh_files_by_ids(
    db: &FoxyDb,
    file_ids: &[i64],
    chunk_size: usize,
) -> Option<HashMap<i64, FoxyModFile>> {
    let mut files_by_id: HashMap<i64, FoxyModFile> = HashMap::new();
    let mut idx = 0usize;
    while idx < file_ids.len() {
        let end = (idx + chunk_size).min(file_ids.len());
        let chunk = &file_ids[idx..end];
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE id IN ({placeholders}) \
             ORDER BY data_order ASC, id ASC"
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    match FoxyModFile::from_row(&row) {
                        Ok(file) => {
                            files_by_id.insert(file.id as i64, file);
                        }
                        Err(err) => {
                            warn!("Failed to read file row for quick scan: {}", err);
                            return None;
                        }
                    }
                }
            }
            Err(err) => {
                warn!("Failed to reload files for quick scan: {}", err);
                return None;
            }
        }
        idx = end;
    }

    Some(files_by_id)
}

pub(super) async fn load_patch_download_bytes_by_file_ids(
    db: &FoxyDb,
    file_ids: &[i64],
    chunk_size: usize,
) -> HashMap<i64, u64> {
    if file_ids.is_empty() {
        return HashMap::new();
    }

    let mut patch_bytes_by_file_id: HashMap<i64, u64> = HashMap::new();
    let mut idx = 0usize;
    while idx < file_ids.len() {
        let end = (idx + chunk_size).min(file_ids.len());
        let chunk = &file_ids[idx..end];
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT file_id, status, planned_download_bytes \
             FROM download_patch_file WHERE file_id IN ({placeholders})"
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    let status = row.get_string("status").unwrap_or_default();
                    if status.eq_ignore_ascii_case("fallback_full") {
                        continue;
                    }
                    let file_id = row.get_i64("file_id").unwrap_or(0);
                    let planned = row.get_i64("planned_download_bytes").unwrap_or(0).max(0) as u64;
                    patch_bytes_by_file_id
                        .entry(file_id)
                        .and_modify(|current| *current = (*current).min(planned))
                        .or_insert(planned);
                }
            }
            Err(err) => {
                warn!(
                    "Failed to load delta patch size hints for quick scan: {}",
                    err
                );
                return HashMap::new();
            }
        }
        idx = end;
    }

    patch_bytes_by_file_id
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PartChangeStats {
    pub(super) changed_parts: usize,
    pub(super) changed_bytes: u64,
    pub(super) missing_bytes: u64,
    pub(super) total_parts: usize,
    pub(super) missing_local_checksums: usize,
}

pub(super) async fn load_changed_part_stats_by_file_ids(
    db: &FoxyDb,
    file_ids: &[i64],
    chunk_size: usize,
) -> HashMap<i64, PartChangeStats> {
    if file_ids.is_empty() {
        return HashMap::new();
    }

    // Use a single SQL GROUP BY query directly on subfiles instead of the previous
    // two-step application-side join through the file_subfiles junction table.
    // The covering index idx_subfiles_file_id_data_order makes this efficient.
    let mut result: HashMap<i64, PartChangeStats> = HashMap::new();
    for chunk in file_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            r#"SELECT
                    sf.file_id,
                    COUNT(*) as total_parts,
                    SUM(CASE
                        WHEN f.local_checksum IS NOT NULL
                             AND f.local_checksum != ''
                             AND f.local_checksum = f.remote_checksum
                             AND f.remote_checksum != ''
                        THEN 0
                        WHEN sf.local_checksum IS NULL OR sf.local_checksum = ''
                        THEN 1
                        ELSE 0
                    END) as missing_local_checksums,
                    SUM(CASE
                        WHEN f.local_checksum IS NOT NULL
                             AND f.local_checksum != ''
                             AND f.local_checksum = f.remote_checksum
                             AND f.remote_checksum != ''
                        THEN 0
                        WHEN sf.local_checksum IS NOT NULL
                             AND sf.local_checksum != ''
                             AND sf.local_checksum != sf.remote_checksum
                        THEN 1
                        ELSE 0
                    END) as changed_parts,
                    COALESCE(SUM(CASE
                        WHEN f.local_checksum IS NOT NULL
                             AND f.local_checksum != ''
                             AND f.local_checksum = f.remote_checksum
                             AND f.remote_checksum != ''
                        THEN 0
                        WHEN sf.local_checksum IS NOT NULL
                             AND sf.local_checksum != ''
                             AND sf.local_checksum != sf.remote_checksum
                        THEN sf.remote_length
                        ELSE 0
                    END), 0) as changed_bytes,
                    COALESCE(SUM(CASE
                        WHEN f.local_checksum IS NOT NULL
                             AND f.local_checksum != ''
                             AND f.local_checksum = f.remote_checksum
                             AND f.remote_checksum != ''
                        THEN 0
                        WHEN sf.local_checksum IS NULL OR sf.local_checksum = ''
                        THEN sf.remote_length
                        ELSE 0
                    END), 0) as missing_bytes
               FROM subfiles sf
               JOIN files f ON f.id = sf.file_id
               WHERE sf.file_id IN ({})
               GROUP BY sf.file_id"#,
            placeholders
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    let file_id: i64 = row.get_i64("file_id").unwrap_or(0);
                    let total_parts: i64 = row.get_i64("total_parts").unwrap_or(0);
                    let missing_local_checksums: i64 =
                        row.get_i64("missing_local_checksums").unwrap_or(0);
                    let changed_parts: i64 = row.get_i64("changed_parts").unwrap_or(0);
                    let changed_bytes: i64 = row.get_i64("changed_bytes").unwrap_or(0);
                    let missing_bytes: i64 = row.get_i64("missing_bytes").unwrap_or(0);
                    result.insert(
                        file_id,
                        PartChangeStats {
                            changed_parts: changed_parts.max(0) as usize,
                            changed_bytes: changed_bytes.max(0) as u64,
                            missing_bytes: missing_bytes.max(0) as u64,
                            total_parts: total_parts.max(0) as usize,
                            missing_local_checksums: missing_local_checksums.max(0) as usize,
                        },
                    );
                }
            }
            Err(err) => {
                warn!("Failed to load changed part stats for quick scan: {}", err);
                return HashMap::new();
            }
        }
    }

    result
}
