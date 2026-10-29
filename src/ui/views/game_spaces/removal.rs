use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};

use crate::core::game::spaces;
use crate::ui::app::Foxy;

impl Foxy {
    pub(crate) fn game_space_removal_in_flight(&self) -> bool {
        self.game_spaces_view_state.removal_rx.is_some()
    }

    pub(super) fn start_game_space_removal(
        &mut self,
        id: String,
        preservation_root: Option<PathBuf>,
    ) {
        if self.game_space_removal_in_flight() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.game_spaces_view_state.removal_rx = Some(receiver);
        let repaint = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let result = spaces::remove_game_space_with_policy(
                &id,
                spaces::GameSpaceRemovalPolicy {
                    preserve_private_payloads_to: preservation_root,
                },
            );
            let _ = sender.send(result);
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        });
    }

    pub(crate) fn poll_game_space_removal(&mut self, ctx: &egui::Context) {
        let result = self
            .game_spaces_view_state
            .removal_rx
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err(
                    "Game-space removal worker ended without a result".to_string(),
                )),
                Err(TryRecvError::Empty) => None,
            });
        if let Some(result) = result {
            self.game_spaces_view_state.removal_rx = None;
            self.reload_game_spaces();
            match result {
                Ok(removed) => self.show_success_toast(self.t_fmt(
                    "Game space {name} removed.",
                    &[("name", removed.display_name)],
                )),
                Err(error) => self.show_error_toast(
                    self.t_fmt("Could not remove game space: {error}", &[("error", error)]),
                ),
            }
        }
        if self.game_space_removal_in_flight() {
            egui::Modal::new(egui::Id::unique("game_space_removal_busy")).show(ctx, |ui| {
                ui.heading(self.t("Remove game space"));
                ui.add(crate::ui::app::PacedSpinner::new());
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
