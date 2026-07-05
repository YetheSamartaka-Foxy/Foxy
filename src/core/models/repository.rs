use crate::core::db::{DbErr, DbRow, params};
use crate::core::models::context::FoxyContext;
use crate::core::models::trait_has_local_checksum::HasLocalChecksum;
use log::debug;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Column list selected when materializing a [`FoxyRepository`] from a row.
pub(crate) const REPOSITORY_COLUMNS: &str = "id, name, remote_url, local_path, image, local_checksum, remote_checksum, \
     local_content_hash, foxy_mode";

/// Build a [`FoxyRepository`] from a seam [`DbRow`].
pub(crate) fn repository_from_row(row: &DbRow) -> Result<FoxyRepository, DbErr> {
    Ok(FoxyRepository {
        id: row.get_i64("id")? as u64,
        name: row.get_string("name")?,
        remote_url: row.get_string("remote_url")?,
        local_path: row.get_string("local_path")?,
        image: row.get_string("image")?,
        local_checksum: row.get_string("local_checksum")?,
        local_content_hash: row.get_string("local_content_hash")?,
        remote_checksum: row.get_string("remote_checksum")?,
        foxy_mode: FoxyMode::from_db_str(&row.get_string("foxy_mode")?),
    })
}

/// Indicates the hashing protocol used by a remote repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum FoxyMode {
    /// Legacy Swifty-compatible: MD5 checksums, mod.srf manifests.
    #[default]
    None,
    /// FoxyModeV1: BLAKE3 checksums, foxy_addon.json manifests.
    V1,
}

impl FoxyMode {
    pub fn is_foxy(&self) -> bool {
        !matches!(self, FoxyMode::None)
    }

    pub fn from_db_str(s: &str) -> Self {
        match s.trim() {
            "FoxyModeV1" => FoxyMode::V1,
            "" => FoxyMode::None,
            other => {
                log::warn!("Unknown foxyMode value '{}', treating as None", other);
                FoxyMode::None
            }
        }
    }

