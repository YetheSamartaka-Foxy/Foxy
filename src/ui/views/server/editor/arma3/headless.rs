use crate::core::server::{
    Arma3HeadlessClient, HeadlessAddonSelection, HeadlessLifecyclePolicy, HeadlessProcessOverrides,
    MAX_HEADLESS_CLIENTS, ServerAddonRole, new_headless_id, selects_headless_source,
};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

use super::{
    HeadlessProfiles,
    forms::{self, Form},
};

pub(super) fn render(
    ui: &mut Ui,
    form: Form,
    clients: &mut Vec<Arma3HeadlessClient>,
    field_scope: egui::Id,
    profiles: HeadlessProfiles<'_>,
) {
    ui.heading(tr("Headless clients"));
    ui.label(tr("Headless clients run this server installation in client mode, connect from 127.0.0.1 and start only after the server answers its query port. Foxy does not verify mission slots or AI offload."));
    let mut remove = None;
    for (index, client) in clients.iter_mut().enumerate() {
        let scope = field_scope.with(("headless", client.id.as_str()));
        ui.push_id(("headless", client.id.as_str()), |ui| {
            ui.group(|ui| {
                ui.horizontal_wrapped(|ui| {
                    let name = ui.label(tr("Headless client name"));
                    ui.add(
                        egui::TextEdit::singleline(&mut client.name)
                            .desired_width(180.0)
                            .char_limit(24),
                    )
                    .labelled_by(name.id);
                    let order = ui.label(tr("Start order"));
                    ui.add(egui::DragValue::new(&mut client.order).range(0..=u32::MAX))
                        .labelled_by(order.id);
                    ui.checkbox(&mut client.enabled, tr("Enabled"));
                    ui.weak(client.id.as_str());
                    if ui.button(tr("Remove headless client")).clicked() {
                        remove = Some(index);
                    }
                });
                ui.checkbox(
                    &mut client.addons.inherit_required,
                    tr("Load the server's required addons"),
                )
                .on_hover_text(tr("Adds the server's Required sources in their configured order. Optional and Server-only sources are not inherited."));
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr("Also load sources"));
                    if let Some(sources) = profiles.registered {
                        source_chooser(ui, &mut client.addons.include, sources, profiles.reviewed, scope.with("include-choice"));
                    } else {
                        ui.label(tr("Open Addons and load the registered profiles to choose them here."));
                    }
                    forms::edit_source_list(
                        ui,
                        form,
                        &mut client.addons.include,
                        scope.with("include"),
                        label.id,
                    );
                });
                ui.add_enabled_ui(client.addons.inherit_required, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label(tr("Skip required sources"));
                        forms::edit_source_list(
                            ui,
                            form,
                            &mut client.addons.exclude,
                            scope.with("exclude"),
                            label.id,
                        );
                    });
                    ui.label(tr("Excluded sources are removed from inherited Required addons."));
                });
                if let Some(reviewed) = profiles.reviewed {
                    ui.collapsing(tr("Headless addon preview"), |ui| {
                        ui.label(tr("Calculated from the last saved loadout and the current headless client settings."));
                        let mut any = false;
                        for entry in &reviewed.bindings {
                            let binding = &entry.binding;
                            if binding.role == ServerAddonRole::Optional || !selects_headless_source(
                                &client.addons,
                                &binding.source_id,
                                binding.role == ServerAddonRole::Required,
                            ) {
                                continue;
                            }
                            any = true;
                            let explicit = client.addons.include.iter().any(|name|
                                name.eq_ignore_ascii_case(&binding.source_id)
                            );
                            ui.label(tr_fmt(if explicit {
                                "Explicit source: {name}"
                            } else {
                                "Inherited source: {name}"
                            }, &[("name", entry.profile.name.clone())]));
                            for name in &entry.profile.addons {
                                ui.label(name);
                            }
                            if entry.profile.addons.is_empty() {
                                ui.label(tr("Uses the entire source folder."));
                            }
                        }
                        if !any {
                            ui.weak(tr("This headless client selects no sources from the saved loadout."));
                        }
                    });
                } else {
                    ui.weak(tr("Save a current loadout under Addons to preview inherited sources."));
                }
                overrides(ui, form, &mut client.process, scope);
                lifecycle(ui, &mut client.lifecycle);
            });
        });
    }
    if let Some(index) = remove {
        clients.remove(index);
        forms::clear_edit_buffers(ui.ctx());
    }
    if ui
        .add_enabled(
            clients.len() < MAX_HEADLESS_CLIENTS,
            egui::Button::new(tr("Add headless client")),
        )
        .clicked()
    {
        let name = (1..=MAX_HEADLESS_CLIENTS + 1)
            .map(|number| format!("HC{number}"))
            .find(|name| {
                !clients
                    .iter()
                    .any(|client| client.name.eq_ignore_ascii_case(name))
            })
            .unwrap_or_else(|| "HC".into());
        clients.push(Arma3HeadlessClient {
            id: new_headless_id(),
            name,
            order: u32::try_from(clients.len()).unwrap_or(u32::MAX),
            enabled: true,
            addons: HeadlessAddonSelection::default(),
            process: HeadlessProcessOverrides::default(),
            lifecycle: HeadlessLifecyclePolicy::default(),
        });
    }
}

