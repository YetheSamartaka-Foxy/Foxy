use crate::core::server::ReforgerAddonRequirement;
use crate::ui::i18n::tr;
use eframe::egui::{self, Ui};

pub(super) fn render(
    ui: &mut Ui,
    settings: &mut crate::core::server::ReforgerServerDefinition,
    keys: &mut Vec<u64>,
    next_key: &mut u64,
    field_scope: egui::Id,
) {
    super::text(
        ui,
        "Server name",
        field_scope.with("hostname"),
        &mut settings.name,
    );
    super::number(ui, "Bind port", &mut settings.bind_port, u16::MAX);
    let mut custom_address = settings.bind_address.is_some();
    if ui
        .checkbox(&mut custom_address, tr("Use explicit IPv4 bind address"))
        .changed()
    {
        settings.bind_address = custom_address.then_some(std::net::Ipv4Addr::LOCALHOST);
    }
    if let Some(address) = &mut settings.bind_address {
        let mut octets = address.octets();
        ui.horizontal_wrapped(|ui| {
            for (index, label) in [
                "IPv4 octet 1",
                "IPv4 octet 2",
                "IPv4 octet 3",
                "IPv4 octet 4",
            ]
            .into_iter()
            .enumerate()
            {
                ui.push_id(field_scope.with(("bind-address", index)), |ui| {
                    let label = ui.label(tr(label));
                    ui.add(egui::DragValue::new(&mut octets[index]).range(0..=255))
                        .labelled_by(label.id);
                });
            }
        });
        *address = std::net::Ipv4Addr::from(octets);
    }
    super::number(ui, "Maximum players", &mut settings.max_players, 128);
    ui.checkbox(&mut settings.visible, tr("List server publicly"));
    let mut query = settings.a2s_port.is_some();
    if ui
        .checkbox(&mut query, tr("Enable A2S query port"))
        .changed()
    {
        settings.a2s_port = query.then_some(17777);
    }
    if let Some(port) = &mut settings.a2s_port {
        super::number(ui, "A2S query port", port, u16::MAX);
    }
    let mut scenario = settings.scenario_id.clone().unwrap_or_default();
    super::text(
        ui,
        "Scenario resource",
        field_scope.with("scenario"),
        &mut scenario,
    );
    settings.scenario_id = (!scenario.is_empty()).then_some(scenario);
    ui.label(tr(
        "Use the full {GUID}path.conf resource name; an empty scenario is allowed in a draft.",
    ));
    ui.label(tr(
        "A scenario resource name does not prove that the scenario is installed.",
    ));
    scenario_end(
        ui,
        &mut settings.scenario_end,
        field_scope.with("scenario-end"),
    );
    ui.heading(tr("Advanced Reforger fields"));
    tri_state(
        ui,
        "Cross-platform",
        field_scope.with("cross-platform"),
        &mut settings.cross_platform,
    );
    tri_state(
        ui,
        "Mods required by default",
        field_scope.with("mods-required"),
        &mut settings.mods_required_by_default,
    );
    optional_distance(
        ui,
        "Set server max view distance",
        &mut settings.game_properties.server_max_view_distance,
    );
    optional_distance(
        ui,
        "Set server min grass distance",
        &mut settings.game_properties.server_min_grass_distance,
    );
    optional_distance(
        ui,
        "Set network view distance",
        &mut settings.game_properties.network_view_distance,
    );
    tri_state(
        ui,
        "Fast validation",
        field_scope.with("fast-validation"),
        &mut settings.game_properties.fast_validation,
    );
    tri_state(
        ui,
        "BattlEye",
        field_scope.with("battleye"),
        &mut settings.game_properties.battleye,
    );
    tri_state(
        ui,
        "Disable third person",
        field_scope.with("third-person"),
        &mut settings.game_properties.disable_third_person,
    );
    ui.heading(tr("Addon intent"));
    let mut remove = None;
    for (index, addon) in settings.addons.iter_mut().enumerate() {
        ui.push_id(("addon", keys[index]), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Addon GUID"));
                ui.text_edit_singleline(&mut addon.addon_id);
                let mut version = addon.version.clone().unwrap_or_default();
                ui.label(tr("Version (optional)"));
                ui.text_edit_singleline(&mut version);
                addon.version = (!version.is_empty()).then_some(version);
                egui::ComboBox::from_id_salt("required")
                    .selected_text(tr(required_label(addon.required)))
                    .show_ui(ui, |ui| {
                        for required in [None, Some(false), Some(true)] {
                            ui.selectable_value(
                                &mut addon.required,
                                required,
                                tr(required_label(required)),
                            );
                        }
                    });
                if ui.button(tr("Remove addon intent")).clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        settings.addons.remove(index);
        keys.remove(index);
    }
    if ui.button(tr("Add addon intent")).clicked() {
        settings.addons.push(ReforgerAddonRequirement {
            addon_id: String::new(),
            version: None,
            required: None,
            preserved: Default::default(),
        });
        keys.push(*next_key);
        *next_key += 1;
    }
}

fn scenario_end(ui: &mut Ui, policy: &mut crate::core::server::ReforgerScenarioEnd, id: egui::Id) {
    use crate::core::server::ReforgerScenarioEnd;
    let mut mode = match policy {
        ReforgerScenarioEnd::Shutdown { .. } => 0,
        ReforgerScenarioEnd::Reload { .. } => 1,
        ReforgerScenarioEnd::Disabled => 2,
    };
    let mut delay = match policy {
        ReforgerScenarioEnd::Shutdown { delay_seconds }
        | ReforgerScenarioEnd::Reload { delay_seconds } => *delay_seconds,
        ReforgerScenarioEnd::Disabled => 0,
    };
    ui.horizontal_wrapped(|ui| {
        let label = ui.label(tr("When the scenario ends"));
        egui::ComboBox::from_id_salt(id)
            .selected_text(tr(match mode {
                0 => "Shut down the process",
                1 => "Reload the scenario",
                _ => "Disable automatic scenario reload",
            }))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut mode, 0, tr("Shut down the process"));
                ui.selectable_value(&mut mode, 1, tr("Reload the scenario"));
                ui.selectable_value(&mut mode, 2, tr("Disable automatic scenario reload"));
            })
            .response
            .labelled_by(label.id);
    });
    if mode != 2 {
        ui.horizontal(|ui| {
            let label = ui.label(tr("Scenario-end delay (seconds)"));
            ui.add(egui::DragValue::new(&mut delay).range(0..=3600))
                .labelled_by(label.id);
        });
    }
    *policy = match mode {
        0 => ReforgerScenarioEnd::Shutdown {
            delay_seconds: delay,
        },
        1 => ReforgerScenarioEnd::Reload {
            delay_seconds: delay,
        },
        _ => ReforgerScenarioEnd::Disabled,
    };
    ui.label(tr("Scenario reload keeps the owned process. Shutdown does not automatically start another process. Disabled reload retains the shutdown safeguard."));
}