    pub fn as_db_str(&self) -> &str {
        match self {
            FoxyMode::None => "",
            FoxyMode::V1 => "FoxyModeV1",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct FoxyRepository {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) remote_url: String,
    pub(crate) local_path: String,
    pub(crate) image: String,
    pub(crate) local_checksum: String,
    pub(crate) local_content_hash: String,
    #[serde(skip)]
    pub(crate) remote_checksum: String,
    #[serde(skip)]
    pub(crate) foxy_mode: FoxyMode,
}

impl HasLocalChecksum for FoxyRepository {
    fn local_checksum(&self) -> &str {
        &self.local_checksum
    }

    fn order(&self) -> i64 {
        0
    }
    fn local_identifier(&self) -> &str {
        &self.local_path
    }
}

pub(crate) fn normalize_repository_local_path_identity(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert_repository_entry(
    context: Arc<FoxyContext>,
    repository_url: &str,
    name: &str,
    image: &str,
    remote_checksum: &str,
    local_checksum: &str,
    local_content_hash: &str,
    local_path: &str,
    foxy_mode: &FoxyMode,
) -> Result<FoxyRepository, DbErr> {
    debug!("Upserting repository entry for {}", repository_url);
    let db = context.db();
    db.execute_retry(
        "upsert repository entry",
        "INSERT INTO repositories \
         (name, remote_url, image, local_path, remote_checksum, local_checksum, local_content_hash, foxy_mode) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (remote_url, local_path) DO UPDATE SET \
         name = excluded.name, image = excluded.image, \
         remote_checksum = excluded.remote_checksum, foxy_mode = excluded.foxy_mode",
        params![
            name,
            repository_url,
            image,
            local_path,
            remote_checksum,
            local_checksum,
            local_content_hash,
            foxy_mode.as_db_str(),
        ],
    )
    .await?;

    let row = db
        .query_one(
            &format!("SELECT {REPOSITORY_COLUMNS} FROM repositories WHERE remote_url = ? AND local_path = ?"),
            params![repository_url, local_path],
        )
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("Failed to find inserted item".to_string()))?;

    repository_from_row(&row)
}

pub async fn load_repository_by_remote_url_and_local_path(
    context: Arc<FoxyContext>,
    remote_url: &str,
    local_path: &str,
) -> Result<FoxyRepository, DbErr> {
    debug!(
        "Loading repository by remote url {} and local path {}",
        remote_url, local_path
    );
    let local_path_key = normalize_repository_local_path_identity(local_path);
    let rows = context
        .db()
        .query_all(
            &format!("SELECT {REPOSITORY_COLUMNS} FROM repositories WHERE remote_url = ?"),
            params![remote_url],
        )
        .await?;

    for row in &rows {
        let repo = repository_from_row(row)?;
        if normalize_repository_local_path_identity(&repo.local_path) == local_path_key {
            return Ok(repo);
        }
    }
    Err(DbErr::RecordNotFound(remote_url.to_string()))
}

pub async fn load_repository_by_remote_url(
    context: Arc<FoxyContext>,
    remote_url: &str,
) -> Result<FoxyRepository, DbErr> {
    debug!("Loading repository by remote url {}", remote_url);
    if let Some(target_local_path) = context.target_local_path.as_deref() {
        return load_repository_by_remote_url_and_local_path(
            context.clone(),
            remote_url,
            target_local_path,
        )
        .await;
    }

    let row = context
        .db()
        .query_one(
            &format!(
                "SELECT {REPOSITORY_COLUMNS} FROM repositories WHERE remote_url = ? ORDER BY id ASC LIMIT 1"
            ),
            params![remote_url],
        )
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(remote_url.to_string()))?;

    repository_from_row(&row)
}

/// Queries foxy_mode for a repository instance using the shared database.
pub async fn is_repository_foxy(remote_url: &str, local_path: &str) -> Option<bool> {
    use crate::core::db::FoxyDb;
    use crate::core::tasks::init_database::init_database;
    let db = FoxyDb::from_handle(init_database().await);
    let rows = match db
        .query_all(
            "SELECT foxy_mode, local_path FROM repositories WHERE remote_url = ? ORDER BY id ASC",
            params![remote_url],
        )
        .await
    {
        Ok(rows) => rows,
        Err(_) => return None,
    };
    let local_path_key = normalize_repository_local_path_identity(local_path);
    let mut fallback_mode: Option<String> = None;
    for row in &rows {
        let Ok(mode) = row.get_string("foxy_mode") else {
            continue;
        };
        if fallback_mode.is_none() {
            fallback_mode = Some(mode.clone());
        }
        if !local_path_key.is_empty()
            && row
                .get_string("local_path")
                .is_ok_and(|path| normalize_repository_local_path_identity(&path) == local_path_key)
        {
            return Some(FoxyMode::from_db_str(&mode).is_foxy());
        }
    }
    fallback_mode.map(|mode| FoxyMode::from_db_str(&mode).is_foxy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foxy_mode_from_db_str_v1() {
        assert_eq!(FoxyMode::from_db_str("FoxyModeV1"), FoxyMode::V1);
    }

    #[test]
    fn foxy_mode_from_db_str_empty_is_none() {
        assert_eq!(FoxyMode::from_db_str(""), FoxyMode::None);
    }

    #[test]
    fn foxy_mode_from_db_str_unknown_falls_back_to_none() {
        assert_eq!(FoxyMode::from_db_str("FoxyModeV99"), FoxyMode::None);
    }

    #[test]
    fn foxy_mode_round_trip() {
        for mode in [FoxyMode::None, FoxyMode::V1] {
            let db_str = mode.as_db_str();
            assert_eq!(FoxyMode::from_db_str(db_str), mode);
        }
    }

    #[test]
    fn foxy_mode_is_foxy() {
        assert!(!FoxyMode::None.is_foxy());
        assert!(FoxyMode::V1.is_foxy());
    }

    #[test]
    fn foxy_mode_from_db_str_whitespace_trimmed() {
        assert_eq!(FoxyMode::from_db_str("  FoxyModeV1  "), FoxyMode::V1);
        assert_eq!(FoxyMode::from_db_str("  "), FoxyMode::None);
    }

    #[test]
    fn foxy_mode_default_is_none() {
        assert_eq!(FoxyMode::default(), FoxyMode::None);
    }

    #[test]
    fn foxy_mode_as_db_str_values() {
        assert_eq!(FoxyMode::None.as_db_str(), "");
        assert_eq!(FoxyMode::V1.as_db_str(), "FoxyModeV1");
    }

    #[test]
    fn foxy_mode_clone_and_debug() {
        let mode = FoxyMode::V1;
        let cloned = mode.clone();
        assert_eq!(mode, cloned);
        let debug = format!("{:?}", mode);
        assert!(debug.contains("V1"));
    }
}
