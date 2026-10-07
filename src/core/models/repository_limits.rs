use log::debug;

use crate::core::db::{DbErr, FoxyDb, params};
use crate::core::models::repository::normalize_repository_local_path_identity;
use crate::core::tasks::init_database::init_database;

/// Size and path extremes of one repository instance's manifest, for checking
/// the destination filesystem without loading the tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryPathLimits {
    pub file_count: u64,
    pub largest_file_bytes: u64,
    /// Files strictly larger than the `size_limit` passed to the query.
    pub files_over_size_limit: u64,
    /// Longest resolved local path in characters.
    pub longest_path_chars: u64,
    /// Files whose resolved local path has `path_chars_limit` characters or
    /// more (the caller folds the sidecar reserve into that limit).
    pub paths_at_or_over_limit: u64,
}

/// One aggregate pass over the instance's file rows in the shared database.
/// Returns `Ok(None)` when the `(remote_url, local_path)` instance has no row.
pub async fn repository_path_limits(
    remote_url: &str,
    local_path: &str,
    size_limit: u64,
    path_chars_limit: u64,
) -> Result<Option<RepositoryPathLimits>, DbErr> {
    let db = FoxyDb::from_handle(init_database().await);
    repository_path_limits_in(&db, remote_url, local_path, size_limit, path_chars_limit).await
}

pub(crate) async fn repository_path_limits_in(
    db: &FoxyDb,
    remote_url: &str,
    local_path: &str,
    size_limit: u64,
    path_chars_limit: u64,
) -> Result<Option<RepositoryPathLimits>, DbErr> {
    let local_path_key = normalize_repository_local_path_identity(local_path);
    let rows = db
        .query_all(
            "SELECT id, local_path FROM repositories WHERE remote_url = ? ORDER BY id ASC",
            params![remote_url],
        )
        .await?;
    let mut repository_id = None;
    for row in &rows {
        if normalize_repository_local_path_identity(&row.get_string("local_path")?)
            == local_path_key
        {
            repository_id = Some(row.get_i64("id")?);
            break;
        }
    }
    let Some(repository_id) = repository_id else {
        return Ok(None);
    };

    let row = db
        .query_one(
            "SELECT COUNT(*) AS file_count, \
             COALESCE(MAX(f.length), 0) AS largest_file_bytes, \
             COALESCE(SUM(CASE WHEN f.length > ? THEN 1 ELSE 0 END), 0) AS files_over_size_limit, \
             COALESCE(MAX(LENGTH(f.local_path)), 0) AS longest_path_chars, \
             COALESCE(SUM(CASE WHEN LENGTH(f.local_path) >= ? THEN 1 ELSE 0 END), 0) AS paths_at_or_over_limit \
             FROM repository_addons ra \
             JOIN addon_files af ON af.addon_id = ra.addon_id \
             JOIN files f ON f.id = af.file_id \
             WHERE ra.repository_id = ?",
            params![size_limit as i64, path_chars_limit as i64, repository_id],
        )
        .await?;
    let Some(row) = row else {
        return Ok(Some(RepositoryPathLimits::default()));
    };
    let limits = RepositoryPathLimits {
        file_count: row.get_i64("file_count")?.max(0) as u64,
        largest_file_bytes: row.get_i64("largest_file_bytes")?.max(0) as u64,
        files_over_size_limit: row.get_i64("files_over_size_limit")?.max(0) as u64,
        longest_path_chars: row.get_i64("longest_path_chars")?.max(0) as u64,
        paths_at_or_over_limit: row.get_i64("paths_at_or_over_limit")?.max(0) as u64,
    };
    debug!(
        "Repository path limits for {}: files={} largest={} over_size_limit={} longest_path={} at_or_over_path_limit={}",
        remote_url,
        limits.file_count,
        limits.largest_file_bytes,
        limits.files_over_size_limit,
        limits.longest_path_chars,
        limits.paths_at_or_over_limit
    );
    Ok(Some(limits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::db_turso::build_test_database;

    const LONG_FILE: &str = "X:/mods/@a/addons/this_name_is_long_enough_to_cross_the_limit.pbo";

    async fn seeded_db() -> FoxyDb {
        let db = FoxyDb::from_turso(build_test_database().await);
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES \
             (1, 'repo', 'http://r/', 'X:/mods'), (2, 'twin', 'http://r/', 'Y:/mods')",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, required) VALUES \
             (10, '@a', 'a', 'X:/mods/@a', 1), (20, '@b', 'b', 'Y:/mods/@b', 1)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            &format!(
                "INSERT INTO files (id, name, remote_path, local_path, length) VALUES \
                 (100, 'small.pbo', 'a/small.pbo', 'X:/mods/@a/addons/small.pbo', 10), \
                 (101, 'big.pbo', 'a/big.pbo', 'X:/mods/@a/addons/big.pbo', 5000000000), \
                 (102, 'long.pbo', 'a/long.pbo', '{LONG_FILE}', 20), \
                 (200, 'other.pbo', 'b/other.pbo', 'Y:/mods/@b/addons/other.pbo', 7000000000)"
            ),
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 10), (2, 20)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (10, 100), (10, 101), (10, 102), (20, 200)",
            Vec::new(),
        )
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn aggregates_are_scoped_to_the_repository_instance() {
        let db = seeded_db().await;
        let limits = repository_path_limits_in(&db, "http://r/", "X:/mods", 4_294_967_295, 50)
            .await
            .unwrap()
            .expect("instance row");
        assert_eq!(limits.file_count, 3);
        assert_eq!(limits.largest_file_bytes, 5_000_000_000);
        assert_eq!(limits.files_over_size_limit, 1);
        assert_eq!(limits.longest_path_chars, LONG_FILE.len() as u64);
        assert_eq!(limits.paths_at_or_over_limit, 1);

        // The same URL in another folder is a different instance; the folder
        // matches through the identity normalization, not byte equality.
        let twin = repository_path_limits_in(&db, "http://r/", "Y:/mods/", 4_294_967_295, 50)
            .await
            .unwrap()
            .expect("twin row");
        assert_eq!(twin.file_count, 1);
        assert_eq!(twin.largest_file_bytes, 7_000_000_000);
    }

    #[tokio::test]
    async fn unknown_instance_yields_none_and_empty_instance_yields_zeroes() {
        let db = seeded_db().await;
        assert_eq!(
            repository_path_limits_in(&db, "http://r/", "Z:/nowhere", 1, 1)
                .await
                .unwrap(),
            None
        );
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (3, 'empty', 'http://e/', 'X:/e')",
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            repository_path_limits_in(&db, "http://e/", "X:/e", 1, 1)
                .await
                .unwrap(),
            Some(RepositoryPathLimits::default())
        );
    }
}
