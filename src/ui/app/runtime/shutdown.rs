use eframe::egui;

use crate::ui::app::Foxy;

impl Foxy {
    pub(super) fn render_close_wait(&mut self, ctx: &egui::Context, saving: bool) {
        let started = self
            .close_worker_wait_started_at
            .or(self.close_requested_at);
        let force_available = started.is_some_and(|at| at.elapsed().as_secs() >= 15);
        let mut force = false;
        egui::Modal::new(egui::Id::unique("close_cleanup_wait")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading(self.t("Closing Foxy"));
            ui.label(self.t(if saving {
                "Waiting for your changes to be saved before closing."
            } else {
                "Cancelling the update and restoring previous files before closing."
            }));
            ui.add(crate::ui::app::PacedSpinner::new());
            if force_available {
                ui.label(self.t("Force quitting can leave recovery or unsaved changes unfinished. Foxy will retry interrupted restoration when reopened."));
                force = ui.button(self.t("Force quit")).clicked();
            }
        });
        if force {
            let root = std::path::PathBuf::from(self.effective_temp_directory());
            match crate::core::tasks::download_files::UpdateRollbackSession::request_process_restore(
                &root,
            ) {
                Ok(()) => std::process::exit(1),
                Err(err) => {
                    log::error!("Could not preserve interrupted restoration: {err}");
                    self.show_error_toast(self.t("Recovery information could not be saved. Keep Foxy open and restore access to its temporary folder."));
                }
            }
        }
    }
}
