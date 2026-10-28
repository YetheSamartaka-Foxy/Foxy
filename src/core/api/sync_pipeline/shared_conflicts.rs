use anyhow::{Context as _, Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::core::db::{DbValue, FoxyDb};
use crate::core::models::context::FoxyContext;
use crate::core::models::repository::{
    load_repository_by_remote_url, normalize_repository_local_path_identity,
    repository_url_match_key,
};
use crate::core::tasks::remote_repository::remote_repository;
use crate::core::utils::content_hash::normalize_path;
use crate::ui::types::HashAlgorithmPreference;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SharedVersionConflicts {
    pub(crate) blocking: Vec<String>,
    pub(crate) advisory: Vec<String>,
}

pub(crate) async fn invalidate_conflicting_siblings(
    context: Arc<FoxyContext>,
    url: &str,
) -> Result<Vec<String>> {
    let current = load_repository_by_remote_url(context.clone(), url).await?;
    let db = context.db();
    let source = verified_source_download_expectations(&db, current.id as i64).await?;
    let source_by_path = source
        .iter()
        .map(|(path, checksum)| (path_key(path), checksum.trim().to_string()))
        .collect::<HashMap<_, _>>();
    if source_by_path.is_empty() {
        return Ok(Vec::new());
    }
    let file_paths = source_by_path.keys().cloned().collect::<Vec<_>>();
    let rows = overlapping_expectations(&db, current.id as i64, &file_paths)
        .await?
        .into_iter()
        .filter(|sibling| {
            let checksum = sibling.checksum.trim();
            !checksum.is_empty()
                && source_by_path
                    .get(&path_key(&sibling.file_path))
                    .is_some_and(|source| !source.eq_ignore_ascii_case(checksum))
        })
        .collect::<Vec<_>>();
    let mut urls = HashSet::new();
    let mut statements = Vec::new();
    for (table, ids) in [
        (
            "files",
            rows.iter().map(|row| row.file_id).collect::<HashSet<_>>(),
        ),
        (
            "addons",
            rows.iter().map(|row| row.addon_id).collect::<HashSet<_>>(),
        ),
        (
            "repositories",
            rows.iter()
                .map(|row| row.repository_id)
                .collect::<HashSet<_>>(),
        ),
    ] {
        let ids = ids.into_iter().collect::<Vec<_>>();
        for chunk in ids.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            statements.push((format!("UPDATE {table} SET local_checksum = '', local_content_hash = '' WHERE id IN ({placeholders})"),
                chunk.iter().copied().map(DbValue::from).collect::<Vec<_>>()));
        }
    }
    db.transaction("invalidate conflicting shared versions", |txn| {
        let statements = statements.clone();
        Box::pin(async move {
            for (sql, params) in statements {
                txn.execute(&sql, params).await?;
            }
            Ok(())
        })
    })
    .await?;
    for row in rows {
        urls.insert(row.repository_url);
    }
    Ok(urls.into_iter().collect())
}

#[derive(Clone, Debug)]
struct FileExpectation {
    file_id: i64,
    addon_id: i64,
    repository_id: i64,
    repository_name: String,
    repository_url: String,
    repository_path: String,
    file_path: String,
    checksum: String,
}

fn path_key(path: &str) -> String {
    normalize_path(path.trim())
}

fn normalized_path_sql(column: &str) -> String {
    let normalized = format!("RTRIM(REPLACE(TRIM({column}), CHAR(92), '/'), '/')");
    if cfg!(windows) {
        format!("LOWER({normalized})")
    } else {
        normalized
    }
}

fn strict_member_keys(members: &[(String, String)]) -> HashSet<(String, String)> {
    members
        .iter()
        .map(|(url, path)| {
            (
                repository_url_match_key(url),
                normalize_repository_local_path_identity(path),
            )
        })
        .collect()
}

