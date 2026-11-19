use crate::core::server::{
    ARMA3_AI_PRESETS, ARMA3_CHANNELS, ARMA3_DEFAULT_BASIC_CONFIG, ARMA3_DEFAULT_PROFILE,
    ARMA3_DEFAULT_SERVER_CONFIG, ARMA3_KICK_CAUSES, Arma3ConfigurationNames, Arma3CustomAi,
    Arma3DifficultyOptions, Arma3DifficultyProfile, Arma3Field, Arma3FieldKind, Arma3ListKind,
    parse_decimal_input,
};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, HashMap};

/// Typed text of a field while the operator edits it; the model only receives valid values.
#[derive(Clone)]
struct FieldText(String);

/// Keep invalid fields recorded while hidden or collapsed so Save remains blocked until they are fixed or cleared.
fn invalid_fields() -> egui::Id {
    egui::Id::NULL.with("arma3-invalid-fields")
}

pub(super) fn has_invalid_edits(ctx: &egui::Context, scope: egui::Id) -> bool {
    ctx.data(|data| {
        data.get_temp::<HashMap<egui::Id, egui::Id>>(invalid_fields())
            .is_some_and(|ids| ids.values().any(|root| *root == scope))
    })
}

pub(super) fn clear_edit_buffers(ctx: &egui::Context) {
    ctx.data_mut(|data| {
        data.remove_by_type::<FieldText>();
        data.remove::<HashMap<egui::Id, egui::Id>>(invalid_fields());
    });
}

#[derive(Clone, Copy)]
pub(super) struct Form {
    pub root: egui::Id,
}

impl Form {
    fn mark(self, ui: &Ui, id: egui::Id, invalid: bool) {
        ui.ctx().data_mut(|data| {
            let ids = data.get_temp_mut_or_default::<HashMap<egui::Id, egui::Id>>(invalid_fields());
            if invalid {
                ids.insert(id, self.root);
            } else {
                ids.remove(&id);
            }
        });
    }
}

pub(super) fn render_section<T: Serialize + DeserializeOwned>(
    ui: &mut Ui,
    form: Form,
    value: &mut T,
    fields: &[Arma3Field],
    scope: egui::Id,
) {
    let mut model = serde_json::to_value(&*value)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let changed = ui
        .push_id(scope, |ui| {
            render_fields(ui, form, &mut model, fields, scope)
        })
        .inner;
    if changed && let Ok(next) = serde_json::from_value(Value::Object(model)) {
        *value = next;
    }
}

pub(super) fn render_difficulty(
    ui: &mut Ui,
    form: Form,
    profile: &mut Option<Box<Arma3DifficultyProfile>>,
    scope: egui::Id,
) {
    let mut enabled = profile.is_some();
    if ui
        .checkbox(&mut enabled, tr("Manage the dedicated custom difficulty profile"))
        .on_hover_text(tr(
            "Writes DifficultyPresets into this instance's own server profile before each start. Missions use it only with Custom difficulty.",
        ))
        .changed()
    {
        *profile = enabled.then(Box::default);
    }
    let Some(profile) = profile.as_deref_mut() else {
        return;
    };
    ui.indent(scope.with("difficulty-indent"), |ui| {
        render_section(
            ui,
            form,
            &mut profile.options,
            Arma3DifficultyOptions::FIELDS,
            scope.with("options"),
        );
        let previous = profile.ai_level_preset;
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(tr("AI level preset"));
            egui::ComboBox::from_id_salt(scope.with("ai-preset"))
                .selected_text(ai_preset_label(profile.ai_level_preset))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut profile.ai_level_preset, None, tr("Engine default"));
                    for (value, name) in ARMA3_AI_PRESETS {
                        ui.selectable_value(
                            &mut profile.ai_level_preset,
                            Some(*value),
                            format!("{value}: {}", tr(name)),
                        );
                    }
                })
                .response
                .labelled_by(label.id);
        });
        if previous != profile.ai_level_preset && profile.ai_level_preset != Some(3) {
            profile.custom_ai = Arma3CustomAi::default();
        }
        if profile.ai_level_preset == Some(3) {
            render_section(
                ui,
                form,
                &mut profile.custom_ai,
                Arma3CustomAi::FIELDS,
                scope.with("custom-ai"),
            );
        }
    });
}

