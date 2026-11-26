use crate::core::game::{registry, spaces};
use crate::core::server::DraftChange;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::tr;
use eframe::egui::{self, Ui};

impl Foxy {
    pub(super) fn render_server_installation(&mut self, ui: &mut Ui) {
        let capabilities = registry()
            .get(&spaces::active_game_space().game_id)
            .map(|module| module.server_capabilities())
            .unwrap_or_default();
        let Some(editor) = &self.server_view.editor else {
            return;
        };
        let id = editor.baseline.id.clone();
        let expected_revision = editor.baseline.revision;
        let binding = editor.baseline.installation_id.clone();
        let field_scope = editor.field_scope();
        let dirty = editor.dirty();
        if let Some(preflight) = &self.server_view.preflight
            && (dirty
                || preflight.instance_id != id
                || preflight.instance_revision != expected_revision)
        {
            self.server_view.preflight = None;
        }
        let mut action = None;
        ui.heading(tr("Existing installation and deployment review"));
        ui.weak(tr("Adoption records an external installation without changing or executing it. It grants no update or removal authority."));
        ui.add_enabled_ui(!dirty, |ui| {
            if capabilities.installation_adoption {
                ui.horizontal_wrapped(|ui| {
                    let dir_label = ui.label(tr("Dedicated installation directory"));
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.server_view.installation_directory)
                                .id(field_scope.with("installation-directory"))
                                .desired_width(350.0),
                        )
                        .labelled_by(dir_label.id)
                        .changed()
                    {
                        self.server_view.inspection = None;
                        self.server_view.inspection_build = None;
                    }
                    if ui.button(tr("Inspect installation")).clicked() {
                        action = Some(ServerTaskAction::Inspect(self.server_view.installation_directory.clone().into()));
                    }
                });
                if let Some(installation) = &self.server_view.inspection {
                    ui.label(installation.directory.display().to_string());
                    ui.label(self.t_fmt("Executable: {name}; platform: {platform}; bytes: {bytes}", &[("name", installation.executable.clone()), ("platform", format!("{:?}", installation.platform)), ("bytes", installation.evidence.bytes.to_string())]));
                    ui.monospace(&installation.evidence.blake3);
                    if let Some(build) = &self.server_view.inspection_build {
                        ui.label(self.t_fmt("Executable version metadata: {version}", &[("version", build.executable_version.map(|version| version.to_string()).unwrap_or_else(|| tr("Unknown")))]));
                        if let Some(steam) = &build.steam_manifest {
                            let branch = if steam.branch_conflict { tr("Conflicting local branch metadata") } else { steam.branch.clone().unwrap_or_else(|| tr("Unknown")) };
                            ui.label(self.t_fmt("Steam manifest: app {app}, build {build}, branch {branch}", &[("app", steam.app_id.to_string()), ("build", steam.build_id.clone()), ("branch", branch)]));
                        }
                        if let Some(layout) = &build.tested_engine_layout {
                            let description = if installation.game == crate::core::server::ServerDefinitionKind::Reforger {
                                "Exact executable matches native-tested Reforger {version}: isolated runtime copies and private profile, download, temporary and log paths."
                            } else {
                                "Exact executable matches native-tested Arma {version} layouts: shared root-relative directories or isolated copies, with base keys retained."
                            };
                            ui.label(self.t_fmt(description, &[("version", layout.tested_runtime_version.into())]));
                        } else {
                            ui.label(tr("This executable does not match a native-tested engine layout."));
                        }
                    }
                    ui.label(tr("Layout compatibility does not authorize deployment or Start. Package completeness and vendor authenticity remain unverified."));
                    if ui.button(tr("Adopt and bind inspected installation")).clicked() {
                        action = Some(ServerTaskAction::Catalog(Some(DraftChange::Adopt { id: id.clone(), expected_revision, installation: installation.clone() })));
                    }
                }
                let installations = self.server_view.catalog.as_ref().map(|catalog| catalog.installations.clone()).unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    let choice_label = ui.label(tr("Recorded installation"));
                    let combo = egui::ComboBox::from_id_salt(field_scope.with("installation-choice"))
                        .selected_text(installations.iter().find(|item| Some(&item.id) == self.server_view.installation_choice.as_ref()).map(|item| item.directory.file_name().unwrap_or_default().to_string_lossy().into_owned()).unwrap_or_else(|| tr("No installation")))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.server_view.installation_choice, None, tr("No installation"));
                            for installation in &installations {
                                ui.selectable_value(&mut self.server_view.installation_choice, Some(installation.id.clone()), installation.directory.display().to_string()).on_hover_text(installation.id.as_str());
                            }
                        });
                    combo.response.labelled_by(choice_label.id);
                    if ui.add_enabled(binding != self.server_view.installation_choice, egui::Button::new(tr("Apply installation binding"))).clicked() {
                        action = Some(ServerTaskAction::Catalog(Some(DraftChange::Bind { id: id.clone(), expected_revision, installation_id: self.server_view.installation_choice.clone() })));
                    }
                });
                if let Some(installation) = installations.iter().find(|item| Some(&item.id) == binding.as_ref()) {
                    ui.label(installation.directory.display().to_string());
                }
            }
            if capabilities.deployment_preflight && ui.button(tr("Review deployment preflight")).clicked() {
                action = Some(ServerTaskAction::Preflight(id));
            }
        });
        if let Some(preflight) = &self.server_view.preflight {
            ui.label(tr(
                "Deployment is not ready. These are proposed paths; no files were written.",
            ));
            if let Some(consumers) = &preflight.installation_consumers
                && consumers.inspection_complete
            {
                ui.label(self.t_fmt(
                    "Registered consumer records: {count}. Liveness has not been verified.",
                    &[("count", consumers.recorded_consumers.len().to_string())],
                ));
            }
            if let Some(recovery) = &preflight.installation_recovery {
                self.render_server_recovery(ui, recovery);
            }
            for path in &preflight.paths {
                ui.label(format!("{}: {}", path.role, path.path.display()));
            }
            for blocker in &preflight.blockers {
                ui.label(format!("{}: {}", blocker.code, tr(&blocker.message)));
            }
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
    }
}
