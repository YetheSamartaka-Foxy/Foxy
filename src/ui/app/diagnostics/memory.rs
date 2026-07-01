use std::mem::size_of;
use std::time::{Duration, Instant};

use log::info;

use crate::core::api::{self, ModDiffSummary, ProgressEvent};
use crate::core::utils::addon_backup;
use crate::ui::app::{
    Foxy, MemoryBreakdownBucket, MemoryDiagnosticsReport, MemoryDiagnosticsSample,
};
use crate::ui::memory::{sample_process_memory, sample_process_virtual_memory_map};
use crate::ui::types::*;

impl Foxy {
    pub(crate) fn build_memory_diagnostics_report(&self) -> MemoryDiagnosticsReport {
        let repository_data_bytes = self.repository_view_state.repositories.capacity()
            * size_of::<Repository>()
            + self
                .repository_view_state
                .repositories
                .iter()
                .map(Self::heap_bytes_of_repository)
                .sum::<usize>()
            + self.repository_spaces.capacity() * size_of::<RepositorySpace>()
            + self
                .repository_spaces
                .iter()
                .map(Self::heap_bytes_of_repository_space)
                .sum::<usize>()
            + Self::heap_bytes_of_settings(&self.settings_view_state);

        let update_cache_bytes = self.mod_diff_cache.capacity() * size_of::<ModDiffSummary>()
            + self
                .mod_diff_cache
                .iter()
                .map(Self::heap_bytes_of_mod_diff_summary)
                .sum::<usize>()
            + self.pending_update_cache.capacity() * size_of::<(String, Vec<ModDiffSummary>)>()
            + self
                .pending_update_cache
                .iter()
                .map(|(url, mods)| {
                    Self::heap_bytes_of_string(url)
                        + mods.capacity() * size_of::<ModDiffSummary>()
                        + mods
                            .iter()
                            .map(Self::heap_bytes_of_mod_diff_summary)
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self.update_modal_sorted_mod_indices.capacity() * size_of::<usize>()
            + self.update_modal_mod_name_lowers.capacity() * size_of::<String>()
            + self
                .update_modal_mod_name_lowers
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>();

        let progress_history_bytes = self.progress_events.capacity() * size_of::<ProgressEvent>()
            + self
                .progress_events
                .iter()
                .map(Self::heap_bytes_of_progress_event)
                .sum::<usize>();

        let activity_log_bytes = self.activity_log_cache.capacity() * size_of::<api::LogEntry>()
            + self
                .activity_log_cache
                .iter()
                .map(Self::heap_bytes_of_log_entry)
                .sum::<usize>();

        let texture_cache_bytes = self.tracked_texture_bytes_total()
            + self.cached_icons.capacity() * size_of::<(String, egui::TextureHandle)>()
            + self.cached_repo_images.capacity() * size_of::<(String, egui::TextureHandle)>()
            + self.tracked_icon_texture_bytes.capacity() * size_of::<(String, usize)>()
            + self.tracked_repo_image_texture_bytes.capacity() * size_of::<(String, usize)>()
            + self
                .cached_icons
                .keys()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + self
                .cached_repo_images
                .keys()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>();

        let ui_cache_bytes = self.heap_bytes_of_repository_list_cache()
            + self.heap_bytes_of_addon_inventory_view_cache()
            + Self::heap_bytes_of_repository_addon_list_cache(&self.repository_addons_list_cache)
            + Self::heap_bytes_of_repository_addon_list_cache(
                &self.repository_optional_addons_list_cache,
            )
            + Self::heap_bytes_of_repository_external_addons_list_cache(
                &self.repository_external_addons_list_cache,
            )
            + Self::heap_bytes_of_string(&self.addons_filter)
            + Self::heap_bytes_of_string(&self.optional_addons_filter)
            + Self::heap_bytes_of_string(&self.external_addons_filter)
            + Self::heap_bytes_of_string(&self.external_addons_origin_filter)
            + Self::heap_bytes_of_string(&self.addon_state_filter)
            + Self::heap_bytes_of_string(&self.new_profile_name)
            + Self::heap_bytes_of_string(&self.direct_download_url_input)
            + Self::heap_bytes_of_string(&self.direct_download_destination_input)
            + Self::heap_bytes_of_string(&self.add_repository_input_address)
            + Self::heap_bytes_of_string(&self.editor_mission_search)
            + Self::heap_bytes_of_string(&self.editor_mission_folder)
            + Self::heap_bytes_of_string(&self.editor_mission_terrain_filter)
            + self
                .cached_all_addons
                .as_ref()
                .map(|addons| {
                    addons.capacity() * size_of::<crate::ui::app::AddonInventoryEntry>()
                        + addons
                            .iter()
                            .map(|(name, path, origin, _size_bytes)| {
                                Self::heap_bytes_of_string(name)
                                    + Self::heap_bytes_of_string(path)
                                    + Self::heap_bytes_of_string(origin)
                            })
                            .sum::<usize>()
                })
                .unwrap_or_default();

        let backup_manager_bytes = self.backup_manager_records.capacity()
            * size_of::<addon_backup::AddonBackupRecord>()
            + self
                .backup_manager_records
                .iter()
                .map(Self::heap_bytes_of_backup_record)
                .sum::<usize>()
            + Self::heap_bytes_of_string(&self.backup_manager_filter);

        let runtime_queue_bytes = self.server_statuses.capacity()
            * size_of::<((String, String), ServerStatusCache)>()
            + self
                .server_statuses
                .keys()
                .map(|(address, port)| {
                    Self::heap_bytes_of_string(address) + Self::heap_bytes_of_string(port)
                })
                .sum::<usize>()
            + self.pending_server_queries.capacity() * size_of::<(String, String)>()
            + self
                .pending_server_queries
                .iter()
                .map(|(address, port)| {
                    Self::heap_bytes_of_string(address) + Self::heap_bytes_of_string(port)
                })
                .sum::<usize>()
            + Self::heap_bytes_of_string_set(&self.deferred_fs_scan)
            + Self::heap_bytes_of_string_set(&self.pending_quick_scan_urls)
            + Self::heap_bytes_of_string_set(&self.pending_quick_scan_prevalidated_urls)
            + Self::heap_bytes_of_string_set(&self.pending_quick_scan_force_fresh_addon_hash_urls)
            + Self::heap_bytes_of_string_set(&self.quick_scan_pending)
            + Self::heap_bytes_of_string_set(&self.repo_db_reset_pending_recheck)
            + Self::heap_bytes_of_startup_sync_queue(&self.startup_recheck_queue)
            + Self::heap_bytes_of_repo_space_sync_queue(&self.repository_space_sync_queue)
            + Self::heap_bytes_of_string_pair_queue(&self.addon_hash_recalc_queue)
            + self.mod_download_progress.capacity()
                * size_of::<(String, (f32, usize, usize, u64, u64))>()
            + self
                .mod_download_progress
                .keys()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + self.repo_states.capacity() * size_of::<(String, RepoState)>()
            + self
                .repo_states
                .keys()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + self.pending_image_jobs.capacity() * size_of::<(String, bool)>()
            + self
                .pending_image_jobs
                .iter()
                .map(|(checksum, _)| Self::heap_bytes_of_string(checksum))
                .sum::<usize>();

        let diagnostics_bytes = self.memory_diagnostics_history.capacity()
            * size_of::<MemoryDiagnosticsSample>()
            + self
                .memory_diagnostics_history
                .iter()
                .map(|sample| Self::heap_bytes_of_string(&sample.label))
                .sum::<usize>();

        let mut buckets = vec![
            MemoryBreakdownBucket {
                label: "Repository data".to_string(),
                bytes: repository_data_bytes,
                detail: format!(
                    "{} repositories, {} spaces",
                    self.repository_view_state.repositories.len(),
                    self.repository_spaces.len()
                ),
            },
            MemoryBreakdownBucket {
                label: "Update caches".to_string(),
                bytes: update_cache_bytes,
                detail: format!(
                    "{} active mods, {} repos cached",
                    self.mod_diff_cache.len(),
                    self.pending_update_cache.len()
                ),
            },
            MemoryBreakdownBucket {
                label: "Texture cache".to_string(),
                bytes: texture_cache_bytes,
                detail: format!("{} cached textures", self.tracked_texture_count()),
            },
            MemoryBreakdownBucket {
                label: "Runtime queues".to_string(),
                bytes: runtime_queue_bytes,
                detail: format!(
                    "{} pending image jobs, {} queued rechecks",
                    self.pending_image_jobs.len(),
                    self.startup_recheck_queue.len()
                ),
            },
            MemoryBreakdownBucket {
                label: "Activity log".to_string(),
                bytes: activity_log_bytes,
                detail: format!("{} log entries", self.activity_log_cache.len()),
            },
            MemoryBreakdownBucket {
                label: "UI caches".to_string(),
                bytes: ui_cache_bytes,
                detail: format!(
                    "{} filtered repositories cached",
                    self.repository_list_cache.filtered_indices.len()
                ),
            },
            MemoryBreakdownBucket {
                label: "Progress history".to_string(),
                bytes: progress_history_bytes,
                detail: format!("{} recent progress events", self.progress_events.len()),
            },
            MemoryBreakdownBucket {
                label: "Backup manager".to_string(),
                bytes: backup_manager_bytes,
                detail: format!("{} backup records", self.backup_manager_records.len()),
            },
            MemoryBreakdownBucket {
                label: "Diagnostics history".to_string(),
                bytes: diagnostics_bytes,
                detail: format!("{} samples", self.memory_diagnostics_history.len()),
            },
        ];

        buckets.sort_by_key(|bucket| std::cmp::Reverse(bucket.bytes));
        let tracked_total_bytes = buckets.iter().map(|bucket| bucket.bytes).sum::<usize>();
        let process = sample_process_memory();
        let untracked_bytes = process
            .baseline_bytes()
            .map(|baseline| baseline as i64 - tracked_total_bytes as i64);

        MemoryDiagnosticsReport {
            process,
            os_virtual_memory_map: self.memory_diagnostics_process_map.clone(),
            buckets,
            tracked_total_bytes,
            untracked_bytes,
        }
    }

    pub(crate) fn refresh_memory_diagnostics_process_map(&mut self, force: bool) {
        if !force && !self.show_memory_diagnostics_window {
            return;
        }

        let refresh_interval = if self.current_sync_mode.is_some() {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(3)
        };
        if !force
            && self
                .memory_diagnostics_last_process_map_at
                .is_some_and(|last| last.elapsed() < refresh_interval)
        {
            return;
        }

        self.memory_diagnostics_process_map = sample_process_virtual_memory_map();
        self.memory_diagnostics_last_process_map_at = Some(Instant::now());
    }

    pub(crate) fn capture_memory_diagnostics_snapshot(
        &mut self,
        label: impl Into<String>,
        log_snapshot: bool,
    ) {
        const MEMORY_DIAGNOSTICS_HISTORY_LIMIT: usize = 240;

        let label = label.into();
        let report = self.build_memory_diagnostics_report();
        let captured_at = Instant::now();
        let sample = MemoryDiagnosticsSample {
            captured_at,
            label: label.clone(),
            process: report.process.clone(),
            task_manager_memory_bytes: report.process.task_manager_memory_bytes(),
            working_set_bytes: report.process.working_set_bytes,
            private_bytes: report.process.private_bytes,
            tracked_total_bytes: report.tracked_total_bytes,
            untracked_bytes: report.untracked_bytes,
            buckets: report.buckets.clone(),
        };

        if self.memory_diagnostics_history.len() >= MEMORY_DIAGNOSTICS_HISTORY_LIMIT {
            self.memory_diagnostics_history.pop_front();
        }
        self.memory_diagnostics_history.push_back(sample);
        self.memory_diagnostics_last_sample_at = Some(captured_at);

        if log_snapshot {
            let top_buckets = report
                .buckets
                .iter()
                .take(3)
                .map(|bucket| {
                    format!(
                        "{}={}",
                        bucket.label,
                        Self::format_bytes_short(bucket.bytes as u64)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let untracked = report
                .untracked_bytes
                .map(|bytes| {
                    if bytes >= 0 {
                        Self::format_bytes_short(bytes as u64)
                    } else {
                        format!("-{}", Self::format_bytes_short(bytes.unsigned_abs()))
                    }
                })
                .unwrap_or_else(|| "n/a".to_string());
            info!(
                "Memory snapshot [{}]: working_set={} private={} tracked={} untracked={} top={}",
                label,
                Self::format_optional_bytes(report.process.working_set_bytes),
                Self::format_optional_bytes(report.process.private_bytes),
                Self::format_bytes_short(report.tracked_total_bytes as u64),
                untracked,
                top_buckets
            );
        }
    }

    pub(crate) fn latest_sync_start_memory_sample(&self) -> Option<&MemoryDiagnosticsSample> {
        self.memory_diagnostics_history
            .iter()
            .rev()
            .find(|sample| sample.label.starts_with("sync-start "))
    }

    pub(in crate::ui::app) fn memory_diagnostics_stage_key(label: &str) -> String {
        if label.starts_with("Hashing ") {
            "Hashing files".to_string()
        } else if label.starts_with("Saving parts ") {
            "Saving parts".to_string()
        } else if label.starts_with("Updating files ") {
            "Updating files".to_string()
        } else if label.starts_with("Saving files ") {
            "Saving files".to_string()
        } else if label.starts_with("Updating addons ") {
            "Updating addons".to_string()
        } else if label.starts_with("Saving addons ") {
            "Saving addons".to_string()
        } else if label.starts_with("Updating repositories ") || label == "Updating repositories" {
            "Updating repositories".to_string()
        } else if label.starts_with("Saving repositories ") {
            "Saving repositories".to_string()
        } else {
            label.to_string()
        }
    }

    pub(in crate::ui::app) fn maybe_sample_memory_diagnostics(&mut self) {
        let sample_interval = if self.current_sync_mode.is_some() {
            Duration::from_millis(500)
        } else if self.show_memory_diagnostics_window {
            Duration::from_secs(1)
        } else {
            return;
        };

        if self
            .memory_diagnostics_last_sample_at
            .is_some_and(|last| last.elapsed() < sample_interval)
        {
            return;
        }

        let label = if let Some(stage) = &self.recheck_stage_label {
            Self::memory_diagnostics_stage_key(stage)
        } else if let Some(mode) = self.current_sync_mode {
            format!("{mode:?}")
        } else {
            "Memory diagnostics".to_string()
        };
        self.capture_memory_diagnostics_snapshot(label, false);
    }

    pub(in crate::ui::app) fn remember_loaded_texture_bytes(
        &mut self,
        checksum_hex: &str,
        is_icon: bool,
        bytes: usize,
    ) {
        if is_icon {
            self.tracked_icon_texture_bytes
                .insert(checksum_hex.to_string(), bytes);
        } else {
            self.tracked_repo_image_texture_bytes
                .insert(checksum_hex.to_string(), bytes);
        }
    }

    pub(in crate::ui::app) fn reuse_tracked_texture_bytes(
        &mut self,
        checksum_hex: &str,
        is_icon: bool,
    ) {
        let bytes = if is_icon {
            self.tracked_icon_texture_bytes
                .get(checksum_hex)
                .copied()
                .or_else(|| {
                    self.tracked_repo_image_texture_bytes
                        .get(checksum_hex)
                        .copied()
                })
        } else {
            self.tracked_repo_image_texture_bytes
                .get(checksum_hex)
                .copied()
                .or_else(|| self.tracked_icon_texture_bytes.get(checksum_hex).copied())
        };

        if let Some(bytes) = bytes {
            self.remember_loaded_texture_bytes(checksum_hex, is_icon, bytes);
        }
    }
}