fn ai_preset_label(value: Option<u8>) -> String {
    match value.and_then(|value| ARMA3_AI_PRESETS.iter().find(|(known, _)| *known == value)) {
        Some((value, name)) => format!("{value}: {}", tr(name)),
        None => tr("Engine default"),
    }
}

pub(super) fn render_configuration(
    ui: &mut Ui,
    form: Form,
    value: &mut Box<Arma3ConfigurationNames>,
    scope: egui::Id,
) {
    for (label, field, default) in [
        (
            "Server configuration filename",
            &mut value.server,
            ARMA3_DEFAULT_SERVER_CONFIG,
        ),
        (
            "Basic configuration filename",
            &mut value.basic,
            ARMA3_DEFAULT_BASIC_CONFIG,
        ),
        (
            "Server profile name",
            &mut value.profile_name,
            ARMA3_DEFAULT_PROFILE,
        ),
    ] {
        let id = scope.with(label);
        let mut present = field.is_some();
        let toggle = ui.horizontal_wrapped(|ui| {
            let toggle = ui
                .checkbox(&mut present, tr(label))
                .on_hover_text(tr("Leave unchecked to keep the engine default."));
            if label == "Server profile name" {
                toggle.clone().on_hover_text(tr("Names this instance's Arma engine profile directory for server configuration and difficulty. It is not a Foxy content profile or addon set."));
            }
            if field.is_none() {
                ui.weak(tr_fmt("Default: {name}", &[("name", default.to_owned())]));
            }
            toggle
        });
        if toggle.inner.changed() {
            *field = present.then(|| default.to_owned());
            forget(ui, id);
        }
        if let Some(text) = field.as_mut() {
            let mut current = Value::String(text.clone());
            ui.indent(id, |ui| {
                if edit_text(ui, form, &mut current, id, toggle.inner.id, |text| {
                    !text.is_empty() && text.len() <= 64
                }) && let Value::String(next) = current
                {
                    *text = next;
                }
            });
        }
    }
}

pub(super) fn render_params(
    ui: &mut Ui,
    form: Form,
    params: &mut BTreeMap<String, Number>,
    scope: egui::Id,
) {
    ui.label(tr("Mission parameters"));
    ui.indent(scope.with("params-indent"), |ui| {
        let entries = params
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        let mut rename = None;
        let mut remove = None;
        for (index, (key, number)) in entries.iter().enumerate() {
            ui.push_id(("param", index), |ui| {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr("Parameter name"));
                    let key_id = scope.with(("param-key", index));
                    let mut text = buffer(ui, key_id).unwrap_or_else(|| key.clone());
                    let response = ui
                        .add(
                            egui::TextEdit::singleline(&mut text)
                                .id(key_id)
                                .desired_width(160.0),
                        )
                        .labelled_by(label.id);
                    let valid =
                        parameter_name(&text) && (text == *key || !params.contains_key(&text));
                    if response.changed() {
                        store(ui, key_id, text.clone());
                    }
                    form.mark(ui, key_id, !valid);
                    if !valid {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            tr("Use a unique name starting with a letter or underscore."),
                        );
                    } else if response.lost_focus() && text != *key {
                        rename = Some((key.clone(), text));
                        forget(ui, key_id);
                    }
                    let value_label = ui.label(tr("Value"));
                    let mut value = Value::Number(number.clone());
                    if edit_decimal(
                        ui,
                        form,
                        &mut value,
                        scope.with(("param-value", index)),
                        value_label.id,
                        f64::MIN,
                        f64::MAX,
                    ) && let Value::Number(next) = value
                    {
                        params.insert(key.clone(), next);
                    }
                    if ui.button(tr("Remove parameter")).clicked() {
                        remove = Some(key.clone());
                    }
                });
            });
        }
        if let Some((old, new)) = rename
            && let Some(value) = params.remove(&old)
        {
            params.insert(new, value);
        }
        if let Some(key) = remove {
            params.remove(&key);
            for index in 0..entries.len() {
                forget(ui, scope.with(("param-key", index)));
                forget(ui, scope.with(("param-value", index)));
            }
        }
        if ui.button(tr("Add mission parameter")).clicked() {
            let mut name = "parameter".to_owned();
            let mut suffix = 2;
            while params.contains_key(&name) {
                name = format!("parameter{suffix}");
                suffix += 1;
            }
            params.insert(name, Number::from(0));
        }
    });
}

