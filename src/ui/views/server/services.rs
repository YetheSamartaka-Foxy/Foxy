use crate::core::server::ServiceExportFormat;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_service_export(&mut self, ui: &mut Ui) {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .service_exports
        {
            return;
        }
        let space_id = crate::core::game::spaces::active_game_space()
            .space_id
            .clone();
        self.server_view.services.ensure_space(&space_id);

        ui.separator();
        ui.collapsing(tr("Unattended owner export"), |ui| {
            let format = Self::server_service_export_format();
            match format {
                ServiceExportFormat::SystemdUser => {
                    ui.label(tr("Export a foreground systemd user unit. Register it separately with systemd."));
                }
                ServiceExportFormat::WindowsUserTask => {
                    ui.label(tr("Export a current-user task definition. Import it separately; it runs after that user logs in."));
                }
            }
            ui.label(tr("Stopping through the unit or task may stop Foxy and its servers. Use the idle-only stop command only after stopping every server instance."));

            ui.horizontal_wrapped(|ui| {
                let preview = ui.add_enabled(!self.server_view.busy(), egui::Button::new(tr("Preview export")));
                self.server_view.services.controls_focus = Some(preview.id);
                Self::focus_outline(ui, &preview);
                if preview.clicked() {
                    self.request_server_service_export_plan();
                }
            });

            let plan = self
                .server_view
                .services
                .plan_for_space(&space_id)
                .cloned();
            if let Some(plan) = plan {
                ui.label(tr("Review this definition before saving it."));
                for warning in &plan.warnings {
                    ui.colored_label(self.color_warn(), tr(warning));
                }
                let mut definition = plan.definition.clone();
                ui.add(
                    egui::TextEdit::multiline(&mut definition)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(12)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                )
                .accessible_name(tr("Unattended owner definition preview"));
                ui.horizontal_wrapped(|ui| {
                    let caption = ui.label(tr("Destination file"));
                    let id = egui::Id::unique(("server.service-export-path", space_id.as_str()));
                    let edit = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.server_view.services.path)
                                .id(id)
                                .desired_width(320.0)
                                .hint_text(&plan.file_name),
                        )
                        .labelled_by(caption.id);
                    Self::focus_outline(ui, &edit);
                    if edit.changed() {
                        self.server_view.services.exported = false;
                    }
                    let path_is_absolute = std::path::Path::new(
                        self.server_view.services.path.trim(),
                    )
                    .is_absolute();
                    let save = ui.add_enabled(
                        path_is_absolute && !self.server_view.busy(),
                        egui::Button::new(tr("Save new definition")),
                    );
                    self.server_view.services.controls_focus = Some(save.id);
                    Self::focus_outline(ui, &save);
                    if save.clicked() {
                        self.export_reviewed_server_service();
                    }
                });
                if self.server_view.services.exported {
                    ui.label(tr("Definition saved."));
                }
            }
        });
    }
}
