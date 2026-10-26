use crate::ui::app::Foxy;
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::types::Repository;
use eframe::egui::{Button, Color32, CursorIcon, TextEdit, Ui};
use rfd::FileDialog;

impl Foxy {
    /// Name, address, local path, and space-inherited path display. Address and
    /// path are read-only while `identity_locked` (work for this instance runs).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_repository_configuration_identity(
        ui: &mut Ui,
        repo: &mut Repository,
        inherited_space: &Option<(String, String)>,
        color_text_dim: Color32,
        pad_f32: f32,
        identity_locked: bool,
        changed: &mut bool,
        addr_changed: &mut bool,
    ) {
        ui.horizontal(|ui| {
            ui.label(tr("Name"));
        });
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            if ui
                .add(
                    TextEdit::singleline(&mut repo.name)
                        .desired_width(w)
                        .char_limit(100),
                )
                .changed()
            {
                *changed = true;
            }
        });
        ui.separator();

        ui.horizontal(|ui| {
            ui.label(tr("Address"));
        });
        ui.horizontal(|ui| {
            let w = ui.available_width() - 2.0 * pad_f32;
            let r = ui.add_enabled(
                !identity_locked,
                TextEdit::singleline(&mut repo.address)
                    .desired_width(w)
                    .char_limit(500),
            );
            if r.changed() {
                *changed = true;
                *addr_changed = true;
            }
            if r.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
        });
        ui.separator();

        ui.horizontal(|ui| {
            ui.label(tr("Local Path"));
        });
        if let Some((space_name, shared_path)) = inherited_space
            && (repo.path.trim().is_empty() || repo.path == *shared_path)
        {
            let inherited_path_text = tr_fmt(
                "Path is inherited from repository space {name}",
                &[("name", space_name.clone())],
            );
            ui.horizontal(|ui| {
                ui.colored_label(color_text_dim, inherited_path_text);
            });
        }
        ui.horizontal(|ui| {
            let default_button_width = if inherited_space.is_some() { 80.0 } else { 0.0 };
            let w = ui.available_width() - 120.0 - default_button_width;
            let r = ui.add_enabled(
                !identity_locked,
                TextEdit::singleline(&mut repo.path).desired_width(w.max(100.0)),
            );
            if r.changed() {
                *changed = true;
            }
            if r.hovered() {
                ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
            }
            if ui
                .add_enabled(!identity_locked, Button::new(tr("Browse")))
                .clicked()
                && let Some(dir) =
                    crate::ui::app::agent_support::pick_folder(|| FileDialog::new().pick_folder())
            {
                repo.path = dir.display().to_string();
                *changed = true;
            }
            if let Some((_, shared_path)) = inherited_space
                && ui
                    .add_enabled(!identity_locked, Button::new(tr("Default")))
                    .clicked()
            {
                repo.path = shared_path.clone();
                *changed = true;
            }
        });
        if identity_locked {
            ui.colored_label(
                color_text_dim,
                tr("The address and local path can be changed once the running check, update, or database wipe of this repository finishes."),
            );
        }
        ui.separator();
    }
}