fn source_chooser(
    ui: &mut Ui,
    included: &mut Vec<String>,
    sources: &crate::core::server::ContentSources,
    reviewed: Option<&crate::core::server::ContentBindingsDocument>,
    id: egui::Id,
) {
    if sources.profiles.is_empty() {
        ui.weak(tr("No registered content profiles are loaded."));
        return;
    }
    let mut selected = String::new();
    egui::ComboBox::from_id_salt(id)
        .selected_text(tr("Choose a loaded source"))
        .show_ui(ui, |ui| {
            for profile in &sources.profiles {
                let already_included = included
                    .iter()
                    .any(|source| source.eq_ignore_ascii_case(&profile.name));
                let eligible = reviewed.is_none_or(|document| document.bindings.iter().any(|entry|
                    entry.binding.source_id == profile.name && entry.binding.role != ServerAddonRole::Optional
                ));
                if ui
                    .add_enabled(!already_included && eligible, egui::Button::new(&profile.name))
                    .on_hover_text(tr("Adds this registered content profile to this headless client's include list."))
                    .on_disabled_hover_text(tr("Select a Required or Server-only profile from this server's loadout that is not already included."))
                    .clicked()
                {
                    selected.clone_from(&profile.name);
                }
            }
        });
    if !selected.is_empty() {
        included.push(selected);
    }
}

fn overrides(ui: &mut Ui, form: Form, process: &mut HeadlessProcessOverrides, scope: egui::Id) {
    ui.horizontal_wrapped(|ui| {
        let mut cpu = process.cpu_count.map(u64::from);
        let label = ui.label(tr("CPU count"));
        if forms::edit_optional_integer(ui, form, &mut cpu, (scope.with("cpu"), label.id), |v| {
            (1..=256).contains(&v)
        }) {
            process.cpu_count = cpu.and_then(|value| u16::try_from(value).ok());
        }
        let mut threads = process.ex_threads.map(u64::from);
        let label = ui.label(tr("Extra threads"));
        if forms::edit_optional_integer(
            ui,
            form,
            &mut threads,
            (scope.with("threads"), label.id),
            |v| [0, 1, 3, 5, 7].contains(&v),
        ) {
            process.ex_threads = threads.and_then(|value| u8::try_from(value).ok());
        }
    });
    ui.horizontal_wrapped(|ui| {
        let mut memory = process.max_mem.map(u64::from);
        let label = ui.label(tr("Memory limit (MB)"));
        if forms::edit_optional_integer(
            ui,
            form,
            &mut memory,
            (scope.with("memory"), label.id),
            |v| (256..=262_144).contains(&v),
        ) {
            process.max_mem = memory.and_then(|value| u32::try_from(value).ok());
        }
        let mut fps = process.limit_fps.map(u64::from);
        let label = ui.label(tr("FPS limit"));
        if forms::edit_optional_integer(ui, form, &mut fps, (scope.with("fps"), label.id), |v| {
            (5..=1000).contains(&v)
        }) {
            process.limit_fps = fps.and_then(|value| u16::try_from(value).ok());
        }
    });
}

fn lifecycle(ui: &mut Ui, policy: &mut HeadlessLifecyclePolicy) {
    ui.checkbox(
        &mut policy.auto_start,
        tr("Start automatically once the server answers its query port"),
    );
    ui.horizontal_wrapped(|ui| {
        let label = ui.label(tr("Start delay (seconds)"));
        ui.add(egui::DragValue::new(&mut policy.start_delay_seconds).range(0..=600))
            .labelled_by(label.id);
        let label = ui.label(tr("Join timeout (seconds)"));
        ui.add(egui::DragValue::new(&mut policy.connect_timeout_seconds).range(15..=900))
            .labelled_by(label.id);
        let label = ui.label(tr("Automatic restarts in 10 minutes"));
        ui.add(egui::DragValue::new(&mut policy.max_restarts).range(0..=10))
            .labelled_by(label.id);
    });
    ui.label(tr("After an unexpected exit Foxy restarts the client with a growing delay. Once the restart limit is reached it stops trying until you start the client again. 0 turns automatic restart off."));
}