async fn source_download_expectations(
    db: &FoxyDb,
    repository_id: i64,
) -> Result<Vec<(String, String)>> {
    let rows = db
        .query_all(
            "SELECT DISTINCT f.local_path, f.remote_checksum \
             FROM download_target_file AS target \
             JOIN files AS f ON f.id = target.file_id \
             JOIN addon_files AS af ON af.file_id = f.id \
             JOIN addons AS a ON a.id = af.addon_id \
             JOIN repository_addons AS ra ON ra.addon_id = a.id \
             WHERE ra.repository_id = ? AND a.enabled = 1 \
               AND TRIM(COALESCE(f.local_path, '')) != '' \
               AND TRIM(COALESCE(f.remote_checksum, '')) != ''",
            crate::core::db::params![repository_id],
        )
        .await
        .context("load current repository update file targets")?;
    rows.iter()
        .map(|row| {
            Ok((
                row.get_string("local_path")?,
                row.get_string("remote_checksum")?,
            ))
        })
        .collect::<std::result::Result<Vec<_>, crate::core::db::DbErr>>()
        .context("read current repository update targets")
}

async fn verified_source_download_expectations(
    db: &FoxyDb,
    repository_id: i64,
) -> Result<Vec<(String, String)>> {
    let rows = db
        .query_all(
            "SELECT DISTINCT f.local_path, f.remote_checksum \
             FROM download_target_file AS target \
             JOIN files AS f ON f.id = target.file_id \
             JOIN addon_files AS af ON af.file_id = f.id \
             JOIN addons AS a ON a.id = af.addon_id \
             JOIN repository_addons AS ra ON ra.addon_id = a.id \
             WHERE ra.repository_id = ? AND a.enabled = 1 \
               AND TRIM(COALESCE(f.local_path, '')) != '' \
               AND TRIM(COALESCE(f.remote_checksum, '')) != '' \
               AND LOWER(TRIM(COALESCE(f.local_checksum, ''))) = LOWER(TRIM(COALESCE(f.remote_checksum, ''))) ",
            crate::core::db::params![repository_id],
        )
        .await
        .context("load verified current repository update file targets")?;
    rows.iter()
        .map(|row| {
            Ok((
                row.get_string("local_path")?,
                row.get_string("remote_checksum")?,
            ))
        })
        .collect::<std::result::Result<Vec<_>, crate::core::db::DbErr>>()
        .context("read verified current repository update targets")
}

async fn overlapping_expectations(
    db: &FoxyDb,
    current_repository_id: i64,
    file_paths: &[String],
) -> Result<Vec<FileExpectation>> {
    let mut keys = file_paths
        .iter()
        .map(|path| path_key(path))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    let mut rows_out = Vec::new();
    for chunk in keys.chunks(400) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let path_expression = normalized_path_sql("f.local_path");
        let sql = format!(
            "SELECT DISTINCT f.id AS file_id, a.id AS addon_id, r.id AS repository_id, r.name AS repository_name, \
                    r.remote_url AS repository_url, r.local_path AS repository_path, \
                    f.local_path AS file_path, f.remote_checksum AS checksum \
             FROM files AS f \
             JOIN addon_files AS af ON af.file_id = f.id \
             JOIN addons AS a ON a.id = af.addon_id \
             JOIN repository_addons AS ra ON ra.addon_id = a.id \
             JOIN repositories AS r ON r.id = ra.repository_id \
             WHERE r.id != ? AND {path_expression} IN ({placeholders}) \
               AND TRIM(COALESCE(f.local_path, '')) != ''"
        );
        let mut params = Vec::with_capacity(chunk.len() + 1);
        params.push(DbValue::from(current_repository_id));
        params.extend(chunk.iter().cloned().map(DbValue::from));
        let rows = db
            .query_all(&sql, params)
            .await
            .context("find other repositories sharing update file paths")?;
        for row in &rows {
            rows_out.push(FileExpectation {
                file_id: row.get_i64("file_id")?,
                addon_id: row.get_i64("addon_id")?,
                repository_id: row.get_i64("repository_id")?,
                repository_name: row.get_string("repository_name")?,
                repository_url: row.get_string("repository_url")?,
                repository_path: row.get_string("repository_path")?,
                file_path: row.get_string("file_path")?,
                checksum: row.get_string("checksum")?,
            });
        }
    }
    Ok(rows_out)
}

