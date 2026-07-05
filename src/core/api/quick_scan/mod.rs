mod content_hash;
mod db_helpers;
mod diff;
mod diff_addon_hash;
mod diff_file_resolution;
mod file_state;
mod local_path_preflight;
mod pending_updates;
mod persistent_cache;
mod readiness;
mod shared_cache;
mod unexpected_files;
mod worker;

// Public API (re-exported by api/mod.rs)
pub use worker::{
    StartupRepositoryInstance, filter_repo_instances_with_db_entry, plan_startup_quick_scan_repos,
    recalculate_hashes_for_addon_by_name, spawn_quick_local_scan, spawn_quick_local_scan_instances,
};

// Crate-visible items
pub(crate) use content_hash::{
    refresh_content_hashes_for_scoped_tree, refresh_content_hashes_for_tree,
    refresh_content_hashes_when_tree_matches,
};
pub(crate) use diff::quick_local_change_diff;
pub(crate) use local_path_preflight::{
    format_local_path_mismatch_message, log_addon_path_disk_state, log_local_path_availability,
    summarize_local_path_availability, suspect_local_path_mismatch,
};

// Items used by sync_pipeline.rs
pub(super) use pending_updates::{
    apply_download_target_estimates_to_pending_updates,
    apply_patch_plan_estimates_to_pending_updates, collect_repo_download_targets,
    pending_update_mod_scope, persist_pending_updates,
    refresh_patch_plan_metadata_for_pending_updates,
};
pub(super) use readiness::{
    collect_files_with_missing_local_tree_hashes, tree_local_checksums_baseline_missing,
    tree_local_checksums_missing,
};
// Re-exported for integration tests (api/tests.rs) only; not referenced by the binary.
#[cfg(test)]
pub(super) use db_helpers::{
    content_hash_baseline_ready_joined, remote_checksum_state_ready_joined,
};
#[cfg(test)]
pub(super) use readiness::{
    StartupQuickScanEligibility, batch_eligible_repos, launch_quick_scan_repo_eligible_joined,
    launch_quick_scan_repo_startup_eligibility,
};
pub(super) use unexpected_files::{
    collect_unexpected_files_for_repo_mods, delete_unexpected_local_files,
};
