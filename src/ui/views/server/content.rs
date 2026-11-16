use crate::core::server::{ContentPlanRequest, ServerAddonBinding, ServerAddonRole};
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(super) fn render_server_content(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .content_bindings
        {
            return None;
        }
        let Some(editor) = &self.server_view.editor else {
            return None;
        };
        let instance_id = editor.baseline.id.clone();
        let field_scope = editor.field_scope().with("content");
        let instance_revision = editor.baseline.revision;
        let dirty = editor.dirty();
        let catalog_revision = self.server_view.catalog.as_ref()?.revision;
        let stale_plan = self.server_view.content_plan.as_ref().is_some_and(|plan| {
            plan.instance_id != instance_id
                || plan.instance_revision != instance_revision
                || plan.catalog_revision != catalog_revision
                || self
                    .server_view
                    .content_sources
                    .as_ref()
                    .map(|sources| &sources.fingerprint)
                    != Some(&plan.content_sources_fingerprint)
        });
        if dirty || stale_plan {
            self.server_view.content_plan = None;
            self.server_view.content_bindings_plan = None;
        }
        ui.separator();
        ui.heading(tr("Server content planning"));
        ui.label(tr(
            "Preview only: this does not download or copy files. Present files are not verified.",
        ));
        let mut action = None;
        let refresh = ui.add_enabled(
            !dirty && !self.server_view.busy(),
            egui::Button::new(tr("Refresh server content profiles")),
        );
        if refresh.clicked() {
            action = Some(ServerTaskAction::LoadContentSources);
        }
        let reload = ui.add_enabled(
            !dirty && !self.server_view.busy(),
            egui::Button::new(tr("Reload saved content bindings")),
        );
        if reload.clicked() && !dirty && !self.server_view.busy() {
            action = Some(ServerTaskAction::ReloadContentBindings(instance_id.clone()));
        }
        let mut anchor = reload;
        if dirty {
            ui.label(tr(
                "Save or discard draft changes before previewing content.",
            ));
        }
        if let Some(sources) = &self.server_view.content_sources {
            if sources.sources.profiles.is_empty() {
                ui.label(tr("No content profiles are registered. Add folders and profiles with the server content registration CLI."));
            }
            let profiles = sources.sources.profiles.clone();
            let mut changed = false;
            ui.add_enabled_ui(!dirty && !self.server_view.busy(), |ui| {
                for profile in &profiles {
                    let mut selected = self
                        .server_view
                        .content_bindings
                        .iter()
                        .any(|binding| binding.source_id == profile.name);
                    let response = ui
                        .push_id((field_scope, &profile.name, "selected"), |ui| {
                            ui.checkbox(
                                &mut selected,
                                tr_fmt("Profile: {name}", &[("name", profile.name.clone())]),
                            )
                        })
                        .inner;
                    if response.changed() {
                        changed = true;
                        if selected {
                            let order = self.server_view.content_bindings.len() as u32;
                            self.server_view.content_bindings.push(ServerAddonBinding {
                                source_id: profile.name.clone(),
                                role: ServerAddonRole::Required,
                                order,
                            });
                        } else {
                            self.server_view
                                .content_bindings
                                .retain(|binding| binding.source_id != profile.name);
                            for (order, binding) in
                                self.server_view.content_bindings.iter_mut().enumerate()
                            {
                                binding.order = order as u32;
                            }
                        }
                    }
                }
                let available = profiles
                    .iter()
                    .map(|profile| profile.name.as_str())
                    .collect::<std::collections::HashSet<_>>();
                let stale = self
                    .server_view
                    .content_bindings
                    .iter()
                    .filter(|binding| !available.contains(binding.source_id.as_str()))
                    .map(|binding| binding.source_id.clone())
                    .collect::<Vec<_>>();
                for name in stale {
                    ui.horizontal(|ui| {
                        ui.label(tr_fmt(
                            "Saved profile is missing: {name}",
                            &[("name", name.clone())],
                        ));
                        if ui
                            .button(tr_fmt("Remove {name}", &[("name", name.clone())]))
                            .clicked()
                        {
                            self.server_view
                                .content_bindings
                                .retain(|binding| binding.source_id != name);
                            for (order, binding) in
                                self.server_view.content_bindings.iter_mut().enumerate()
                            {
                                binding.order = order as u32;
                            }
                            changed = true;
                        }
                    });
                }
                let names = self
                    .server_view
                    .content_bindings
                    .iter()
                    .map(|binding| binding.source_id.clone())
                    .collect::<Vec<_>>();
                let selected_count = self.server_view.content_bindings.len();
                for (index, name) in names.into_iter().enumerate() {
                    let mut move_up = false;
                    let mut move_down = false;
                    if let Some(binding) = self
                        .server_view
                        .content_bindings
                        .iter_mut()
                        .find(|binding| binding.source_id == name)
                    {
                        ui.push_id((field_scope, &name), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(tr_fmt("Role for {name}", &[("name", name.clone())]));
                                egui::ComboBox::from_id_salt((
                                    "server-content-role",
                                    field_scope,
                                    &name,
                                ))
                                .selected_text(tr(match binding.role {
                                    ServerAddonRole::Required => "Required",
                                    ServerAddonRole::Optional => "Optional",
                                    ServerAddonRole::ServerOnly => "Server only",
                                }))
                                .show_ui(ui, |ui| {
                                    for (role, label) in [
                                        (ServerAddonRole::Required, "Required"),
                                        (ServerAddonRole::Optional, "Optional"),
                                        (ServerAddonRole::ServerOnly, "Server only"),
                                    ] {
                                        if ui
                                            .selectable_value(&mut binding.role, role, tr(label))
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                                move_up = ui
                                    .add_enabled(
                                        index > 0,
                                        egui::Button::new(tr_fmt(
                                            "Move {name} up",
                                            &[("name", name.clone())],
                                        )),
                                    )
                                    .clicked();
                                move_down = ui
                                    .add_enabled(
                                        index + 1 < selected_count,
                                        egui::Button::new(tr_fmt(
                                            "Move {name} down",
                                            &[("name", name.clone())],
                                        )),
                                    )
                                    .clicked();
                            })
                        });
                    }
                    if move_up || move_down {
                        let target = if move_up { index - 1 } else { index + 1 };
                        self.server_view.content_bindings.swap(index, target);
                        for (order, binding) in
                            self.server_view.content_bindings.iter_mut().enumerate()
                        {
                            binding.order = order as u32;
                        }
                        changed = true;
                    }
                }
            });
            if changed {
                self.server_view.content_plan = None;
                self.server_view.content_bindings_plan = None;
            }
            let response = ui.add_enabled(
                !dirty && !self.server_view.busy(),
                egui::Button::new(tr("Preview selected content")),
            );
            let clicked = response.clicked();
            anchor = response;
            if clicked {
                let request = content_request(
                    &self.server_view.content_bindings,
                    &instance_id,
                    instance_revision,
                    catalog_revision,
                    sources.fingerprint.clone(),
                );
                let saved_bindings = self.server_view.saved_content_bindings.as_ref();
                let expected_bindings_fingerprint =
                    saved_bindings.and_then(|snapshot| snapshot.fingerprint.clone());
                action = Some(ServerTaskAction::PlanContent {
                    request,
                    save_bindings: saved_bindings.is_some(),
                    expected_bindings_fingerprint,
                });
            }
            if self.server_view.saved_content_bindings.is_some()
                && self.server_view.content_bindings.is_empty()
            {
                ui.label(tr(
                    "Reviewing an empty selection clears saved content bindings.",
                ));
            }
            if let Some(snapshot) = &self.server_view.saved_content_bindings {
                ui.label(tr_fmt(
                    "Saved content binding generation: {generation}",
                    &[(
                        "generation",
                        snapshot
                            .document
                            .as_ref()
                            .map_or(0, |doc| doc.generation)
                            .to_string(),
                    )],
                ));
                ui.label(tr(if snapshot.review_current {
                    "Saved content bindings match current sources."
                } else {
                    "Saved content bindings need review before saving."
                }));
                for issue in &snapshot.issues {
                    ui.label(tr(issue));
                }
                if let Some(document) = &snapshot.document {
                    for entry in &document.bindings {
                        ui.label(tr_fmt(
                            "Reviewed profile {name}: {url} at {path}",
                            &[
                                ("name", entry.profile.name.clone()),
                                ("url", entry.profile.remote_url.clone()),
                                ("path", entry.profile.local_path.display().to_string()),
                            ],
                        ));
                        ui.label(tr_fmt(
                            "Reviewed content folder: {path}",
                            &[("path", entry.folder.path.display().to_string())],
                        ));
                    }
                }
            }
            if self.server_view.content_bindings_plan.is_some() {
                let save = ui.add_enabled(
                    !dirty && !self.server_view.busy(),
                    egui::Button::new(tr("Save reviewed content bindings")),
                );
                if save.clicked() {
                    action = self
                        .server_view
                        .content_bindings_plan
                        .take()
                        .map(ServerTaskAction::SaveContentBindings);
                }
                anchor = save;
            }
        } else {
            ui.label(tr(
                "Refresh server content profiles to load registered sources.",
            ));
        }
        if let Some(plan) = &self.server_view.content_plan {
            ui.label(tr("Content preview is not deployment ready."));
            for binding in &plan.bindings {
                ui.label(tr_fmt(
                    "{profile}: {role}",
                    &[
                        ("profile", binding.profile_name.clone()),
                        (
                            "role",
                            tr(match binding.role {
                                ServerAddonRole::Required => "Required",
                                ServerAddonRole::Optional => "Optional",
                                ServerAddonRole::ServerOnly => "Server only",
                            })
                            .to_owned(),
                        ),
                    ],
                ));
                for path in &binding.addon_paths {
                    let state = tr(match path.state {
                        crate::core::server::ContentSourceState::PresentUnverified => {
                            "Present, not verified"
                        }
                        crate::core::server::ContentSourceState::Missing => "Missing",
                        crate::core::server::ContentSourceState::Unsafe => "Unsafe source",
                        crate::core::server::ContentSourceState::AcquisitionRequired => {
                            "Acquisition required"
                        }
                    });
                    ui.label(tr_fmt(
                        "{source} -> {destination} ({state})",
                        &[
                            ("source", path.source.display().to_string()),
                            ("destination", path.destination.display().to_string()),
                            ("state", state.to_owned()),
                        ],
                    ));
                }
            }
            for blocker in &plan.blockers {
                ui.colored_label(self.color_text_error(), tr(&blocker.message));
            }
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        Some(anchor)
    }
}

fn content_request(
    bindings: &[ServerAddonBinding],
    instance_id: &crate::core::server::ServerInstanceId,
    instance_revision: u64,
    catalog_revision: u64,
    fingerprint: Option<String>,
) -> ContentPlanRequest {
    ContentPlanRequest {
        instance_id: instance_id.clone(),
        expected_instance_revision: instance_revision,
        expected_catalog_revision: catalog_revision,
        content_sources_fingerprint: fingerprint,
        bindings: bindings.to_vec(),
    }
}
