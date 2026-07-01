use std::sync::Arc;

use crate::core::models::context::FoxyContext;
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::tasks::create_web_client::create_web_client;
use crate::core::tasks::init_database::init_database;
use log::debug;

/// Create FoxyContext with shared data: available database connection and reqwest client
pub(crate) async fn create_context() -> Arc<FoxyContext> {
    debug!("Creating core context");
    let database = init_database().await;
    let client = create_web_client().await;
    debug!("Core context created");
    Arc::new(FoxyContext::new(database, client))
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
