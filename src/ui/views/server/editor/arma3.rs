use crate::core::server::{
    Arma3BasicSettings, Arma3Difficulty, Arma3Mission, Arma3MissionSource, Arma3ServerSettings,
    SignatureVerification,
};
use crate::ui::i18n::tr;
use eframe::egui::{self, Ui};

mod forms;
mod headless;

pub(super) fn has_invalid_edits(ctx: &egui::Context, scope: egui::Id) -> bool {
    forms::has_invalid_edits(ctx, scope)
}

pub(super) fn clear_edit_buffers(ctx: &egui::Context) {
    forms::clear_edit_buffers(ctx);
}

pub(super) fn render(
    ui: &mut Ui,
    settings: &mut crate::core::server::Arma3ServerDefinition,
    keys: &mut Vec<u64>,
    next_key: &mut u64,
    field_scope: egui::Id,
    headless_clients: bool,
) {
    let form = forms::Form { root: field_scope };
    super::text(
        ui,
        "Server hostname",
        field_scope.with("hostname"),
        &mut settings.hostname,
    );
    super::number(ui, "Game port", &mut settings.game_port, u16::MAX);
    super::number(ui, "Maximum players", &mut settings.max_players, u16::MAX);
    ui.checkbox(&mut settings.persistent, tr("Persistent mission"));
    ui.checkbox(
        &mut settings.auto_init,
        tr("Initialize mission automatically"),
    );
    ui.checkbox(&mut settings.battleye, tr("Enable BattlEye"));
    let mut signatures = settings.verify_signatures == SignatureVerification::V2;
    if ui
        .checkbox(&mut signatures, tr("Verify addon signatures (v2)"))
        .changed()
    {
        settings.verify_signatures = if signatures {
            SignatureVerification::V2
        } else {
            SignatureVerification::Disabled
        };
    }
    ui.heading(tr("Mission cycle"));
    let mission_count = settings.mission_cycle.len();
    let mut remove = None;
    let mut move_mission = None;
    for (index, mission) in settings.mission_cycle.iter_mut().enumerate() {
        ui.push_id(("mission", keys[index]), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut mission.enabled, tr("Enabled"));
                let source_label = match &mission.source {
                    Some(Arma3MissionSource::Library {
                        mission_id,
                        revision_id,
                    }) => {
                        format!("{} ({mission_id} / {revision_id})", tr("Library revision"))
                    }
                    Some(Arma3MissionSource::AddonClass) => tr("Addon class mission").to_owned(),
                    None => tr("File mission template").to_owned(),
                };
                ui.label(source_label);
                if !matches!(mission.source, Some(Arma3MissionSource::Library { .. })) {
                    let mut addon_class =
                        matches!(mission.source, Some(Arma3MissionSource::AddonClass));
                    if ui
                        .checkbox(&mut addon_class, tr("Addon class mission"))
                        .changed()
                    {
                        mission.source = addon_class.then_some(Arma3MissionSource::AddonClass);
                    }
                }
                let label = ui.label(tr("Mission template"));
                ui.add_enabled(
                    !matches!(mission.source, Some(Arma3MissionSource::Library { .. })),
                    egui::TextEdit::singleline(&mut mission.template),
                )
                .labelled_by(label.id);
                let difficulty = ui.label(tr("Difficulty"));
                egui::ComboBox::from_id_salt("difficulty")
                    .selected_text(tr(difficulty_label(mission.difficulty)))
                    .show_ui(ui, |ui| {
                        for difficulty in [
                            Arma3Difficulty::Recruit,
                            Arma3Difficulty::Regular,
                            Arma3Difficulty::Veteran,
                            Arma3Difficulty::Custom,
                        ] {
                            ui.selectable_value(
                                &mut mission.difficulty,
                                difficulty,
                                tr(difficulty_label(difficulty)),
                            );
                        }
                    })
                    .response
                    .labelled_by(difficulty.id);
                if ui.button(tr("Remove mission")).clicked() {
                    remove = Some(index);
                }
                if index > 0 && ui.button(tr("Move mission up")).clicked() {
                    move_mission = Some((index, index - 1));
                }
                if index + 1 < mission_count && ui.button(tr("Move mission down")).clicked() {
                    move_mission = Some((index, index + 1));
                }
            });
            forms::render_params(
                ui,
                form,
                &mut mission.params,
                field_scope.with(("mission-params", keys[index])),
            );
        });
    }
    if let Some(index) = remove {
        settings.mission_cycle.remove(index);
        keys.remove(index);
        forms::clear_edit_buffers(ui.ctx());
    } else if let Some((from, to)) = move_mission {
        settings.mission_cycle.swap(from, to);
        keys.swap(from, to);
        forms::clear_edit_buffers(ui.ctx());
    }
    if ui.button(tr("Add mission")).clicked() {
        settings.mission_cycle.push(Arma3Mission {
            template: String::new(),
            difficulty: Arma3Difficulty::Regular,
            params: Default::default(),
            enabled: true,
            source: None,
        });
        keys.push(*next_key);
        *next_key += 1;
    }
    if headless_clients {
        ui.separator();
        headless::render(
            ui,
            form,
            &mut settings.headless_clients,
            field_scope.with("headless"),
        );
    }

    ui.separator();
    ui.heading(tr("Configuration files"));
    ui.label(tr(
        "Unchecked settings are omitted from the generated files, so the engine keeps its own defaults.",
    ));
    forms::render_configuration(
        ui,
        form,
        &mut settings.configuration,
        field_scope.with("configuration"),
    );
    ui.collapsing(tr("Basic network tuning"), |ui| {
        forms::render_section(
            ui,
            form,
            &mut settings.basic,
            Arma3BasicSettings::FIELDS,
            field_scope.with("basic"),
        );
    });
    ui.collapsing(tr("Server configuration overrides"), |ui| {
        forms::render_section(
            ui,
            form,
            &mut settings.server,
            Arma3ServerSettings::FIELDS,
            field_scope.with("server"),
        );
    });
    ui.collapsing(tr("Custom difficulty"), |ui| {
        forms::render_difficulty(
            ui,
            form,
            &mut settings.difficulty,
            field_scope.with("difficulty"),
        );
    });
}

fn difficulty_label(value: Arma3Difficulty) -> &'static str {
    match value {
        Arma3Difficulty::Recruit => "Recruit",
        Arma3Difficulty::Regular => "Regular",
        Arma3Difficulty::Veteran => "Veteran",
        Arma3Difficulty::Custom => "Custom",
    }
}
