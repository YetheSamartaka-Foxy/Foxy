use serde_json::{Map, Number, Value};

use super::super::arma3_config::{
    Arma3BasicSettings, Arma3CustomAi, Arma3DifficultyOptions, Arma3Field, Arma3FieldKind,
    Arma3ServerSettings,
};
use super::super::{Arma3Difficulty, ServerDefinition, ServerInstance, SignatureVerification};
use super::ConfigurationRole;
use super::parser::{CfgValue, Document, Node, NodeKind};

pub(super) fn apply(
    instance: &mut ServerInstance,
    role: ConfigurationRole,
    document: &Document,
) -> Result<Vec<String>, String> {
    let ServerDefinition::Arma3(arma) = &mut instance.definition else {
        return Err("Configuration role is not supported for this server type".into());
    };
    let mut warnings = Vec::new();
    match role {
        ConfigurationRole::Server => {
            let mut root =
                serde_json::to_value(&*arma.server).map_err(|_| "Invalid server settings")?;
            for node in &document.nodes {
                if matches!(&node.kind, NodeKind::Assignment { append: true, .. }) {
                    warnings.push(format!(
                        "Append assignment requires operator ownership: {}",
                        node.name
                    ));
                    continue;
                }
                if let NodeKind::Class { .. } = &node.kind {
                    if node.name.eq_ignore_ascii_case("Missions") {
                        map_missions(node, &mut arma.mission_cycle, &mut warnings)?;
                    } else if find_field(Arma3ServerSettings::FIELDS, &node.name).is_some() {
                        map_fields(
                            std::slice::from_ref(node),
                            Arma3ServerSettings::FIELDS,
                            &mut root,
                            "server",
                            &mut warnings,
                        )?;
                    } else {
                        warnings.push(format!("Unknown server class: {}", node.name));
                    }
                    continue;
                }
                let NodeKind::Assignment { value, .. } = &node.kind else {
                    continue;
                };
                if node.name.eq_ignore_ascii_case("hostname") {
                    arma.hostname = as_string(value)?;
                } else if node.name.eq_ignore_ascii_case("maxPlayers") {
                    arma.max_players = as_u16(value)?;
                } else if node.name.eq_ignore_ascii_case("persistent") {
                    arma.persistent = as_bool(value)?;
                } else if node.name.eq_ignore_ascii_case("BattlEye") {
                    arma.battleye = as_bool(value)?;
                } else if node.name.eq_ignore_ascii_case("verifySignatures") {
                    arma.verify_signatures = match as_u64(value)? {
                        0 => SignatureVerification::Disabled,
                        2 => SignatureVerification::V2,
                        _ => return Err("Invalid signature verification value".into()),
                    };
                } else if let Some(field) = find_field(Arma3ServerSettings::FIELDS, &node.name) {
                    if is_hook(&node.name)
                        && matches!(value, CfgValue::String(text) if text.contains('\r') || text.contains('\n'))
                    {
                        warnings.push(format!("Multiline operator hook preserved: {}", node.name));
                        continue;
                    }
                    let json = value_json(value, field.kind)?;
                    map_field(&mut root, field, json, &node.name, &mut warnings)?;
                } else {
                    warnings.push(format!("Unknown server setting: {}", node.name));
                }
            }
            *arma.server =
                serde_json::from_value(root).map_err(|_| "Invalid server setting value")?;
        }
        ConfigurationRole::Basic => {
            let mut root =
                serde_json::to_value(&*arma.basic).map_err(|_| "Invalid basic settings")?;
            map_fields(
                document.nodes.as_slice(),
                Arma3BasicSettings::FIELDS,
                &mut root,
                "basic",
                &mut warnings,
            )?;
            *arma.basic =
                serde_json::from_value(root).map_err(|_| "Invalid basic setting value")?;
        }
        ConfigurationRole::Profile => map_profile(document, arma, &mut warnings)?,
    }
    Ok(warnings)
}

