use std::sync::Arc;

use crate::core::models::context::FoxyContext;
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::tasks::create_web_client::create_web_client;
use crate::core::tasks::init_database::init_database;
use log::debug;

static BACKGROUND_CONTEXT: tokio::sync::Mutex<Option<Arc<FoxyContext>>> =
    tokio::sync::Mutex::const_new(None);

pub(crate) fn invalidate_shared_background_context(
    database: &crate::core::db::DbHandle,
) -> Result<(), String> {
    let mut slot = BACKGROUND_CONTEXT.try_lock().map_err(|_| {
        "Background database context is still being prepared; retry when it finishes".to_string()
    })?;
    invalidate_cached_context(&mut slot, database);
    Ok(())
}

fn invalidate_cached_context(
    slot: &mut Option<Arc<FoxyContext>>,
    database: &crate::core::db::DbHandle,
) {
    if slot
        .as_ref()
        .is_some_and(|context| Arc::ptr_eq(&context.database, database))
    {
        slot.take();
    }
}

pub(crate) async fn create_context() -> Arc<FoxyContext> {
    debug!("Creating core context");
    let database = init_database().await;
    let client = create_web_client().await;
    debug!("Core context created");
    Arc::new(FoxyContext::new(database, client))
}

pub(crate) async fn shared_background_context() -> Arc<FoxyContext> {
    let database = init_database().await;
    let mut slot = BACKGROUND_CONTEXT.lock().await;
    if let Some(context) = slot.as_ref()
        && Arc::ptr_eq(&context.database, &database)
    {
        return context.clone();
    }
    let client = create_web_client().await;
    let context = Arc::new(FoxyContext::new(database, client));
    *slot = Some(context.clone());
    context
}

pub(crate) async fn create_context_with_recheck_level(
    recheck_level: RecheckLevel,
) -> Arc<FoxyContext> {
    debug!(
        "Creating core context with recheck level {:?}",
        recheck_level
    );
    let database = init_database().await;
    let client = create_web_client().await;
    let mut context = FoxyContext::new(database, client);
    context.recheck_level = recheck_level;
    debug!(
        "Core context created with recheck level {:?}",
        recheck_level
    );
    Arc::new(context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalidating_the_cache_releases_only_its_own_reference() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("database.db");
        let database = Arc::new(
            crate::core::tasks::db_turso::build_and_bootstrap(path.to_str().unwrap())
                .await
                .unwrap(),
        );
        let running = Arc::new(FoxyContext::new(database.clone(), reqwest::Client::new()));
        let mut cache = Some(running.clone());
        invalidate_cached_context(&mut cache, &database);
        assert!(cache.is_none());
        assert_eq!(Arc::strong_count(&running), 1);
        assert!(Arc::strong_count(&database) > 1);
        drop(running);
        assert_eq!(Arc::strong_count(&database), 1);
    }

    #[tokio::test]
    async fn invalidating_another_database_preserves_the_cached_context() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.db");
        let second = root.path().join("second.db");
        let database = Arc::new(
            crate::core::tasks::db_turso::build_and_bootstrap(first.to_str().unwrap())
                .await
                .unwrap(),
        );
        let other = Arc::new(
            crate::core::tasks::db_turso::build_and_bootstrap(second.to_str().unwrap())
                .await
                .unwrap(),
        );
        let mut cache = Some(Arc::new(FoxyContext::new(
            database.clone(),
            reqwest::Client::new(),
        )));
        invalidate_cached_context(&mut cache, &other);
        assert!(Arc::ptr_eq(&cache.as_ref().unwrap().database, &database));
    }
}
