use crate::ui::app::Foxy;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{RichText, Ui};
use log::warn;

impl Foxy {
    /// The game a custom game repository delivers, shown in place of an addon
    /// row so the build reads as the game itself.
    pub(super) fn render_repository_game_build_section(
        &mut self,
        ui: &mut Ui,
        selected_idx: usize,
    ) {
        let Some(repo) = self.repository_view_state.repositories.get(selected_idx) else {
            return;
        };
        let Some(build) = repo.game_build.clone() else {
            return;
        };
        let repo_path = repo.path.trim().to_string();

        ui.heading(tr("Game"));
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&build.addon).strong());
            ui.label(
                RichText::new(tr_fmt(
                    "Starts {file}",
                    &[("file", build.executable.clone())],
                ))
                .color(self.color_text_dim()),
            );
            let open = ui
                .button(tr("Open game folder"))
                .on_hover_text(tr("Open the folder the game was downloaded to."));
            if open.clicked() {
                let dir = crate::core::utils::fs_safety::resolve_child_dir_case_insensitive(
                    std::path::Path::new(&repo_path),
                    &build.addon,
                );
                match dir {
                    Some(dir) => {
                        if !self.open_addon_directory(&build.addon, &dir.display().to_string()) {
                            warn!("Failed to open game folder for {}", build.addon);
                            self.show_error_toast(self.t("Failed to open the game folder."));
                        }
                    }
                    None => self.show_error_toast(self.t_fmt(
                        "{game} has not been downloaded yet. Update the repository first.",
                        &[("game", build.addon.clone())],
                    )),
                }
            }
        });
        ui.add_space(10.0);
    }
}
