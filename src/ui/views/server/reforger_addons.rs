use crate::core::server::{
    ReforgerAddonChannel, ReforgerAddonLock, ReforgerAddonProvider, ServerDefinitionKind,
};
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_reforger_addons(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        let capabilities = crate::core::game::registry().active().server_capabilities();
        if capabilities.definition_kind != Some(ServerDefinitionKind::Reforger)
            || !capabilities.content_staging
            || !capabilities.lifecycle
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let revision = editor.baseline.revision;
        let dirty = editor.dirty();
        if dirty {
            self.server_view.invalidate_reforger_addon_acquisition();
        }
        let ready = !dirty
            && self.server_view.selected.as_ref() == Some(&id)
            && self.server_view.catalog.as_ref().is_some_and(|catalog| {
                catalog
                    .instances
                    .iter()
                    .any(|instance| instance.id == id && instance.revision == revision)
            });
        if !ready {
            self.server_view.invalidate_reforger_addon_acquisition();
        }
        let busy = self.server_view.busy();

        ui.separator();
        Some(ui.push_id("server-reforger-addon-controls", |ui| {
            ui.heading(tr("Reforger addon revisions"));
            ui.label(tr("Resolve requested addons using the accepted Reforger engine. The review is read-only and starts no process."));
            ui.label(tr("Only the stable addon channel is currently supported."));
            if !ready {
                ui.label(tr("Save the selected Reforger server draft before reviewing addon acquisition."));
            }

            let provider_label = ui.label(tr("Optional scenario provider addon GUID"));
            let provider = ui.add_enabled(
                ready && !busy,
                egui::TextEdit::singleline(&mut self.server_view.reforger_addon_scenario_provider)
                    .hint_text(tr("Optional scenario provider addon GUID"))
                    .desired_width(280.0),
            ).labelled_by(provider_label.id);
            if provider.changed() {
                self.server_view.invalidate_reforger_addon_acquisition();
            }
            ui.label(tr("Leave the provider blank to detect it from the verified resource indexes."));

            let review = ui.horizontal_wrapped(|ui| {
                let review = ui.add_enabled(ready && !busy, egui::Button::new(tr("Review stable Reforger addon acquisition")));
                if review.clicked() {
                    self.server_view.invalidate_reforger_addon_acquisition();
                    let value = self.server_view.reforger_addon_scenario_provider.trim();
                    self.start_server_task(ServerTaskAction::PlanReforgerAddonAcquisition {
                        id: id.clone(),
                        scenario_provider: (!value.is_empty()).then(|| value.to_owned()),
                    });
                }
                if ui.add_enabled(ready && !busy, egui::Button::new(tr("Refresh selected Reforger addon lock"))).clicked() {
                    self.server_view.invalidate_reforger_addon_acquisition();
                    self.start_server_task(ServerTaskAction::LoadReforgerAddonLock(id.clone()));
                }
                review
            }).inner;

            let mut just_opened = false;
            if let Some(plan) = &self.server_view.reforger_addon_plan {
                for addon in &plan.requested {
                    ui.label(tr_fmt("Requested addon: {id}; version: {version}; required: {required}", &[
                        ("id", addon.addon_id.clone()),
                        ("version", addon.version.clone().unwrap_or_else(|| tr("Latest acquisition intent"))),
                        ("required", addon.required.unwrap_or(plan.mods_required_by_default.unwrap_or(true)).to_string()),
                    ]));
                }
                ui.label(tr_fmt("Reforger addon review fingerprint: {fingerprint}", &[ ("fingerprint", plan.fingerprint.clone()) ]));
                ui.label(tr_fmt("Expected saved server revision: {revision}", &[("revision", plan.expected_instance_revision.to_string())]));
                ui.label(tr_fmt("Scenario: {scenario}; channel: {channel}; provider addon: {provider}", &[
                    ("scenario", plan.scenario_id.clone()),
                    ("channel", channel_text(plan.channel)),
                    ("provider", plan.scenario_provider.clone().unwrap_or_else(|| tr("Not specified"))),
                ]));
                ui.label(tr_fmt("Plan effects: writes performed {writes}; process started {process}", &[
                    ("writes", plan.writes_performed.to_string()),
                    ("process", plan.process_started.to_string()),
                ]));
                if !plan.blockers.is_empty() {
                    ui.label(tr("Acquisition blockers"));
                    for blocker in &plan.blockers {
                        ui.label(blocker);
                    }
                }
                if ui.add_enabled(ready && !busy && plan.blockers.is_empty(), egui::Button::new(tr("Review confirmation and acquire Reforger addons"))).clicked() {
                    self.server_view.reforger_addon_confirmation = true;
                    just_opened = true;
                }
            }

            if !ready {
                ui.label(tr("Refresh the selected addon lock after saving the current server draft."));
            } else if !self.server_view.reforger_addon_lock_loaded {
                ui.label(tr("Refresh the selected addon lock to inspect retained revisions."));
            } else if let Some(lock) = &self.server_view.reforger_addon_lock {
                render_lock(ui, lock);
            } else {
                ui.label(tr("No Reforger addon lock is selected."));
            }

            if self.server_view.reforger_addon_confirmation {
                let mut confirm = false;
                let mut cancel = false;
                let plan = self.server_view.reforger_addon_plan.as_ref();
                egui::Modal::new(egui::Id::unique(("reforger-addon-acquisition-confirmation", id.as_str()))).show(ui.ctx(), |ui| {
                    ui.heading(tr("Acquire the reviewed Reforger addon revisions?"));
                    if let Some(plan) = plan {
                        ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                        ui.label(tr_fmt("Scenario: {scenario}; provider addon: {provider}", &[
                            ("scenario", plan.scenario_id.clone()),
                            ("provider", plan.scenario_provider.clone().unwrap_or_else(|| tr("Not specified"))),
                        ]));
                        ui.label(tr("Acquisition copies the engine into a private working directory, starts a local server and downloads the requested addons from Bohemia Workshop. Verified revisions are retained separately from the current deployment."));
                    }
                    ui.horizontal_wrapped(|ui| {
                        let cancel_button = ui.button(tr("Cancel"));
                        if just_opened { cancel_button.request_focus(); }
                        if cancel_button.clicked() { cancel = true; }
                        if ui.add_enabled(plan.is_some_and(|plan| plan.blockers.is_empty()) && ready && !busy, egui::Button::new(tr("Acquire reviewed addon revisions"))).clicked() { confirm = true; }
                    });
                });
                if cancel {
                    self.server_view.invalidate_reforger_addon_acquisition();
                }
                if confirm && let Some(plan) = self.server_view.reforger_addon_plan.take() {
                        self.server_view.reforger_addon_confirmation = false;
                        self.start_server_task(ServerTaskAction::AcquireReforgerAddons(plan));
                }
            }
            review
        }).inner)
    }
}

