use eframe::egui;

use crate::core::server::owner::{UpdateBlocker, UpdateBlockerKind};
use crate::ui::app::Foxy;

const OWNER_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Server owners checked off the UI thread before a queued installer replaces the binary.
#[derive(Default)]
pub(crate) struct InstallerOwnerCheck {
    worker: Option<std::thread::JoinHandle<Vec<UpdateBlocker>>>,
    last: Option<(std::time::Instant, Vec<UpdateBlocker>)>,
}

impl InstallerOwnerCheck {
    /// Blockers from a recent check, starting the next check when one is due; `None` until a
    /// fresh answer exists.
    fn current(&mut self) -> Option<&[UpdateBlocker]> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
            && let Some(worker) = self.worker.take()
        {
            self.last = worker
                .join()
                .ok()
                .map(|blockers| (std::time::Instant::now(), blockers));
        }
        let due = self
            .last
            .as_ref()
            .is_none_or(|(checked, _)| checked.elapsed() >= OWNER_CHECK_INTERVAL);
        if due && self.worker.is_none() {
            let root = crate::core::utils::app_paths::configured_data_dir();
            self.worker = std::thread::Builder::new()
                .name("installer-owner-check".into())
                .spawn(move || crate::core::server::owner::update_blockers(&root))
                .ok();
        }
        self.last
            .as_ref()
            .filter(|(checked, _)| checked.elapsed() < OWNER_CHECK_INTERVAL * 2)
            .map(|(_, blockers)| blockers.as_slice())
    }

    fn reset(&mut self) {
        self.last = None;
    }
}

impl Foxy {
    pub(crate) fn accepted_work_in_flight(&self) -> bool {
        self.repository_sync_active()
            || self.pending_link_target_sync.is_some()
            || self.is_direct_download_running()
            || self.launch_action.is_pending()
            || self.game_space_removal_in_flight()
            || self.backup_manager_work_in_flight()
            || self.profile_backup_work_in_flight()
            || !self.active_quick_scan_instance_keys.is_empty()
            || self.hash_evidence_retry_in_flight
            || self.retained_files_cleanup_in_flight
            || !self.pending_repository_db_wipes.is_empty()
            || !self.pending_addon_deletes.is_empty()
            || !self.pending_addon_force_redownloads.is_empty()
            || self.addon_hash_recalc_in_flight
            || self.repository_space_import_in_flight
            || self.scheduler_active_run.is_some()
            || self.pending_download_retry.is_some()
            || !self.repository_space_sync_queue.is_empty()
            || !self.repository_visual_folder_sync_queue.is_empty()
            || !self.startup_recheck_queue.is_empty()
            || self
                .addon_backup_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            || self
                .workshop_task_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
    }

    pub(crate) fn poll_installer_request(&mut self, ctx: &egui::Context) {
        let Some(path) = self.pending_installer_path.clone() else {
            return;
        };
        let busy = self.accepted_work_in_flight();
        self.poll_persistence_results();
        if !busy {
            self.maybe_dispatch_persistence_requests(true);
        }
        let owners = self.installer_owner_check.current().map(<[_]>::to_vec);
        if !busy
            && owners.as_ref().is_some_and(Vec::is_empty)
            && !self.has_pending_persistence_writes()
            && self.close_requested_at.is_none()
            && self.configuration_recovery_error.is_none()
            && self.pending_installer_error.is_none()
        {
            self.close_requested_at = Some(std::time::Instant::now());
            if let Err(error) = crate::core::tasks::app_update::launch_installer(&path, true) {
                self.close_requested_at = None;
                self.pending_installer_error = Some(error.to_string());
                self.show_error_toast(self.t_fmt(
                    "Could not start the verified installer: {error}",
                    &[("error", error.to_string())],
                ));
            }
            return;
        }
        let mut cancel = false;
        let mut retry = false;
        egui::Window::new(self.t("Foxy update queued"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(
                    self.t(
                        "The verified installer will start after accepted work and saves finish.",
                    ),
                );
                for blocker in owners.iter().flatten() {
                    let space = ("space", blocker.space_name.clone());
                    ui.label(match blocker.kind {
                        UpdateBlockerKind::Detached => self.t_fmt(
                            "{space}: a detached server owner is running. Stop its servers and shut it down with server supervise --shutdown before the update installs.",
                            &[space],
                        ),
                        UpdateBlockerKind::Foreground => self.t_fmt(
                            "{space}: a foreground server owner is running. Stop its servers and shut it down before the update installs.",
                            &[space],
                        ),
                        UpdateBlockerKind::Sessions(count) => self.t_fmt(
                            "{space}: {count} supervised server sessions are running. Stop them before the update installs.",
                            &[space, ("count", count.to_string())],
                        ),
                        UpdateBlockerKind::Unreachable => self.t_fmt(
                            "{space}: its server owner did not answer, so running servers cannot be ruled out. Stop them or shut the owner down before the update installs.",
                            &[space],
                        ),
                        UpdateBlockerKind::RegistryUnreadable => self.t(
                            "The game space list could not be read, so running servers cannot be ruled out.",
                        ),
                    });
                }
                if let Some(error) = &self.pending_installer_error {
                    ui.label(error);
                    retry = ui.button(self.t("Retry")).clicked();
                }
                cancel = ui.button(self.t("Cancel queued installation")).clicked();
            });
        if cancel {
            self.pending_installer_path = None;
            self.pending_installer_error = None;
            self.installer_owner_check.reset();
        } else {
            if retry {
                self.pending_installer_error = None;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    pub(crate) fn render_configuration_recovery_error(&mut self, ctx: &egui::Context) -> bool {
        let Some(error) = self.configuration_recovery_error.clone() else {
            return false;
        };
        egui::Modal::new(egui::Id::unique("configuration_recovery_error")).show(ctx, |ui| {
            ui.heading(self.t("Configuration recovery could not finish"));
            ui.label(self.t("Restore access to the game-space files and restart Foxy. Your pending recovery data has been kept."));
            ui.label(error);
            self.render_export_logs_button(ui);
            if ui.button(self.t("Close Foxy")).clicked() { std::process::exit(1); }
        });
        true
    }
}
