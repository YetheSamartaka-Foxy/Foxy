use crate::core::db::{DbErr, FoxyDb, params};
use crate::core::models::context::FoxyContext;
use crate::core::tasks::init_database::init_database;
use chrono::Utc;
use log::debug;
use std::sync::Arc;

fn now_ts() -> i64 {
    Utc::now().timestamp()
}

/// Canonicalize a download folder so the pending-update key matches regardless of
/// separator style, trailing slash, or (on Windows) case. The UI looks the row up
/// by `repository.path`; the core writes it from `context.target_local_path`.
/// Both go through this function so the two sides agree.
fn normalize_pending_update_local_path(local_path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(local_path)
}

/// The local-path identity a pending update is stored under for a given context.
/// An unset target folder yields an empty key, matching single-instance repos
/// that have not been scoped to a specific folder.
fn context_pending_update_local_path(context: &FoxyContext) -> String {
    context
        .target_local_path
        .as_deref()
        .map(normalize_pending_update_local_path)
        .unwrap_or_default()
}

async fn upsert_payload(
    db: &FoxyDb,
    repository_url: &str,
    local_path: &str,
    payload: &str,
) -> Result<(), DbErr> {
    db.execute_retry(
        "upsert pending update",
        "INSERT INTO pending_updates (repository_url, local_path, diff_json, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT (repository_url, local_path) DO UPDATE SET \
         diff_json = excluded.diff_json, updated_at = excluded.updated_at",
        params![repository_url, local_path, payload, now_ts()],
    )
    .await?;
    Ok(())
}

async fn delete_payload(db: &FoxyDb, repository_url: &str, local_path: &str) -> Result<(), DbErr> {
    db.execute_retry(
        "delete pending update",
        "DELETE FROM pending_updates WHERE repository_url = ? AND local_path = ?",
        params![repository_url, local_path],
    )
    .await?;
    Ok(())
}

async fn fetch_payload(
    db: &FoxyDb,
    repository_url: &str,
    local_path: &str,
) -> Result<Option<String>, DbErr> {
    let row = db
        .query_one(
            "SELECT diff_json FROM pending_updates WHERE repository_url = ? AND local_path = ?",
            params![repository_url, local_path],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_string("diff_json")?)),
        None => Ok(None),
    }
}

pub(crate) async fn save_pending_update_for_context(
    context: Arc<FoxyContext>,
    repository_url: &str,
    payload: &str,
) -> Result<(), DbErr> {
    let local_path = context_pending_update_local_path(&context);
    debug!(
        "Saving pending update payload for repo {} (local_path={:?}, {} bytes)",
        repository_url,
        local_path,
        payload.len()
    );
    upsert_payload(&context.db(), repository_url, &local_path, payload).await
}

pub(crate) async fn clear_pending_update_for_context(
    context: Arc<FoxyContext>,
    repository_url: &str,
) -> Result<(), DbErr> {
    let local_path = context_pending_update_local_path(&context);
    debug!(
        "Clearing pending update payload for repo {} (local_path={:?})",
        repository_url, local_path
    );
    delete_payload(&context.db(), repository_url, &local_path).await
}

/// Load the cached pending-update payload for the repository instance scoped by
/// `context.target_local_path`, matching how [`save_pending_update_for_context`]
/// writes it.
pub(crate) async fn fetch_pending_update_for_context(
    context: Arc<FoxyContext>,
    repository_url: &str,
) -> Result<Option<String>, DbErr> {
    let local_path = context_pending_update_local_path(&context);
    fetch_payload(&context.db(), repository_url, &local_path).await
}

/// Load the cached pending-update payload for a specific repository instance.
pub async fn load_pending_update_payload_for_path(
    repository_url: &str,
    local_path: &str,
) -> Result<Option<String>, DbErr> {
    let local_path = normalize_pending_update_local_path(local_path);
    debug!(
        "Loading pending update payload for repo {} (local_path={:?})",
        repository_url, local_path
    );
    let db = FoxyDb::from_handle(init_database().await);
    fetch_payload(&db, repository_url, &local_path).await
}

/// Clear the cached pending-update payload for a specific repository instance.
pub async fn clear_pending_update_payload_for_path(
    repository_url: &str,
    local_path: &str,
) -> Result<(), DbErr> {
    let local_path = normalize_pending_update_local_path(local_path);
    debug!(
        "Clearing pending update payload via standalone helper for repo {} (local_path={:?})",
        repository_url, local_path
    );
    let db = FoxyDb::from_handle(init_database().await);
    delete_payload(&db, repository_url, &local_path).await
}
