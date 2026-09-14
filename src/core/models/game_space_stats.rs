use crate::core::db::{DbErr, DbValue, FoxyDb, params};
use crate::core::models::repository::normalize_repository_local_path_identity;
use crate::core::tasks::init_database::init_database;

/// Aggregate over a set of repository instances. Addons and files are counted
/// once per local path, so repositories that share a download folder (a
/// repository space) do not double count the mods they have in common.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddonGroupStats {
    /// Instances from the requested set that have a database row.
    pub matched_repositories: u64,
    pub unique_addons: u64,
    pub total_bytes: u64,
}

/// Aggregate stats for the `(remote_url, local_path)` instances in `members`.
pub async fn addon_group_stats(members: &[(String, String)]) -> Result<AddonGroupStats, DbErr> {
    let db = FoxyDb::from_handle(init_database().await);
    addon_group_stats_in(&db, members).await
}

pub(crate) async fn addon_group_stats_in(
    db: &FoxyDb,
    members: &[(String, String)],
) -> Result<AddonGroupStats, DbErr> {
    if members.is_empty() {
        return Ok(AddonGroupStats::default());
    }
    let wanted: Vec<(&str, String)> = members
        .iter()
        .map(|(url, path)| (url.as_str(), normalize_repository_local_path_identity(path)))
        .collect();
    let rows = db
        .query_all(
            "SELECT id, remote_url, local_path FROM repositories ORDER BY id ASC",
            params![],
        )
        .await?;
    let mut ids: Vec<i64> = Vec::new();
    for row in &rows {
        let url = row.get_string("remote_url")?;
        let path_key = normalize_repository_local_path_identity(&row.get_string("local_path")?);
        if wanted
            .iter()
            .any(|(wanted_url, wanted_path)| *wanted_url == url && *wanted_path == path_key)
        {
            ids.push(row.get_i64("id")?);
        }
    }
    if ids.is_empty() {
        return Ok(AddonGroupStats::default());
    }

    let placeholders = vec!["?"; ids.len()].join(", ");
    let id_params: Vec<DbValue> = ids.iter().map(|id| DbValue::from(*id)).collect();
    let addons_row = db
        .query_one(
            &format!(
                "SELECT COUNT(DISTINCT LOWER(a.local_path)) AS unique_addons \
                 FROM repository_addons ra \
                 JOIN addons a ON a.id = ra.addon_id \
                 WHERE ra.repository_id IN ({placeholders})"
            ),
            id_params.clone(),
        )
        .await?;
    let bytes_row = db
        .query_one(
            &format!(
                "SELECT COALESCE(SUM(length), 0) AS total_bytes FROM ( \
                 SELECT MAX(f.length) AS length \
                 FROM repository_addons ra \
                 JOIN addon_files af ON af.addon_id = ra.addon_id \
                 JOIN files f ON f.id = af.file_id \
                 WHERE ra.repository_id IN ({placeholders}) \
                 GROUP BY LOWER(f.local_path))"
            ),
            id_params,
        )
        .await?;
    Ok(AddonGroupStats {
        matched_repositories: ids.len() as u64,
        unique_addons: addons_row
            .map(|row| row.get_i64("unique_addons"))
            .transpose()?
            .unwrap_or(0)
            .max(0) as u64,
        total_bytes: bytes_row
            .map(|row| row.get_i64("total_bytes"))
            .transpose()?
            .unwrap_or(0)
            .max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::db_turso::build_test_database;

    /// Two repositories in one shared folder ("space") that both carry `@a`,
    /// plus a standalone repository with its own `@b`.
    async fn seeded_db() -> FoxyDb {
        let db = FoxyDb::from_turso(build_test_database().await);
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES \
             (1, 'one', 'http://one/', 'X:/space'), (2, 'two', 'http://two/', 'X:/space'), \
             (3, 'solo', 'http://solo/', 'Y:/solo')",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, required) VALUES \
             (10, '@a', 'one/a', 'X:/space/@a', 1), (11, '@a', 'two/a', 'X:/space/@a', 1), \
             (12, '@c', 'two/c', 'X:/space/@c', 1), (20, '@b', 'b', 'Y:/solo/@b', 1)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path, length) VALUES \
             (100, 'a.pbo', 'one/a/a.pbo', 'X:/space/@a/a.pbo', 100), \
             (101, 'a.pbo', 'two/a/a.pbo', 'X:/space/@a/a.pbo', 100), \
             (102, 'c.pbo', 'two/c/c.pbo', 'X:/space/@c/c.pbo', 30), \
             (200, 'b.pbo', 'b/b.pbo', 'Y:/solo/@b/b.pbo', 7)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES \
             (1, 10), (2, 11), (2, 12), (3, 20)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES \
             (10, 100), (11, 101), (12, 102), (20, 200)",
            Vec::new(),
        )
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn shared_folder_addons_and_files_count_once() {
        let db = seeded_db().await;
        let space = addon_group_stats_in(
            &db,
            &[
                ("http://one/".to_string(), "X:/space".to_string()),
                ("http://two/".to_string(), "X:/space".to_string()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            space,
            AddonGroupStats {
                matched_repositories: 2,
                unique_addons: 2,
                total_bytes: 130,
            }
        );
    }

    #[tokio::test]
    async fn totals_span_every_instance() {
        let db = seeded_db().await;
        let all = addon_group_stats_in(
            &db,
            &[
                ("http://one/".to_string(), "X:/space".to_string()),
                ("http://two/".to_string(), "X:/space".to_string()),
                ("http://solo/".to_string(), "Y:/solo".to_string()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(all.matched_repositories, 3);
        assert_eq!(all.unique_addons, 3);
        assert_eq!(all.total_bytes, 137);
    }

    #[tokio::test]
    async fn unknown_instances_and_empty_input_are_zero() {
        let db = seeded_db().await;
        let missing = addon_group_stats_in(
            &db,
            &[("http://one/".to_string(), "Z:/elsewhere".to_string())],
        )
        .await
        .unwrap();
        assert_eq!(missing, AddonGroupStats::default());
        let empty = addon_group_stats_in(&db, &[]).await.unwrap();
        assert_eq!(empty, AddonGroupStats::default());
    }
}