fn map_profile(
    document: &Document,
    arma: &mut super::super::Arma3ServerDefinition,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let mut root = serde_json::to_value(arma.difficulty.as_deref().cloned().unwrap_or_default())
        .map_err(|_| "Invalid profile settings")?;
    let object = root.as_object_mut().ok_or("Invalid profile settings")?;
    object
        .entry("options")
        .or_insert_with(|| Value::Object(Map::new()));
    object
        .entry("custom_ai")
        .or_insert_with(|| Value::Object(Map::new()));
    for node in &document.nodes {
        let NodeKind::Class { children, .. } = &node.kind else {
            if node.name.eq_ignore_ascii_case("version") {
                continue;
            }
            warnings.push(format!("Unmanaged profile setting: {}", node.name));
            continue;
        };
        let target = if node.name.eq_ignore_ascii_case("DifficultyPresets")
            || node.name.eq_ignore_ascii_case("CustomDifficulty")
        {
            Some((children.as_slice(), ""))
        } else {
            None
        };
        if let Some((nodes, _)) = target {
            for section in nodes {
                match &section.kind {
                    NodeKind::Class { children, .. }
                        if section.name.eq_ignore_ascii_case("CustomAILevel") =>
                    {
                        map_fields(
                            children,
                            Arma3CustomAi::FIELDS,
                            root.get_mut("custom_ai").ok_or("Invalid profile")?,
                            "CustomAILevel",
                            warnings,
                        )?
                    }
                    NodeKind::Class { children, .. }
                        if section.name.eq_ignore_ascii_case("CustomDifficulty")
                            || section.name.eq_ignore_ascii_case("DifficultyPresets") =>
                    {
                        for child in children {
                            match &child.kind {
                                NodeKind::Class {
                                    children: fields, ..
                                } if child.name.eq_ignore_ascii_case("Options") => map_fields(
                                    fields,
                                    Arma3DifficultyOptions::FIELDS,
                                    root.get_mut("options").ok_or("Invalid profile")?,
                                    "Options",
                                    warnings,
                                )?,
                                NodeKind::Class {
                                    children: fields, ..
                                } if child.name.eq_ignore_ascii_case("CustomAILevel") => {
                                    map_fields(
                                        fields,
                                        Arma3CustomAi::FIELDS,
                                        root.get_mut("custom_ai").ok_or("Invalid profile")?,
                                        "CustomAILevel",
                                        warnings,
                                    )?
                                }
                                NodeKind::Assignment { value, .. }
                                    if child.name.eq_ignore_ascii_case("aiLevelPreset") =>
                                {
                                    root.as_object_mut().ok_or("Invalid profile")?.insert(
                                        "ai_level_preset".into(),
                                        value_json(
                                            value,
                                            Arma3FieldKind::Integer { min: 0, max: 3 },
                                        )?,
                                    );
                                }
                                NodeKind::Assignment { .. }
                                    if child.name.eq_ignore_ascii_case("difficulty") =>
                                {
                                    warnings.push(
                                        "Addon-defined difficulty is preserved for operator review"
                                            .into(),
                                    )
                                }
                                _ => warnings
                                    .push(format!("Unmanaged profile setting: {}", child.name)),
                            }
                        }
                    }
                    _ => warnings.push(format!("Unmanaged profile setting: {}", section.name)),
                }
            }
        } else {
            warnings.push(format!("Unmanaged profile class: {}", node.name));
        }
    }
    arma.difficulty = Some(Box::new(
        serde_json::from_value(root).map_err(|_| "Invalid profile setting value")?,
    ));
    Ok(())
}

fn map_missions(
    node: &Node,
    missions: &mut Vec<super::super::Arma3Mission>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let NodeKind::Class { children, .. } = &node.kind else {
        return Err("Invalid mission cycle".into());
    };
    let mut parsed = Vec::new();
    for entry in children {
        let NodeKind::Class {
            children: fields, ..
        } = &entry.kind
        else {
            warnings.push("Unmanaged mission entry".into());
            continue;
        };
        let mut template = None;
        let mut difficulty = Arma3Difficulty::Regular;
        let mut params = std::collections::BTreeMap::new();
        for field in fields {
            match &field.kind {
                NodeKind::Assignment { value, .. }
                    if field.name.eq_ignore_ascii_case("template") =>
                {
                    template = Some(as_string(value)?)
                }
                NodeKind::Assignment { value, .. }
                    if field.name.eq_ignore_ascii_case("difficulty") =>
                {
                    difficulty = match as_string(value)?.to_ascii_lowercase().as_str() {
                        "recruit" => Arma3Difficulty::Recruit,
                        "regular" => Arma3Difficulty::Regular,
                        "veteran" => Arma3Difficulty::Veteran,
                        "custom" => Arma3Difficulty::Custom,
                        _ => return Err("Invalid mission difficulty".into()),
                    }
                }
                NodeKind::Class { children, .. } if field.name.eq_ignore_ascii_case("Params") => {
                    for param in children {
                        if let NodeKind::Assignment { value, .. } = &param.kind {
                            if let Some(number) = numeric(value) {
                                params.insert(param.name.clone(), number);
                            } else {
                                warnings.push("Unmanaged mission parameter".into());
                            }
                        }
                    }
                }
                _ => warnings.push(format!("Unmanaged mission setting: {}", field.name)),
            }
        }
        if let Some(template) = template {
            parsed.push(super::super::Arma3Mission {
                enabled: true,
                source: None,
                template,
                difficulty,
                params,
            });
        } else {
            warnings.push("Mission entry without a template".into());
        }
    }
    *missions = parsed;
    Ok(())
}