fn classify_conflicts(
    source: &[(String, String)],
    siblings: &[FileExpectation],
    current_repository_url: &str,
    current_repository_path: &str,
    strict_members: &HashSet<(String, String)>,
) -> SharedVersionConflicts {
    let source_by_path = source
        .iter()
        .filter(|(_, checksum)| !checksum.trim().is_empty())
        .map(|(path, checksum)| (path_key(path), checksum.trim().to_string()))
        .collect::<HashMap<_, _>>();
    let current_identity = (
        repository_url_match_key(current_repository_url),
        normalize_repository_local_path_identity(current_repository_path),
    );
    let mut blocking = HashSet::new();
    let mut advisory = HashSet::new();
    for sibling in siblings {
        let checksum = sibling.checksum.trim();
        if checksum.is_empty() {
            continue;
        }
        let sibling_identity = (
            repository_url_match_key(&sibling.repository_url),
            normalize_repository_local_path_identity(&sibling.repository_path),
        );
        if sibling_identity == current_identity {
            continue;
        }
        let Some(source_checksum) = source_by_path.get(&path_key(&sibling.file_path)) else {
            continue;
        };
        if source_checksum.eq_ignore_ascii_case(checksum) {
            continue;
        }
        let label = format!("{} (#{})", sibling.repository_name, sibling.repository_id);
        if strict_members.contains(&sibling_identity) {
            blocking.insert(label);
        } else {
            advisory.insert(label);
        }
    }
    let mut result = SharedVersionConflicts {
        blocking: blocking.into_iter().collect(),
        advisory: advisory.into_iter().collect(),
    };
    result.blocking.sort();
    result.advisory.sort();
    result
}

/// Check the requested update's physical file targets against other repository
/// instances. Known overlapping instances are refreshed before the decision.
pub(crate) async fn check_shared_version_conflicts(
    context: Arc<FoxyContext>,
    repo_url: &str,
    strict_members: &[(String, String)],
) -> Result<SharedVersionConflicts> {
    let current = load_repository_by_remote_url(context.clone(), repo_url)
        .await
        .context("load repository for shared-version check")?;
    let db = context.db();
    let source = source_download_expectations(&db, current.id as i64).await?;
    if source.is_empty() {
        return Ok(SharedVersionConflicts::default());
    }
    let file_paths = source
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let before_refresh = overlapping_expectations(&db, current.id as i64, &file_paths).await?;
    let strict = strict_member_keys(strict_members);
    let mut siblings = HashMap::new();
    for sibling in before_refresh {
        let key = (
            repository_url_match_key(&sibling.repository_url),
            normalize_repository_local_path_identity(&sibling.repository_path),
        );
        siblings.entry(key).or_insert(sibling);
    }
    let current_identity = (
        repository_url_match_key(&current.remote_url),
        normalize_repository_local_path_identity(&current.local_path),
    );
    let mut refresh_targets = HashMap::new();
    for sibling in siblings.values() {
        let identity = (
            repository_url_match_key(&sibling.repository_url),
            normalize_repository_local_path_identity(&sibling.repository_path),
        );
        if identity == current_identity {
            continue;
        }
        refresh_targets.entry(identity.clone()).or_insert_with(|| {
            (
                sibling.repository_id,
                sibling.repository_name.clone(),
                sibling.repository_url.clone(),
                sibling.repository_path.clone(),
                strict.contains(&identity),
            )
        });
    }
    for (_, (_, sibling_name, sibling_url, sibling_path, strict_member)) in refresh_targets {
        let mut sibling_context = (*context)
            .clone()
            .with_target_local_path(sibling_path.clone())
            .with_download_target_queueing(false);
        if !strict_member {
            sibling_context = sibling_context.with_repository_space_shared_path(None);
        }
        let sibling_context = Arc::new(sibling_context);
        let refreshed = remote_repository(
            sibling_context,
            &sibling_url,
            Some(&sibling_path),
            None,
            false,
            HashAlgorithmPreference::default(),
        )
        .await;
        if refreshed.is_none() {
            if strict_member {
                return Err(anyhow!(
                    "Could not refresh repository-space member {} before checking shared file versions",
                    sibling_name
                ));
            }
            log::warn!(
                "Could not refresh a manually shared repository while checking file-version conflicts"
            );
        }
    }
    let after_refresh = overlapping_expectations(&db, current.id as i64, &file_paths).await?;
    Ok(classify_conflicts(
        &source,
        &after_refresh,
        &current.remote_url,
        &current.local_path,
        &strict,
    ))
}

