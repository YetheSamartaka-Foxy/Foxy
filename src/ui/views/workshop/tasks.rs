use std::path::PathBuf;
use std::sync::mpsc::Sender;

use log::{info, warn};

use crate::core::game::workshop::bundle::{self, BundleExportOptions};
use crate::core::game::workshop::pin;
use crate::core::game::workshop::share::SharedItem;
use crate::core::game::{spaces, workshop};
use crate::ui::app::Foxy;

/// Work the Workshop view hands to a background thread. Everything here either
/// touches the network, spawns the Steam helper, or copies whole mod folders,
/// so none of it may run on the frame loop.
#[derive(Clone, Debug)]
pub enum WorkshopTask {
    Import {
        items: Vec<SharedItem>,
        download: bool,
        freeze: bool,
    },
    RefreshMetadata,
    Freeze {
        item_id: String,
    },
    FreezeAll {
        refresh: bool,
    },
    Remove {
        item_id: String,
        delete_data: bool,
    },
    ExportBundle {
        path: PathBuf,
        include_disabled: bool,
    },
    ImportBundle {
        path: PathBuf,
        download: bool,
    },
}

impl WorkshopTask {
    pub fn busy_label(&self) -> &'static str {
        match self {
            WorkshopTask::Import { .. } => "Importing Workshop mods",
            WorkshopTask::RefreshMetadata => "Refreshing Workshop details",
            WorkshopTask::Freeze { .. } | WorkshopTask::FreezeAll { .. } => "Freezing mod versions",
            WorkshopTask::Remove { .. } => "Removing Workshop mod",
            WorkshopTask::ExportBundle { .. } => "Writing share bundle",
            WorkshopTask::ImportBundle { .. } => "Reading share bundle",
        }
    }
}

pub struct WorkshopTaskOutcome {
    pub message: Result<String, String>,
}

pub struct WorkshopTaskContext {
    pub app_id: u32,
    pub game_id: String,
    pub steam_directory: String,
    pub timeout_seconds: u64,
}

pub fn run_workshop_task(
    task: WorkshopTask,
    ctx: WorkshopTaskContext,
    result_tx: Sender<WorkshopTaskOutcome>,
    repaint_ctx: Option<eframe::egui::Context>,
) {
    let message = execute(task, &ctx);
    match &message {
        Ok(text) => info!("Workshop task finished: {}", text),
        Err(error) => warn!("Workshop task failed: {}", error),
    }
    let _ = result_tx.send(WorkshopTaskOutcome { message });
    if let Some(ctx) = repaint_ctx {
        ctx.request_repaint();
    }
}

