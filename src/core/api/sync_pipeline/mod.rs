mod backup;
mod hashing;
mod pipeline;
mod remote_recovery;
pub(crate) mod summary;
mod verification;

#[cfg(test)]
pub(crate) use pipeline::recheck_reapplied_addons;
pub use pipeline::spawn_repository_sync;