fn tri_state(ui: &mut Ui, label: &str, id: egui::Id, value: &mut Option<bool>) {
    ui.horizontal_wrapped(|ui| {
        let caption = ui.label(tr(label));
        egui::ComboBox::from_id_salt(id)
            .selected_text(tr(tri_label(*value)))
            .show_ui(ui, |ui| {
                for choice in [None, Some(false), Some(true)] {
                    ui.selectable_value(value, choice, tr(tri_label(choice)));
                }
            })
            .response
            .labelled_by(caption.id);
    });
}

fn tri_label(value: Option<bool>) -> &'static str {
    match value {
        None => "Leave unset",
        Some(false) => "Explicit false",
        Some(true) => "Explicit true",
    }
}

fn optional_distance(ui: &mut Ui, label: &str, value: &mut Option<u32>) {
    ui.horizontal_wrapped(|ui| {
        let mut enabled = value.is_some();
        if ui.checkbox(&mut enabled, tr(label)).changed() {
            *value = enabled.then_some(0);
        }
        if let Some(distance) = value {
            let caption = ui.label(tr("Distance meters"));
            ui.add(egui::DragValue::new(distance).range(0..=100_000))
                .labelled_by(caption.id);
        }
    });
}

fn required_label(value: Option<bool>) -> &'static str {
    match value {
        None => "Engine default",
        Some(false) => "Not required",
        Some(true) => "Required",
    }
}
