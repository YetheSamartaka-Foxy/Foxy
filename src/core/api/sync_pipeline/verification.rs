use crate::core::db::DbValue;
use crate::core::models::context::FoxyContext;
use crate::core::models::repository::load_repository_by_remote_url;
use crate::core::tasks::init_database::read_chunk_ids;
use anyhow::{Context, bail};
use std::collections::HashSet;
use std::sync::Arc;

pub(super) async fn verify_downloaded_files(
    context: Arc<FoxyContext>,
    repository_url: &str,
    file_ids: &HashSet<u64>,
) -> anyhow::Result<()> {
    if file_ids.is_empty() {
        return Ok(());
    }

    let repository = load_repository_by_remote_url(context.clone(), repository_url)
        .await
        .context("failed to locate repository for downloaded-file verification")?;
    let repository_id = i64::try_from(repository.id)
        .context("repository id is outside the supported database range")?;
    let mut requested_ids = file_ids.iter().copied().collect::<Vec<_>>();
    requested_ids.sort_unstable();

    let mut verified_ids = HashSet::with_capacity(requested_ids.len());
    let mut mismatch_count = 0usize;
    for chunk in requested_ids.chunks(read_chunk_ids()) {
        let ids = chunk
            .iter()
            .filter_map(|id| i64::try_from(*id).ok())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }

        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT DISTINCT f.id, f.local_checksum, f.remote_checksum \
             FROM repository_addons ra \
             JOIN addon_files af ON af.addon_id = ra.addon_id \
             JOIN files f ON f.id = af.file_id \
             WHERE ra.repository_id = ? AND f.id IN ({placeholders})"
        );
        let mut query_params = Vec::with_capacity(ids.len() + 1);
        query_params.push(DbValue::Int(repository_id));
        query_params.extend(ids.into_iter().map(DbValue::Int));
        let rows = context
            .db()
            .query_all(&sql, query_params)
            .await
            .context("failed to read downloaded-file checksums")?;

        for row in rows {
            let id = row
                .get_i64("id")
                .context("downloaded-file verification returned an invalid file id")?;
            let Ok(id) = u64::try_from(id) else {
                continue;
            };
            verified_ids.insert(id);
            let local = row
                .get_opt_string("local_checksum")
                .context("downloaded-file verification returned an invalid local checksum")?
                .unwrap_or_default();
            let remote = row
                .get_opt_string("remote_checksum")
                .context("downloaded-file verification returned an invalid remote checksum")?
                .unwrap_or_default();
            if local.trim().is_empty()
                || remote.trim().is_empty()
                || !local.trim().eq_ignore_ascii_case(remote.trim())
            {
                mismatch_count += 1;
            }
        }
    }

    let absent_count = requested_ids
        .iter()
        .filter(|id| !verified_ids.contains(id))
        .count();
    if absent_count > 0 || mismatch_count > 0 {
        bail!(
            "Downloaded-file verification failed: {absent_count} requested file(s) are missing from this repository and {mismatch_count} file(s) have empty or mismatched checksums. Refresh repository metadata and retry the update."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::params;
    use crate::core::models::context::FoxyContext;

    async fn test_context() -> Arc<FoxyContext> {
        Arc::new(FoxyContext::new(
            crate::core::tasks::db_turso::build_test_database().await,
            reqwest::Client::new(),
        ))
    }

    async fn add_repository(context: &FoxyContext, id: i64, url: &str, path: &str) {
        context
            .db()
            .execute(
                "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (?, ?, ?, ?)",
                params![id, format!("Repo{id}"), url, path],
            )
            .await
            .unwrap();
    }

    async fn add_file(
        context: &FoxyContext,
        repository_id: i64,
        addon_id: i64,
        file_id: i64,
        local: &str,
        remote: &str,
    ) {
        context
            .db()
            .execute(
                "INSERT INTO addons (id, name, enabled, required) VALUES (?, ?, 1, 1)",
                params![addon_id, format!("@mod_{addon_id:02}")],
            )
            .await
            .unwrap();
        context
            .db()
            .execute(
                "INSERT INTO repository_addons (repository_id, addon_id) VALUES (?, ?)",
                params![repository_id, addon_id],
            )
            .await
            .unwrap();
        context
            .db()
            .execute(
                "INSERT INTO files (id, name, local_checksum, remote_checksum) VALUES (?, ?, ?, ?)",
                params![file_id, format!("file_{file_id}.pbo"), local, remote],
            )
            .await
            .unwrap();
        context
            .db()
            .execute(
                "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
                params![addon_id, file_id],
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn accepts_matching_checksums_case_insensitively() {
        let context = test_context().await;
        add_repository(
            &context,
            1,
            "https://repo.example.invalid/",
            "R:/Mods/RepoAlpha",
        )
        .await;
        add_file(&context, 1, 1, 11, "AABB", "aabb").await;

        verify_downloaded_files(
            context,
            "https://repo.example.invalid/",
            &HashSet::from([11]),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn rejects_mismatched_empty_and_absent_files_without_paths() {
        let context = test_context().await;
        add_repository(
            &context,
            1,
            "https://repo.example.invalid/",
            "R:/Mods/RepoAlpha",
        )
        .await;
        add_file(&context, 1, 1, 11, "actual", "expected").await;
        add_file(&context, 1, 2, 12, "", "expected").await;

        let error = verify_downloaded_files(
            context,
            "https://repo.example.invalid/",
            &HashSet::from([11, 12, 13]),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("1 requested file(s) are missing"));
        assert!(error.contains("2 file(s) have empty or mismatched checksums"));
        assert!(error.contains("Refresh repository metadata"));
        assert!(!error.contains("RepoAlpha"));
    }

    #[tokio::test]
    async fn scopes_files_to_selected_repository_instance_and_requested_ids() {
        let context = Arc::new(
            FoxyContext::new(
                crate::core::tasks::db_turso::build_test_database().await,
                reqwest::Client::new(),
            )
            .with_target_local_path("R:/Mods/RepoBeta"),
        );
        let url = "https://repo.example.invalid/";
        add_repository(&context, 1, url, "R:/Mods/RepoAlpha").await;
        add_repository(&context, 2, url, "R:/Mods/RepoBeta").await;
        add_file(&context, 1, 1, 11, "wrong", "checksum").await;
        add_file(&context, 2, 2, 12, "same", "same").await;
        add_file(&context, 2, 3, 13, "wrong", "unrelated").await;

        verify_downloaded_files(context, url, &HashSet::from([12]))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn empty_scope_succeeds_without_repository_lookup() {
        verify_downloaded_files(
            Arc::new(FoxyContext::new(
                crate::core::tasks::db_turso::build_test_database().await,
                reqwest::Client::new(),
            )),
            "https://repo.example.invalid/",
            &HashSet::new(),
        )
        .await
        .unwrap();
    }
}