#[cfg(test)]
mod tests {
    use super::invalidate_conflicting_siblings;
    use super::{classify_conflicts, overlapping_expectations, source_download_expectations};
    use crate::core::db::{FoxyDb, params};
    use crate::core::models::context::FoxyContext;
    use crate::core::models::repository::normalize_repository_local_path_identity;
    use crate::core::tasks::db_turso::build_test_database;
    use std::collections::HashSet;
    use std::sync::Arc;

    async fn checksum_state(db: &FoxyDb, table: &str, id: i64) -> (String, String, String) {
        let row = db
            .query_one(
                &format!(
                    "SELECT local_checksum, remote_checksum, local_content_hash FROM {table} WHERE id = ?"
                ),
                params![id],
            )
            .await
            .unwrap()
            .unwrap();
        (
            row.get_string("local_checksum").unwrap(),
            row.get_string("remote_checksum").unwrap(),
            row.get_string("local_content_hash").unwrap(),
        )
    }

    #[tokio::test]
    async fn verified_update_invalidates_only_conflicting_shared_instances() {
        let database = build_test_database().await;
        let context = Arc::new(
            FoxyContext::new(database, reqwest::Client::new())
                .with_target_local_path("R:/Mods/MainRepo"),
        );
        let db = context.db();
        db.execute(
            "INSERT INTO repositories \
             (id,name,remote_url,local_path,local_checksum,remote_checksum,local_content_hash) VALUES \
             (1,'MainRepo','http://repo.example.invalid:8080/','R:/Mods/MainRepo','repo-v2','repo-v2','repo-content-v2'), \
             (2,'RepoAlpha','http://alpha.example.invalid:8080/','R:/Mods/RepoAlpha','repo-v1','repo-v1','repo-content-alpha'), \
             (3,'RepoCopy','http://repo.example.invalid:8080/','R:/Mods/RepoCopy','repo-copy-v1','repo-copy-v1','repo-content-copy'), \
             (4,'RepoOther','http://other.example.invalid:8080/','R:/Mods/RepoOther','repo-other','repo-other','repo-content-other')",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons \
             (id,name,remote_path,local_path,enabled,local_checksum,remote_checksum,local_content_hash,required) VALUES \
             (1,'@main','main/@main','R:/Mods/MainRepo/@main',1,'addon-v2','addon-v2','addon-content-v2',1), \
             (2,'@alpha','alpha/@alpha','R:/Mods/RepoAlpha/@alpha',1,'addon-v1','addon-v1','addon-content-alpha',1), \
             (3,'@copy','copy/@copy','R:/Mods/RepoCopy/@copy',1,'addon-copy-v1','addon-copy-v1','addon-content-copy',1), \
             (4,'@other','other/@other','R:/Mods/RepoOther/@other',1,'addon-other','addon-other','addon-content-other',1)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id,addon_id) VALUES (1,1),(2,2),(3,3),(4,4)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files \
             (id,name,remote_path,local_path,local_checksum,remote_checksum,local_content_hash,length) VALUES \
             (10,'data.bin','main/data.bin','R:/Mods/Shared/@mod_01/data.bin','FILE-V2','file-v2','content-v2',1), \
             (11,'empty.bin','main/empty.bin','R:/Mods/Shared/@mod_01/empty.bin','','','',1), \
             (20,'data.bin','alpha/data.bin','R:/Mods/Shared/@mod_01/data.bin','file-v1','file-v1','content-alpha',1), \
             (21,'empty.bin','alpha/empty.bin','R:/Mods/Shared/@mod_01/empty.bin','empty-v1','empty-v1','empty-content',1), \
             (30,'data.bin','copy/data.bin','R:/Mods/RepoCopy/@copy/data.bin','file-copy-v1','file-copy-v1','content-copy',1), \
             (40,'other.bin','other/other.bin','R:/Mods/RepoOther/@other/other.bin','file-other','file-other','content-other',1)",
            params![],
        )
        .await
        .unwrap();
        let alpha_path = if cfg!(windows) {
            r"r:\mods\shared\@mod_01\DATA.bin"
        } else {
            r"R:\Mods\Shared\@mod_01\data.bin"
        };
        db.execute(
            "UPDATE files SET local_path = ? WHERE id = 20",
            params![alpha_path],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id,file_id) VALUES (1,10),(1,11),(2,20),(2,21),(3,30),(4,40)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO download_target_file (file_id,download_remote_url,download_local_path,size) \
             VALUES (10,'http://repo.example.invalid:8080/main/data.bin','R:/Mods/Shared/@mod_01/data.bin',1), \
                    (11,'http://repo.example.invalid:8080/main/empty.bin','R:/Mods/Shared/@mod_01/empty.bin',1)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO pending_updates (repository_url,local_path,diff_json) \
             VALUES ('http://repo.example.invalid:8080/','R:/Mods/MainRepo','{\"keep\":\"source-payload\"}'), \
                    ('http://alpha.example.invalid:8080/','R:/Mods/RepoAlpha','{\"keep\":\"alpha-payload\"}'), \
                    ('http://repo.example.invalid:8080/','R:/Mods/RepoCopy','{\"keep\":\"copy-payload\"}')",
            params![],
        )
        .await
        .unwrap();

        let mut invalidated =
            invalidate_conflicting_siblings(context, "http://repo.example.invalid:8080/")
                .await
                .unwrap();
        invalidated.sort();
        assert_eq!(
            invalidated,
            vec!["http://alpha.example.invalid:8080/".to_string()]
        );

        for (table, source_ids, alpha_ids, copy_ids, other_ids) in [
            ("files", &[10, 11][..], &[20, 21][..], &[30][..], &[40][..]),
            ("addons", &[1][..], &[2][..], &[3][..], &[4][..]),
            ("repositories", &[1][..], &[2][..], &[3][..], &[4][..]),
        ] {
            for id in source_ids {
                let source = checksum_state(&db, table, *id).await;
                assert!(source.0.eq_ignore_ascii_case(&source.1));
                if *id == 11 {
                    assert!(source.0.is_empty());
                    assert!(source.2.is_empty());
                } else {
                    assert!(!source.0.is_empty());
                    assert!(!source.2.is_empty());
                }
            }

            for id in alpha_ids {
                let sibling = checksum_state(&db, table, *id).await;
                if table == "files" && *id == 21 {
                    assert_eq!(sibling.0, sibling.1);
                    assert!(!sibling.0.is_empty());
                    assert!(!sibling.2.is_empty());
                } else {
                    assert_eq!(sibling.0, "");
                    assert!(!sibling.1.is_empty());
                    assert_eq!(sibling.2, "");
                }
            }

            for id in copy_ids.iter().chain(other_ids) {
                let unaffected = checksum_state(&db, table, *id).await;
                assert_eq!(unaffected.0, unaffected.1);
                assert!(!unaffected.0.is_empty());
                assert!(!unaffected.2.is_empty());
            }
        }

        for (url, path, expected) in [
            (
                "http://repo.example.invalid:8080/",
                "R:/Mods/MainRepo",
                r#"{"keep":"source-payload"}"#,
            ),
            (
                "http://alpha.example.invalid:8080/",
                "R:/Mods/RepoAlpha",
                r#"{"keep":"alpha-payload"}"#,
            ),
            (
                "http://repo.example.invalid:8080/",
                "R:/Mods/RepoCopy",
                r#"{"keep":"copy-payload"}"#,
            ),
        ] {
            let payload = db
                .query_one(
                    "SELECT diff_json FROM pending_updates WHERE repository_url = ? AND local_path = ?",
                    params![url, path],
                )
                .await
                .unwrap()
                .unwrap()
                .get_string("diff_json")
                .unwrap();
            assert_eq!(payload, expected);
        }
    }

    #[tokio::test]
    async fn shared_file_conflict_is_classified_without_writing_database_state() {
        let database = build_test_database().await;
        let context = Arc::new(
            FoxyContext::new(database, reqwest::Client::new())
                .with_target_local_path("R:/Mods/RepoAlpha"),
        );
        let db = context.db();
        db.execute(
            "INSERT INTO repositories (id,name,remote_url,local_path) VALUES \
             (1,'RepoAlpha','https://alpha.example.invalid/','R:/Mods/RepoAlpha'), \
             (2,'RepoBeta','https://beta.example.invalid/','R:/Mods/RepoBeta'), \
             (3,'RepoGamma','https://gamma.example.invalid/','R:/Mods/RepoGamma')",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons (id,name,local_path,enabled,required) VALUES \
             (1,'@shared','R:/Mods/Shared/@shared',1,1), \
             (2,'@shared','R:/Mods/Shared/@shared',1,1), \
             (3,'@separate','R:/Mods/Gamma/@separate',1,1)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id,addon_id) VALUES (1,1),(2,2),(3,3)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files (id,name,local_path,remote_checksum) VALUES \
             (10,'shared.bin','R:/Mods/Shared/@shared/shared.bin','VERSION_A'), \
             (20,'shared.bin','R:/Mods/Shared/@shared/shared.bin','VERSION_B'), \
             (30,'separate.bin','R:/Mods/Gamma/@separate/separate.bin','VERSION_C')",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id,file_id) VALUES (1,10),(2,20),(3,30)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO download_target_file (file_id,download_remote_url,download_local_path,size) \
             VALUES (10,'https://alpha.example.invalid/shared.bin','R:/Mods/Shared/@shared/shared.bin',1)",
            params![],
        )
        .await
        .unwrap();

        let source = source_download_expectations(&db, 1).await.unwrap();
        assert_eq!(source.len(), 1);
        let siblings =
            overlapping_expectations(&db, 1, &["R:/Mods/Shared/@shared/shared.bin".to_string()])
                .await
                .unwrap();
        let strict = HashSet::from([(
            "https://beta.example.invalid/".to_string(),
            normalize_repository_local_path_identity("R:/Mods/RepoBeta"),
        )]);
        let conflicts = classify_conflicts(
            &source,
            &siblings,
            "https://alpha.example.invalid/",
            "R:/Mods/RepoAlpha",
            &strict,
        );
        assert_eq!(conflicts.blocking, vec!["RepoBeta (#2)"]);
        assert!(conflicts.advisory.is_empty());

        let advisory = classify_conflicts(
            &source,
            &siblings,
            "https://alpha.example.invalid/",
            "R:/Mods/RepoAlpha",
            &HashSet::new(),
        );
        assert!(advisory.blocking.is_empty());
        assert_eq!(advisory.advisory, vec!["RepoBeta (#2)"]);

        let mut same_version = siblings.clone();
        same_version[0].checksum = source[0].1.clone();
        assert_eq!(
            classify_conflicts(
                &source,
                &same_version,
                "https://alpha.example.invalid/",
                "R:/Mods/RepoAlpha",
                &strict,
            ),
            Default::default()
        );

        let separate = overlapping_expectations(
            &db,
            1,
            &["R:/Mods/Gamma/@separate/separate.bin".to_string()],
        )
        .await
        .unwrap();
        let separate_conflicts = classify_conflicts(
            &source,
            &separate,
            "https://alpha.example.invalid/",
            "R:/Mods/RepoAlpha",
            &strict,
        );
        assert_eq!(separate_conflicts, Default::default());
        let unchanged = db
            .query_one("SELECT remote_checksum FROM files WHERE id = 20", params![])
            .await
            .unwrap()
            .unwrap()
            .get_string("remote_checksum")
            .unwrap();
        assert_eq!(unchanged, "VERSION_B");
    }
}