fn map_fields(
    nodes: &[Node],
    fields: &[Arma3Field],
    root: &mut Value,
    path: &str,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    for node in nodes {
        let Some(field) = find_field(fields, &node.name) else {
            if node.name.to_ascii_lowercase().starts_with("mapcontent") {
                warnings.push(format!(
                    "Legacy mapContent conflict requires review: {}",
                    node.name
                ));
            } else {
                warnings.push(format!("Unknown setting {path}.{}", node.name));
            }
            continue;
        };
        match (&node.kind, field.kind) {
            (NodeKind::Class { children, .. }, Arma3FieldKind::Section(nested)) => {
                let key = field.json_name;
                if root.get(key).is_none_or(Value::is_null) {
                    root.as_object_mut()
                        .ok_or("Invalid settings")?
                        .insert(key.into(), Value::Object(Map::new()));
                }
                map_fields(
                    children,
                    nested,
                    root.get_mut(key).ok_or("Invalid settings")?,
                    &format!("{path}.{}", node.name),
                    warnings,
                )?;
            }
            (NodeKind::Assignment { value, .. }, kind) => map_field(
                root,
                field,
                value_json(value, kind)?,
                &format!("{path}.{}", node.name),
                warnings,
            )?,
            _ => warnings.push(format!("Unrepresentable setting {path}.{}", node.name)),
        }
    }
    Ok(())
}

fn map_field(
    root: &mut Value,
    field: &Arma3Field,
    value: Value,
    path: &str,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let object = root.as_object_mut().ok_or("Invalid settings")?;
    if let Arma3FieldKind::Section(nested) = field.kind {
        let current = object
            .entry(field.json_name)
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(map) = current.as_object_mut() else {
            return Err("Invalid nested setting".into());
        };
        return map_field_table(map, &value, nested, path, warnings);
    }
    object.insert(field.json_name.into(), value);
    Ok(())
}

fn map_field_table(
    object: &mut Map<String, Value>,
    value: &Value,
    fields: &[Arma3Field],
    path: &str,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let Some(source) = value.as_object() else {
        return Err("Invalid nested setting".into());
    };
    for (key, value) in source {
        if let Some(field) = find_field(fields, key) {
            object.insert(field.json_name.into(), value.clone());
        } else {
            warnings.push(format!("Unknown setting {path}.{key}"));
        }
    }
    Ok(())
}

fn find_field<'a>(fields: &'a [Arma3Field], name: &str) -> Option<&'a Arma3Field> {
    fields
        .iter()
        .find(|field| field.engine_name.eq_ignore_ascii_case(name))
}

fn is_hook(name: &str) -> bool {
    [
        "doubleIdDetected",
        "onUserConnected",
        "onUserDisconnected",
        "onUserKicked",
        "onHackedData",
        "onUnsignedData",
    ]
    .iter()
    .any(|hook| hook.eq_ignore_ascii_case(name))
}
fn value_json(value: &CfgValue, kind: Arma3FieldKind) -> Result<Value, String> {
    let mut json = match value {
        CfgValue::String(s) => Value::String(s.clone()),
        CfgValue::Number(n) => Value::Number(n.clone()),
        CfgValue::Bool(b) => Value::Bool(*b),
        CfgValue::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| {
                    value_json(v, Arma3FieldKind::Text)
                        .or_else(|_| {
                            value_json(
                                v,
                                Arma3FieldKind::Integer {
                                    min: i64::MIN,
                                    max: u64::MAX,
                                },
                            )
                        })
                        .or_else(|_| value_json(v, Arma3FieldKind::Bool))
                        .map_err(|_| "Invalid array value".to_string())
                })
                .collect::<Result<_, _>>()?,
        ),
        CfgValue::Opaque(_) => return Err("Unsupported value for managed setting".into()),
    };
    if let Arma3FieldKind::SecretRef = kind
        && let Some(text) = json.as_str().and_then(|s| s.strip_prefix("foxy-secret:"))
        && text.len() == 39
        && text.starts_with("secret_")
        && text[7..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        json = Value::String(text.to_owned());
    }
    convert_numeric_booleans(&mut json, kind);
    Ok(json)
}

