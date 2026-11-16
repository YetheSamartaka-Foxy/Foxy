//! Shared runtime keeps background callers on one executor for the shared
//! Turso handle; blocking work uses `spawn_blocking`.

use log::warn;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

pub(crate) fn background_runtime() -> Option<&'static Runtime> {
    static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            crate::core::utils::work_priority::work_runtime_builder()
                .worker_threads(2)
                .thread_name("foxy-background")
                .build()
                .map_err(|err| {
                    warn!("Failed to build shared background runtime: {}", err);
                    err
                })
                .ok()
        })
        .as_ref()
}
