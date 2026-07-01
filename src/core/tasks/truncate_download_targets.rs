use crate::core::db::{DbErr, params};
use crate::core::models::context::FoxyContext;
use log::debug;
use std::sync::Arc;

pub async fn truncate_all_download_tables(context: Arc<FoxyContext>) -> Result<(), DbErr> {
    debug!("Truncating download target tables");

    context
        .db()
        .transaction("truncate download tables", |txn| {
            Box::pin(async move {
                // Order: children first, then parents (respects FK dependencies)
                txn.execute("DELETE FROM download_patch_op", params![])
                    .await?;
                txn.execute("DELETE FROM download_patch_file", params![])
                    .await?;
                txn.execute("DELETE FROM download_target_file_part", params![])
                    .await?;
                txn.execute("DELETE FROM download_target_file", params![])
                    .await?;
                Ok(())
            })
        })
        .await?;

    debug!("Download target tables truncated");
    Ok(())
}
