mod background_tasks;
mod database_repair;
pub(crate) mod download_retry;
pub(crate) mod frame_cost;
mod game_space_switch;
mod hash_evidence;
mod keyboard_focus;
mod launch_actions;
mod maintenance;
mod progress_events;
pub(crate) mod repository_status;
mod shutdown;
mod startup_prompts;
mod startup_sync;
mod storage_notice;
pub(crate) mod update_loop;

pub(crate) use database_repair::DatabaseLaunchContinuation;
pub(crate) use launch_actions::{LaunchActionState, LaunchRequest};
pub(crate) use startup_sync::StartupSyncTracker;
pub(crate) use storage_notice::{
    StorageCompatNotice, storage_notice_fingerprint, storage_notice_rows,
};
