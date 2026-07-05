//! Process-wide runtime for background workers.
//!
//! The startup workers (quick scan, eligibility planning, pending-update
//! restore/clear, DB-entry filters) used to build a full tokio runtime each on
//! their own thread. Beyond the thread-pool waste, every extra runtime is
//! another cross-runtime `block_on` caller against the shared Turso handle -
//! the topology `DB_EXCLUSIVE` exists to defend against. One shared runtime
//! keeps them all on the same executor; blocking work still goes through
//! `spawn_blocking`, which has its own pool.

use log::warn;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// The shared multi-thread background runtime. `None` only if the runtime could
/// not be built (never in practice); callers degrade the same way they did when
/// their private `Runtime::new()` failed.
pub(crate) fn background_runtime() -> Option<&'static Runtime> {
    static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("foxy-background")
                .enable_all()
                .build()
                .map_err(|err| {
                    warn!("Failed to build shared background runtime: {}", err);
                    err
                })
                .ok()
        })
        .as_ref()
}