fn render_lock(ui: &mut Ui, lock: &ReforgerAddonLock) {
    ui.heading(tr("Selected Reforger addon lock"));
    ui.label(tr_fmt(
        "Scenario: {scenario}; channel: {channel}; provider addon: {provider}",
        &[
            ("scenario", lock.scenario_id.clone()),
            ("channel", channel_text(lock.channel)),
            (
                "provider",
                lock.scenario_provider
                    .clone()
                    .unwrap_or_else(|| tr("Not specified")),
            ),
        ],
    ));
    ui.label(tr_fmt(
        "Scenario: {scenario}; channel: {channel}; default mods required: {required}",
        &[
            ("scenario", lock.scenario_id.clone()),
            ("channel", channel_text(lock.channel)),
            (
                "required",
                lock.mods_required_by_default
                    .map_or_else(|| tr("Default (true)"), |value| value.to_string()),
            ),
        ],
    ));
    ui.label(tr_fmt("Addon lock status: degraded {degraded}; requested {requested}; realized {realized}; omitted {omitted}", &[
        ("degraded", lock.degraded.to_string()),
        ("requested", lock.requested.len().to_string()),
        ("realized", lock.realized.len().to_string()),
        ("omitted", lock.omissions.len().to_string()),
    ]));
    for addon in &lock.requested {
        ui.label(tr_fmt(
            "Requested addon: {id}; version: {version}; required: {required}",
            &[
                ("id", addon.addon_id.clone()),
                (
                    "version",
                    addon
                        .version
                        .clone()
                        .unwrap_or_else(|| tr("Latest acquisition intent")),
                ),
                (
                    "required",
                    addon
                        .required
                        .map_or_else(|| tr("Default"), |value| value.to_string()),
                ),
            ],
        ));
    }
    for addon in &lock.realized {
        ui.collapsing(
            tr_fmt(
                "Realized addon {id} version {version}",
                &[
                    ("id", addon.addon_id.clone()),
                    ("version", addon.version.clone()),
                ],
            ),
            |ui| {
                ui.label(tr_fmt(
                    "Addon provider: {provider}",
                    &[("provider", provider_text(addon.provider))],
                ));
                if addon.dependencies.is_empty() {
                    ui.label(tr("No addon dependencies recorded."));
                } else {
                    for dependency in &addon.dependencies {
                        ui.label(tr_fmt(
                            "Addon dependency: {id}; version: {version}",
                            &[
                                ("id", dependency.addon_id.clone()),
                                (
                                    "version",
                                    dependency
                                        .version
                                        .clone()
                                        .unwrap_or_else(|| tr("Version constraint not provided")),
                                ),
                            ],
                        ));
                    }
                }
            },
        );
    }
    for omission in &lock.omissions {
        ui.label(tr_fmt(
            "Requested addon omitted: {id}; reason: {reason}",
            &[
                ("id", omission.addon_id.clone()),
                ("reason", omission.reason.clone()),
            ],
        ));
    }
}

fn channel_text(channel: ReforgerAddonChannel) -> String {
    tr(match channel {
        ReforgerAddonChannel::Stable => "Stable",
        ReforgerAddonChannel::Experimental => "Experimental",
    })
}

fn provider_text(provider: ReforgerAddonProvider) -> String {
    tr(match provider {
        ReforgerAddonProvider::Local => "Local",
        ReforgerAddonProvider::BohemiaWorkshop => "Bohemia Workshop",
    })
}