fn execute(task: WorkshopTask, ctx: &WorkshopTaskContext) -> Result<String, String> {
    let space_dir = spaces::active_game_space_dir();
    match task {
        WorkshopTask::Import {
            items,
            download,
            freeze,
        } => {
            let resolvable = items
                .iter()
                .filter(|item| item.is_resolvable())
                .cloned()
                .collect::<Vec<_>>();
            if resolvable.is_empty() {
                return Err("The shared list has no Steam Workshop ids Foxy can import".to_string());
            }
            let ids = resolvable
                .iter()
                .map(|item| item.item_id.clone())
                .collect::<Vec<_>>();
            let metadata = workshop::fetch_published_file_details(&ids)
                .map(workshop::metadata_by_id)
                .unwrap_or_else(|error| {
                    warn!("Workshop metadata lookup failed, importing without it: {error}");
                    Default::default()
                });
            if !metadata.is_empty() {
                workshop::validate_metadata_app_ids(&metadata, ctx.app_id)?;
            }

            let mut imported = 0;
            let mut failures = Vec::new();
            for item in &resolvable {
                let helper = if download {
                    match workshop::run_steam_helper_install(
                        ctx.app_id,
                        &item.item_id,
                        ctx.timeout_seconds,
                    ) {
                        Ok(outcome) => Some(outcome),
                        Err(error) => {
                            failures.push(format!("{}: {}", item.item_id, error));
                            None
                        }
                    }
                } else {
                    None
                };
                workshop::upsert_item(
                    &space_dir,
                    ctx.app_id,
                    &item.item_id,
                    item.name.clone(),
                    metadata.get(&item.item_id),
                    helper.as_ref(),
                    true,
                )?;
                if item.load_order.is_some() {
                    workshop::set_item_load_order(
                        &space_dir,
                        ctx.app_id,
                        &item.item_id,
                        item.load_order,
                    )?;
                }
                if freeze
                    && let Err(error) = workshop::freeze_item(
                        &space_dir,
                        ctx.app_id,
                        &item.item_id,
                        &ctx.steam_directory,
                    )
                {
                    failures.push(format!("{}: {}", item.item_id, error));
                }
                imported += 1;
            }
            Ok(summarize(
                format!("Imported {} Workshop mod(s)", imported),
                &failures,
            ))
        }
        WorkshopTask::RefreshMetadata => {
            let store = workshop::load_store(&space_dir)?;
            let ids = store
                .entries
                .iter()
                .filter(|entry| entry.app_id == ctx.app_id)
                .map(|entry| entry.item_id.clone())
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return Ok("No Workshop mods to refresh".to_string());
            }
            let metadata = workshop::metadata_by_id(workshop::fetch_published_file_details(&ids)?);
            for id in &ids {
                let entry = store.entry(ctx.app_id, id);
                workshop::upsert_item(
                    &space_dir,
                    ctx.app_id,
                    id,
                    None,
                    metadata.get(id),
                    None,
                    entry.is_none_or(|entry| entry.enabled),
                )?;
            }
            Ok(format!("Refreshed {} Workshop mod(s)", ids.len()))
        }
        WorkshopTask::Freeze { item_id } => {
            workshop::freeze_item(&space_dir, ctx.app_id, &item_id, &ctx.steam_directory)?;
            Ok(format!("Froze the current version of {}", item_id))
        }
        WorkshopTask::FreezeAll { refresh } => {
            let summary =
                pin::freeze_all(&space_dir, ctx.app_id, &ctx.steam_directory, true, refresh)?;
            let failures = summary
                .failed
                .iter()
                .map(|failure| format!("{}: {}", failure.item_id, failure.error))
                .collect::<Vec<_>>();
            Ok(summarize(
                format!(
                    "Froze {} mod(s), skipped {} already frozen",
                    summary.frozen.len(),
                    summary.skipped.len()
                ),
                &failures,
            ))
        }
        WorkshopTask::Remove {
            item_id,
            delete_data,
        } => {
            if let Err(error) =
                workshop::run_steam_helper_remove(ctx.app_id, &item_id, ctx.timeout_seconds)
            {
                warn!("Could not unsubscribe {} through Steam: {}", item_id, error);
            }
            workshop::remove_item(
                &space_dir,
                ctx.app_id,
                &item_id,
                &ctx.steam_directory,
                delete_data,
            )?;
            Ok(format!("Removed {}", item_id))
        }
        WorkshopTask::ExportBundle {
            path,
            include_disabled,
        } => {
            let checksum = workshop::checksum::state_checksum_for_space(
                &space_dir,
                &ctx.game_id,
                ctx.app_id,
                &ctx.steam_directory,
                &[],
            )?;
            let summary = bundle::export_bundle(
                &space_dir,
                &ctx.game_id,
                ctx.app_id,
                &path,
                BundleExportOptions {
                    include_disabled,
                    include_frozen_payloads: true,
                },
                Some(checksum),
                None,
            )?;
            Ok(format!(
                "Wrote {} mod(s) and {} frozen copy(ies)",
                summary.item_count, summary.payload_count
            ))
        }
        WorkshopTask::ImportBundle { path, download } => {
            let summary = bundle::import_bundle(&space_dir, ctx.app_id, &path, true)?;
            let mut failures = Vec::new();
            if download {
                for item_id in &summary.needs_download {
                    if let Err(error) =
                        workshop::run_steam_helper_install(ctx.app_id, item_id, ctx.timeout_seconds)
                    {
                        failures.push(format!("{}: {}", item_id, error));
                    }
                }
            }
            Ok(summarize(
                format!(
                    "Imported {} mod(s) and restored {} frozen copy(ies)",
                    summary.added.len() + summary.updated.len(),
                    summary.restored_payloads.len()
                ),
                &failures,
            ))
        }
    }
}

fn summarize(headline: String, failures: &[String]) -> String {
    if failures.is_empty() {
        return headline;
    }
    format!("{} ({} failed: {})", headline, failures.len(), {
        let shown = failures.iter().take(3).cloned().collect::<Vec<_>>();
        shown.join("; ")
    })
}

impl Foxy {
    /// The Steam helper timeout the Workshop view uses. Downloads run in a
    /// subprocess, so a generous ceiling only matters when Steam stalls.
    pub(crate) fn workshop_task_timeout_seconds(&self) -> u64 {
        600
    }
}