fn convert_numeric_booleans(value: &mut Value, kind: Arma3FieldKind) {
    match kind {
        Arma3FieldKind::Bool => {
            if let Some(n) = value.as_i64()
                && (n == 0 || n == 1)
            {
                *value = Value::Bool(n == 1);
            }
        }
        Arma3FieldKind::SlowNetwork => {
            if let Some(items) = value.as_array_mut() {
                for item in items {
                    convert_numeric_booleans(item, Arma3FieldKind::Bool);
                }
            }
        }
        Arma3FieldKind::Channels
        | Arma3FieldKind::VoteCommands
        | Arma3FieldKind::VotedAdminCommands => {
            if let Some(rows) = value.as_array_mut() {
                for row in rows {
                    if let Some(items) = row.as_array_mut() {
                        for index in [1, 2] {
                            if let Some(item) = items.get_mut(index) {
                                convert_numeric_booleans(item, Arma3FieldKind::Bool);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
fn numeric(value: &CfgValue) -> Option<Number> {
    match value {
        CfgValue::Number(n) => Some(n.clone()),
        CfgValue::Bool(b) => Some(Number::from(u8::from(*b))),
        _ => None,
    }
}
fn as_string(value: &CfgValue) -> Result<String, String> {
    if let CfgValue::String(s) = value {
        Ok(s.clone())
    } else {
        Err("Invalid string setting".into())
    }
}
fn as_u64(value: &CfgValue) -> Result<u64, String> {
    if let CfgValue::Number(n) = value {
        n.as_u64().ok_or_else(|| "Invalid integer setting".into())
    } else {
        Err("Invalid integer setting".into())
    }
}
fn as_u16(value: &CfgValue) -> Result<u16, String> {
    u16::try_from(as_u64(value)?).map_err(|_| "Invalid integer setting".into())
}
fn as_bool(value: &CfgValue) -> Result<bool, String> {
    match value {
        CfgValue::Bool(b) => Ok(*b),
        CfgValue::Number(n) if n.as_i64() == Some(0) => Ok(false),
        CfgValue::Number(n) if n.as_i64() == Some(1) => Ok(true),
        _ => Err("Invalid boolean setting".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::ServerDefinitionKind;

    #[test]
    fn maps_server_basic_profile_and_missions_with_unknown_diagnostics() {
        let mut instance =
            ServerInstance::draft("arma3", ServerDefinitionKind::Arma3, "Server").unwrap();
        let server = super::super::parser::parse("hostname=\"Imported\"; maxPlayers=48; verifySignatures=0; disableVoN=0; class Missions { class M1 { template=\"boot.Altis\"; difficulty=\"Veteran\"; class Params { mode=18446744073709551615; }; }; }; customSecret=\"redacted?\";").unwrap();
        let warnings = apply(&mut instance, ConfigurationRole::Server, &server).unwrap();
        assert_eq!(warnings, vec!["Unknown server setting: customSecret"]);
        let ServerDefinition::Arma3(arma) = &instance.definition else {
            panic!()
        };
        assert_eq!(arma.hostname, "Imported");
        assert_eq!(arma.max_players, 48);
        assert_eq!(
            arma.mission_cycle[0].params["mode"].as_u64(),
            Some(u64::MAX)
        );
        assert_eq!(arma.verify_signatures, SignatureVerification::Disabled);
        assert_eq!(arma.server.disable_von, Some(false));

        let basic = super::super::parser::parse(
            "MinBandwidth=18446744073709551615; class sockets { maxPacketSize=1400; }; unknown=1;",
        )
        .unwrap();
        let warnings = apply(&mut instance, ConfigurationRole::Basic, &basic).unwrap();
        assert_eq!(warnings.len(), 1);
        let ServerDefinition::Arma3(arma) = &instance.definition else {
            panic!()
        };
        assert_eq!(arma.basic.min_bandwidth, Some(u64::MAX));
        assert_eq!(
            arma.basic.sockets.as_ref().unwrap().max_packet_size,
            Some(1400)
        );

        let profile = super::super::parser::parse("class DifficultyPresets { class CustomDifficulty { class Options { reducedDamage=1; }; aiLevelPreset=3; }; class CustomAILevel { skillAI=0.35; }; };").unwrap();
        apply(&mut instance, ConfigurationRole::Profile, &profile).unwrap();
        let ServerDefinition::Arma3(arma) = &instance.definition else {
            panic!()
        };
        let difficulty = arma.difficulty.as_ref().unwrap();
        assert_eq!(difficulty.options.reduced_damage, Some(true));
        assert_eq!(difficulty.ai_level_preset, Some(3));
        assert_eq!(
            difficulty
                .custom_ai
                .skill_ai
                .as_ref()
                .and_then(Number::as_f64),
            Some(0.35)
        );
    }
}