fn parameter_name(key: &str) -> bool {
    key.len() <= 64
        && key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn render_fields(
    ui: &mut Ui,
    form: Form,
    map: &mut Map<String, Value>,
    fields: &[Arma3Field],
    scope: egui::Id,
) -> bool {
    let mut changed = false;
    for field in fields {
        let key = field.json_name;
        let id = scope.with(key);
        if matches!(field.kind, Arma3FieldKind::Bool) {
            changed |= render_bool(ui, map, field, id);
            continue;
        }
        let mut present = map.contains_key(key);
        let toggle = ui
            .horizontal_wrapped(|ui| {
                let toggle = ui
                    .checkbox(&mut present, tr(field.label))
                    .on_hover_text(tr("Leave unchecked to keep the engine default."));
                since_hint(ui, field);
                toggle
            })
            .inner;
        if toggle.changed() {
            if present {
                map.insert(key.to_owned(), default_value(field.kind));
            } else {
                map.remove(key);
            }
            forget(ui, id);
            changed = true;
        }
        if let Some(value) = map.get_mut(key) {
            ui.indent(id, |ui| {
                changed |= render_value(ui, form, value, field, id, toggle.id);
            });
        }
    }
    changed
}

fn since_hint(ui: &mut Ui, field: &Arma3Field) {
    if field.since != 0 {
        ui.weak(tr_fmt(
            "Requires Arma 3 {version} or newer",
            &[(
                "version",
                format!("{}.{:02}", field.since / 100, field.since % 100),
            )],
        ));
    }
}

fn render_bool(
    ui: &mut Ui,
    map: &mut Map<String, Value>,
    field: &Arma3Field,
    id: egui::Id,
) -> bool {
    let inverted = field.json_name == "disable_von";
    let current = map
        .get(field.json_name)
        .and_then(Value::as_bool)
        .map(|value| value ^ inverted);
    let mut selected = current;
    ui.horizontal_wrapped(|ui| {
        let label = ui.label(tr(field.label));
        egui::ComboBox::from_id_salt(id)
            .selected_text(bool_label(selected))
            .show_ui(ui, |ui| {
                for option in [None, Some(true), Some(false)] {
                    ui.selectable_value(&mut selected, option, bool_label(option));
                }
            })
            .response
            .labelled_by(label.id);
        since_hint(ui, field);
    });
    if selected == current {
        return false;
    }
    match selected {
        Some(value) => map.insert(field.json_name.to_owned(), Value::Bool(value ^ inverted)),
        None => map.remove(field.json_name),
    };
    true
}

fn bool_label(value: Option<bool>) -> String {
    tr(match value {
        None => "Engine default",
        Some(true) => "Enabled",
        Some(false) => "Disabled",
    })
}

fn default_value(kind: Arma3FieldKind) -> Value {
    match kind {
        Arma3FieldKind::Bool => Value::Bool(false),
        Arma3FieldKind::Integer { min, max } => {
            Value::from(min.max(0).min(i64::try_from(max).unwrap_or(i64::MAX)))
        }
        Arma3FieldKind::Decimal { min, .. } => Number::from_f64(min.max(0.0))
            .map(Value::Number)
            .unwrap_or_else(|| Value::from(0)),
        Arma3FieldKind::Text | Arma3FieldKind::SecretRef => Value::String(String::new()),
        Arma3FieldKind::Difficulty => Value::String("regular".into()),
        Arma3FieldKind::Strings(_)
        | Arma3FieldKind::KickTimeouts
        | Arma3FieldKind::VoteCommands
        | Arma3FieldKind::VotedAdminCommands
        | Arma3FieldKind::Channels => Value::Array(Vec::new()),
        Arma3FieldKind::Pair => Value::Array(vec![Value::from(0), Value::from(0)]),
        Arma3FieldKind::SlowNetwork => Value::Array(vec![Value::Bool(false); 4]),
        Arma3FieldKind::Choice(choices) => Value::String(
            choices
                .first()
                .map(|choice| choice.0)
                .unwrap_or_default()
                .into(),
        ),
        Arma3FieldKind::Section(_) => Value::Object(Map::new()),
    }
}

fn render_value(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    field: &Arma3Field,
    id: egui::Id,
    label: egui::Id,
) -> bool {
    match field.kind {
        Arma3FieldKind::Bool => false,
        Arma3FieldKind::Integer { min, max } => edit_integer(ui, form, value, id, label, min, max),
        Arma3FieldKind::Decimal { min, max } => edit_decimal(ui, form, value, id, label, min, max),
        Arma3FieldKind::Text => edit_text(ui, form, value, id, label, |_| true),
        Arma3FieldKind::SecretRef => {
            ui.label(tr(
                "Enter a protected credential reference, not a password. Native activation stays blocked until protected credential binding is available.",
            ));
            edit_text(ui, form, value, id, label, secret_reference)
        }
        Arma3FieldKind::Difficulty => choice(
            ui,
            value,
            id,
            label,
            &[
                ("recruit", "Recruit"),
                ("regular", "Regular"),
                ("veteran", "Veteran"),
                ("custom", "Custom"),
            ],
        ),
        Arma3FieldKind::Choice(choices) => choice(ui, value, id, label, choices),
        Arma3FieldKind::Strings(list) => render_strings(ui, form, value, list, id),
        Arma3FieldKind::Section(fields) => {
            let Some(map) = value.as_object_mut() else {
                *value = Value::Object(Map::new());
                return true;
            };
            render_fields(ui, form, map, fields, id)
        }
        kind => render_rows(ui, form, value, kind, id),
    }
}

fn secret_reference(text: &str) -> bool {
    text.len() == 39
        && text.starts_with("secret_")
        && text[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn choice(
    ui: &mut Ui,
    value: &mut Value,
    id: egui::Id,
    label: egui::Id,
    choices: &[(&str, &str)],
) -> bool {
    let current = value.as_str().unwrap_or_default().to_owned();
    let mut selected = current.clone();
    egui::ComboBox::from_id_salt(id)
        .selected_text(
            choices
                .iter()
                .find(|(key, _)| *key == current)
                .map(|(_, name)| tr(name))
                .unwrap_or_default(),
        )
        .show_ui(ui, |ui| {
            for (key, name) in choices {
                ui.selectable_value(&mut selected, (*key).to_owned(), tr(name));
            }
        })
        .response
        .labelled_by(label);
    if selected == current {
        return false;
    }
    *value = Value::String(selected);
    true
}

fn render_strings(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    kind: Arma3ListKind,
    id: egui::Id,
) -> bool {
    let Some(items) = value.as_array_mut() else {
        *value = Value::Array(Vec::new());
        return true;
    };
    let mut changed = false;
    let mut remove = None;
    for (index, item) in items.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal_wrapped(|ui| {
                let label = ui.label(tr(list_item_label(kind)));
                changed |= edit_text(ui, form, item, id.with(index), label.id, |text| {
                    list_text_valid(kind, text)
                });
                if ui.button(tr("Remove")).clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        items.remove(index);
        forget_rows(ui);
        changed = true;
    }
    if items.is_empty() {
        ui.weak(tr(
            "Empty list: the engine receives an explicit empty array.",
        ));
    }
    if ui.button(tr("Add item")).clicked() {
        items.push(Value::String(String::new()));
        changed = true;
    }
    changed
}

fn list_text_valid(kind: Arma3ListKind, text: &str) -> bool {
    match kind {
        Arma3ListKind::Motd => true,
        Arma3ListKind::Uids => text.len() == 17 && text.bytes().all(|byte| byte.is_ascii_digit()),
        Arma3ListKind::Ipv4 => text.parse::<std::net::Ipv4Addr>().is_ok(),
        Arma3ListKind::Extensions => {
            !text.is_empty() && text.len() <= 16 && text.bytes().all(|b| b.is_ascii_alphanumeric())
        }
        Arma3ListKind::Urls | Arma3ListKind::Missions => !text.is_empty(),
    }
}

fn list_item_label(kind: Arma3ListKind) -> &'static str {
    match kind {
        Arma3ListKind::Motd => "Message",
        Arma3ListKind::Uids => "Steam ID",
        Arma3ListKind::Ipv4 => "IPv4 address",
        Arma3ListKind::Extensions => "Extension",
        Arma3ListKind::Urls => "URL",
        Arma3ListKind::Missions => "Mission template",
    }
}

fn render_rows(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    kind: Arma3FieldKind,
    id: egui::Id,
) -> bool {
    if matches!(kind, Arma3FieldKind::Pair | Arma3FieldKind::SlowNetwork) {
        return render_fixed_tuple(ui, form, value, kind, id);
    }
    let Some(rows) = value.as_array_mut() else {
        *value = Value::Array(Vec::new());
        return true;
    };
    let mut changed = false;
    let mut remove = None;
    for (index, row) in rows.iter_mut().enumerate() {
        let Some(cells) = row.as_array_mut() else {
            continue;
        };
        let row_id = id.with(index);
        ui.push_id(index, |ui| {
            ui.horizontal_wrapped(|ui| {
                changed |= match kind {
                    Arma3FieldKind::KickTimeouts => kick_row(ui, form, cells, row_id),
                    Arma3FieldKind::Channels => channel_row(ui, cells, row_id),
                    Arma3FieldKind::VoteCommands => vote_row(ui, form, cells, row_id),
                    _ => admin_row(ui, form, cells, row_id),
                };
                if ui.button(tr("Remove")).clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        rows.remove(index);
        forget_rows(ui);
        changed = true;
    }
    if rows.is_empty() {
        ui.weak(tr(
            "Empty list: the engine receives an explicit empty array.",
        ));
    }
    if ui.button(tr("Add row")).clicked() {
        let row = match kind {
            Arma3FieldKind::KickTimeouts => vec![Value::from(0), Value::from(-1)],
            Arma3FieldKind::Channels => {
                vec![Value::from(0), Value::Bool(false), Value::Bool(false)]
            }
            Arma3FieldKind::VoteCommands => {
                vec![
                    Value::String("kick".into()),
                    Value::Bool(false),
                    Value::Bool(true),
                ]
            }
            _ => vec![
                Value::String(String::new()),
                Value::Bool(true),
                Value::Bool(true),
            ],
        };
        rows.push(Value::Array(row));
        changed = true;
    }
    changed
}

fn render_fixed_tuple(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    kind: Arma3FieldKind,
    id: egui::Id,
) -> bool {
    let Some(cells) = value.as_array_mut() else {
        *value = default_value(kind);
        return true;
    };
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        if matches!(kind, Arma3FieldKind::Pair) {
            for (index, name) in ["Ready (seconds)", "Not ready (seconds)"]
                .iter()
                .enumerate()
            {
                let label = ui.label(tr(name));
                if let Some(cell) = cells.get_mut(index) {
                    changed |= edit_integer(
                        ui,
                        form,
                        cell,
                        id.with(index),
                        label.id,
                        0,
                        u64::from(u32::MAX),
                    );
                }
            }
        } else {
            for (index, name) in [
                "Kick for high ping",
                "Kick for packet loss",
                "Kick for desync",
                "Kick for disconnect timeout",
            ]
            .iter()
            .enumerate()
            {
                if let Some(cell) = cells.get_mut(index) {
                    changed |= flag(ui, cell, name);
                }
            }
        }
    });
    changed
}

fn kick_row(ui: &mut Ui, form: Form, cells: &mut Vec<Value>, id: egui::Id) -> bool {
    cells.resize(2, Value::from(0));
    let label = ui.label(tr("Kick cause"));
    let mut changed = integer_choice(
        ui,
        &mut cells[0],
        id.with("cause"),
        label.id,
        ARMA3_KICK_CAUSES,
    );
    let delay = ui.label(tr(
        "Rejoin delay (seconds, -1 until mission end, -2 until restart)",
    ));
    changed |= edit_integer(
        ui,
        form,
        &mut cells[1],
        id.with("delay"),
        delay.id,
        -2,
        i32::MAX as u64,
    );
    changed
}

fn channel_row(ui: &mut Ui, cells: &mut Vec<Value>, id: egui::Id) -> bool {
    cells.resize(3, Value::Bool(false));
    let label = ui.label(tr("Channel"));
    let mut changed = integer_choice(
        ui,
        &mut cells[0],
        id.with("channel"),
        label.id,
        ARMA3_CHANNELS,
    );
    changed |= flag(ui, &mut cells[1], "Disable text chat");
    changed |= flag(ui, &mut cells[2], "Disable voice");
    changed
}

fn vote_row(ui: &mut Ui, form: Form, cells: &mut Vec<Value>, id: egui::Id) -> bool {
    if cells.len() < 3 {
        return false;
    }
    let label = ui.label(tr("Vote command"));
    let mut changed = edit_text(
        ui,
        form,
        &mut cells[0],
        id.with("command"),
        label.id,
        |text| {
            !text.is_empty()
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        },
    );
    changed |= flag(ui, &mut cells[1], "Allowed before mission start");
    changed |= flag(ui, &mut cells[2], "Allowed after mission start");
    for (index, name) in [(3, "Vote threshold"), (4, "Side vote threshold")] {
        let mut custom = cells.len() > index;
        let toggle = ui.checkbox(&mut custom, tr(name));
        if toggle.changed() {
            if custom && cells.len() == index {
                cells.push(Value::Number(
                    Number::from_f64(0.5).unwrap_or_else(|| Number::from(0)),
                ));
            } else if !custom {
                cells.truncate(index);
            }
            forget(ui, id.with(index));
            changed = true;
        }
        if cells.len() > index {
            changed |= edit_decimal(
                ui,
                form,
                &mut cells[index],
                id.with(index),
                toggle.id,
                0.0,
                1.0,
            );
        } else {
            break;
        }
    }
    changed
}

fn admin_row(ui: &mut Ui, form: Form, cells: &mut [Value], id: egui::Id) -> bool {
    if cells.len() < 3 {
        return false;
    }
    let label = ui.label(tr("Administrator command (empty for all)"));
    let mut changed = edit_text(
        ui,
        form,
        &mut cells[0],
        id.with("command"),
        label.id,
        |text| {
            let name = text.strip_prefix('#').unwrap_or(text);
            (text.is_empty() || !name.is_empty())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        },
    );
    changed |= flag(ui, &mut cells[1], "Allowed before mission start");
    changed |= flag(ui, &mut cells[2], "Allowed after mission start");
    changed
}

fn flag(ui: &mut Ui, cell: &mut Value, name: &str) -> bool {
    let mut checked = cell.as_bool().unwrap_or(false);
    let changed = ui.checkbox(&mut checked, tr(name)).changed();
    if changed {
        *cell = Value::Bool(checked);
    }
    changed
}

fn integer_choice(
    ui: &mut Ui,
    value: &mut Value,
    id: egui::Id,
    label: egui::Id,
    choices: &[(u64, &str)],
) -> bool {
    let current = value.as_u64().unwrap_or(0);
    let mut selected = current;
    let text = |number: u64| {
        choices
            .iter()
            .find(|(known, _)| *known == number)
            .map(|(_, name)| format!("{number}: {}", tr(name)))
            .unwrap_or_else(|| number.to_string())
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(text(current))
        .show_ui(ui, |ui| {
            for (number, _) in choices {
                ui.selectable_value(&mut selected, *number, text(*number));
            }
        })
        .response
        .labelled_by(label);
    if selected == current {
        return false;
    }
    *value = Value::from(selected);
    true
}

fn buffer(ui: &Ui, id: egui::Id) -> Option<String> {
    ui.ctx()
        .data(|data| data.get_temp::<FieldText>(id))
        .map(|text| text.0)
}

fn store(ui: &Ui, id: egui::Id, text: String) {
    ui.ctx()
        .data_mut(|data| data.insert_temp(id, FieldText(text)));
}

fn forget(ui: &Ui, id: egui::Id) {
    ui.ctx().data_mut(|data| {
        data.remove::<FieldText>(id);
        data.get_temp_mut_or_default::<HashMap<egui::Id, egui::Id>>(invalid_fields())
            .remove(&id);
    });
}

pub(super) fn edit_source_list(
    ui: &mut Ui,
    form: Form,
    values: &mut Vec<String>,
    id: egui::Id,
    label: egui::Id,
) -> bool {
    let mut value = Value::Array(values.iter().cloned().map(Value::String).collect());
    let changed = edit_buffered(
        ui,
        form,
        &mut value,
        (id, label),
        values.join(", "),
        |text| {
            let items: Vec<&str> = text
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect();
            items
                .iter()
                .all(|item| {
                    item.len() <= 64
                        && item
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                })
                .then(|| {
                    Value::Array(
                        items
                            .into_iter()
                            .map(|item| Value::String(item.to_owned()))
                            .collect(),
                    )
                })
        },
        "Use comma-separated content source ids.",
    );
    if changed {
        *values = value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
    }
    changed
}

pub(super) fn edit_optional_integer(
    ui: &mut Ui,
    form: Form,
    value: &mut Option<u64>,
    (id, label): (egui::Id, egui::Id),
    allowed: impl Fn(u64) -> bool,
) -> bool {
    let mut current = value.map_or(Value::Null, Value::from);
    let changed = edit_buffered(
        ui,
        form,
        &mut current,
        (id, label),
        value.map(|number| number.to_string()).unwrap_or_default(),
        |text| {
            let text = text.trim();
            if text.is_empty() {
                return Some(Value::Null);
            }
            text.parse::<u64>()
                .ok()
                .filter(|number| allowed(*number))
                .map(Value::from)
        },
        "Enter a whole number in the allowed range, or leave it empty.",
    );
    if changed {
        *value = current.as_u64();
    }
    changed
}

/// Drop typed buffers after removal because row positions shift.
fn forget_rows(ui: &Ui) {
    clear_edit_buffers(ui.ctx());
}

/// Keep typed text while invalid or focused; write only accepted values and report invalid fields to block Save.
fn edit_buffered(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    (id, label): (egui::Id, egui::Id),
    model_text: String,
    parse: impl Fn(&str) -> Option<Value>,
    error: &str,
) -> bool {
    let typed = buffer(ui, id);
    let focused = ui.memory(|memory| memory.has_focus(id));
    let mut text = match typed {
        Some(typed) if focused || parse(&typed).is_none() => typed,
        Some(_) => {
            forget(ui, id);
            model_text
        }
        None => model_text,
    };
    let response = ui
        .add(
            egui::TextEdit::singleline(&mut text)
                .id(id)
                .desired_width(260.0)
                .char_limit(64 * 1024),
        )
        .labelled_by(label);
    let parsed = parse(&text);
    form.mark(ui, id, parsed.is_none());
    if parsed.is_none() {
        ui.colored_label(ui.visuals().error_fg_color, tr(error));
    }
    if !response.changed() {
        return false;
    }
    store(ui, id, text);
    match parsed {
        Some(next) if next != *value => {
            *value = next;
            true
        }
        _ => false,
    }
}

fn edit_text(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    id: egui::Id,
    label: egui::Id,
    valid: impl Fn(&str) -> bool,
) -> bool {
    let model = value.as_str().unwrap_or_default().to_owned();
    edit_buffered(
        ui,
        form,
        value,
        (id, label),
        model,
        |text| {
            (text.len() <= 64 * 1024 && !text.chars().any(char::is_control) && valid(text))
                .then(|| Value::String(text.to_owned()))
        },
        "Enter a valid value for this field.",
    )
}

fn edit_decimal(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    id: egui::Id,
    label: egui::Id,
    min: f64,
    max: f64,
) -> bool {
    let model = value
        .as_number()
        .map(crate::core::server::arma3_config::number_text)
        .unwrap_or_default();
    edit_buffered(
        ui,
        form,
        value,
        (id, label),
        model,
        |text| {
            parse_decimal_input(text)
                .filter(|number| {
                    number
                        .as_f64()
                        .is_some_and(|number| number >= min && number <= max)
                })
                .map(Value::Number)
        },
        "Enter a number in the allowed range. Use a decimal point or comma.",
    )
}

fn edit_integer(
    ui: &mut Ui,
    form: Form,
    value: &mut Value,
    id: egui::Id,
    label: egui::Id,
    min: i64,
    max: u64,
) -> bool {
    let model = value
        .as_number()
        .map(ToString::to_string)
        .unwrap_or_default();
    edit_buffered(
        ui,
        form,
        value,
        (id, label),
        model,
        |text| {
            let text = text.trim();
            if let Ok(number) = text.parse::<u64>() {
                (number <= max && i128::from(number) >= i128::from(min))
                    .then(|| Value::from(number))
            } else {
                text.parse::<i64>()
                    .ok()
                    .filter(|number| *number >= min && *number < 0)
                    .map(Value::from)
            }
        },
        "Enter a whole number in the allowed range.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_names_are_engine_identifiers() {
        assert!(parameter_name("Probe_1"));
        assert!(parameter_name("_hidden"));
        assert!(!parameter_name("1st"));
        assert!(!parameter_name("bad-name"));
        assert!(!parameter_name(""));
    }

    #[test]
    fn secret_reference_shape_matches_core_validation() {
        assert!(secret_reference(&format!("secret_{}", "0".repeat(32))));
        assert!(!secret_reference("hunter2"));
        assert!(!secret_reference(&format!("secret_{}", "G".repeat(32))));
    }
}
