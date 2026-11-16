use serde::Serialize;
use serde_json::{Map, Number, Value};

#[derive(Clone, Copy, Debug)]
pub struct Arma3Field {
    pub json_name: &'static str,
    pub engine_name: &'static str,
    pub kind: Arma3FieldKind,
    pub label: &'static str,
    /// First Arma 3 version (major * 100 + minor) that Bohemia documents for the field; 0 when
    /// no introduction version is documented. Every field is present in the accepted 2.22 binary.
    pub since: u16,
}

#[derive(Clone, Copy, Debug)]
pub enum Arma3FieldKind {
    Bool,
    Integer { min: i64, max: u64 },
    Decimal { min: f64, max: f64 },
    Text,
    Strings(Arma3ListKind),
    Pair,
    SlowNetwork,
    KickTimeouts,
    VoteCommands,
    VotedAdminCommands,
    Channels,
    Choice(&'static [(&'static str, &'static str)]),
    Difficulty,
    SecretRef,
    Section(&'static [Arma3Field]),
}

#[derive(Clone, Copy, Debug)]
pub enum Arma3ListKind {
    Motd,
    Uids,
    Ipv4,
    Extensions,
    Urls,
    Missions,
}

pub const ARMA3_KICK_CAUSES: &[(u64, &str)] = &[
    (0, "Manual kick"),
    (1, "Connectivity kick"),
    (2, "BattlEye kick"),
    (3, "Harmless kick"),
];

pub const ARMA3_CHANNELS: &[(u64, &str)] = &[
    (0, "Global"),
    (1, "Side"),
    (2, "Command"),
    (3, "Group"),
    (4, "Vehicle"),
    (5, "Direct"),
    (6, "System"),
];

const MAX_STRING: usize = 64 * 1024;
const MAX_ITEMS: usize = 1024;
const MAX_SECTION: usize = 1024 * 1024;

fn object<T: Serialize>(settings: &T) -> Result<Map<String, Value>, String> {
    match serde_json::to_value(settings).map_err(|error| error.to_string())? {
        Value::Object(map) => Ok(map),
        _ => Err("settings must serialize as an object".into()),
    }
}

pub(crate) fn validate_section(
    settings: &impl Serialize,
    fields: &[Arma3Field],
) -> Result<(), String> {
    validate_map(&object(settings)?, fields, None)
}

pub(crate) fn validate_version(
    settings: &impl Serialize,
    fields: &[Arma3Field],
    version: u16,
) -> Result<(), String> {
    validate_map(&object(settings)?, fields, Some(version))
}

pub(crate) fn render_section(
    settings: &impl Serialize,
    fields: &[Arma3Field],
) -> Result<String, String> {
    let map = object(settings)?;
    validate_map(&map, fields, None)?;
    let mut out = String::new();
    render_map(&mut out, &map, fields);
    if out.len() > MAX_SECTION {
        return Err("rendered section exceeds size limit".into());
    }
    Ok(out)
}

fn validate_map(
    map: &Map<String, Value>,
    fields: &[Arma3Field],
    version: Option<u16>,
) -> Result<(), String> {
    if let Some(key) = map
        .keys()
        .find(|key| !fields.iter().any(|field| field.json_name == key.as_str()))
    {
        return Err(format!("unknown configuration field: {key}"));
    }
    for field in fields {
        let Some(value) = map.get(field.json_name).filter(|value| !value.is_null()) else {
            continue;
        };
        if let Some(version) = version
            && field.since > version
        {
            return Err(format!(
                "{} requires Arma 3 {}.{:02} or newer",
                field.label,
                field.since / 100,
                field.since % 100
            ));
        }
        let invalid = || format!("Invalid value for {}", field.label);
        match field.kind {
            Arma3FieldKind::Section(nested) => {
                validate_map(value.as_object().ok_or_else(invalid)?, nested, version)?
            }
            kind => validate_value(value, kind).map_err(|_| invalid())?,
        }
    }
    Ok(())
}

fn validate_value(value: &Value, kind: Arma3FieldKind) -> Result<(), ()> {
    match kind {
        Arma3FieldKind::Bool => value.is_boolean().then_some(()).ok_or(()),
        Arma3FieldKind::Integer { min, max } => integer_in(value, min, max),
        Arma3FieldKind::Decimal { min, max } => decimal_in(value, min, max),
        Arma3FieldKind::Text => text(value).map(drop),
        Arma3FieldKind::SecretRef => text(value)
            .ok()
            .filter(|reference| secret_reference(reference))
            .map(drop)
            .ok_or(()),
        Arma3FieldKind::Difficulty => text(value)
            .ok()
            .filter(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_lowercase()))
            .map(drop)
            .ok_or(()),
        Arma3FieldKind::Choice(choices) => value
            .as_str()
            .filter(|current| choices.iter().any(|(key, _)| key == current))
            .map(drop)
            .ok_or(()),
        Arma3FieldKind::Strings(list) => items(value)?
            .iter()
            .try_for_each(|item| list_item(item, list)),
        Arma3FieldKind::Pair => tuple(value, 2..=2)?
            .iter()
            .try_for_each(|item| integer_in(item, 0, u64::from(u32::MAX))),
        Arma3FieldKind::SlowNetwork => tuple(value, 4..=4)?
            .iter()
            .all(Value::is_boolean)
            .then_some(())
            .ok_or(()),
        Arma3FieldKind::KickTimeouts => unique_rows(value, ARMA3_KICK_CAUSES, |row| {
            tuple(row, 2..=2)?;
            integer_in(&row[1], -2, i32::MAX as u64)
        }),
        Arma3FieldKind::Channels => unique_rows(value, ARMA3_CHANNELS, |row| {
            let row = tuple(row, 3..=3)?;
            row[1..]
                .iter()
                .all(Value::is_boolean)
                .then_some(())
                .ok_or(())
        }),
        Arma3FieldKind::VoteCommands => items(value)?.iter().try_for_each(|row| {
            let row = tuple(row, 3..=5)?;
            if !row[0].as_str().is_some_and(vote_command) {
                return Err(());
            }
            command_flags(row)?;
            row[3..]
                .iter()
                .try_for_each(|threshold| decimal_in(threshold, 0.0, 1.0))
        }),
        Arma3FieldKind::VotedAdminCommands => items(value)?.iter().try_for_each(|row| {
            let row = tuple(row, 3..=3)?;
            if !row[0].as_str().is_some_and(admin_command) {
                return Err(());
            }
            command_flags(row)
        }),
        Arma3FieldKind::Section(_) => Err(()),
    }
}

fn integer_in(value: &Value, min: i64, max: u64) -> Result<(), ()> {
    let number = value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .ok_or(())?;
    (number >= i128::from(min) && number <= i128::from(max))
        .then_some(())
        .ok_or(())
}

fn decimal_in(value: &Value, min: f64, max: f64) -> Result<(), ()> {
    value
        .as_f64()
        .filter(|number| number.is_finite() && *number >= min && *number <= max)
        .map(drop)
        .ok_or(())
}

fn text(value: &Value) -> Result<&str, ()> {
    value
        .as_str()
        .filter(|text| text.len() <= MAX_STRING && !text.chars().any(char::is_control))
        .ok_or(())
}

fn items(value: &Value) -> Result<&Vec<Value>, ()> {
    value
        .as_array()
        .filter(|items| items.len() <= MAX_ITEMS)
        .ok_or(())
}

fn tuple(value: &Value, len: std::ops::RangeInclusive<usize>) -> Result<&Vec<Value>, ()> {
    value
        .as_array()
        .filter(|row| len.contains(&row.len()))
        .ok_or(())
}

fn unique_rows(
    value: &Value,
    ids: &[(u64, &str)],
    row_valid: impl Fn(&Value) -> Result<(), ()>,
) -> Result<(), ()> {
    let mut seen = Vec::new();
    for row in items(value)? {
        row_valid(row)?;
        let id = row[0]
            .as_u64()
            .filter(|id| ids.iter().any(|(known, _)| known == id));
        let id = id.ok_or(())?;
        if seen.contains(&id) {
            return Err(());
        }
        seen.push(id);
    }
    Ok(())
}

fn command_flags(row: &[Value]) -> Result<(), ()> {
    (row[1].is_boolean() && row[2].is_boolean())
        .then_some(())
        .ok_or(())
}

fn list_item(item: &Value, list: Arma3ListKind) -> Result<(), ()> {
    let value = text(item)?;
    let valid = match list {
        Arma3ListKind::Motd => true,
        Arma3ListKind::Uids => value.len() == 17 && value.bytes().all(|b| b.is_ascii_digit()),
        Arma3ListKind::Ipv4 => value.parse::<std::net::Ipv4Addr>().is_ok(),
        Arma3ListKind::Extensions => {
            !value.is_empty()
                && value.len() <= 16
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }
        Arma3ListKind::Urls => http_url(value),
        Arma3ListKind::Missions => mission_name(value),
    };
    valid.then_some(()).ok_or(())
}

pub(crate) fn http_url(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn secret_reference(value: &str) -> bool {
    value.len() == 39
        && value.starts_with("secret_")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn mission_name(value: &str) -> bool {
    let Some((stem, extension)) = value.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !extension.is_empty()
        && value.len() <= 256
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && !extension.eq_ignore_ascii_case("sqf")
}

fn vote_command(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn admin_command(value: &str) -> bool {
    let name = value.strip_prefix('#').unwrap_or(value);
    value.len() <= 64
        && (value.is_empty() || !name.is_empty())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(crate) fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) fn number_text(number: &Number) -> String {
    if number.is_f64() {
        let value = number.as_f64().unwrap_or_default();
        if value == value.trunc() && value.abs() < 1e15 {
            format!("{value:.1}")
        } else {
            format!("{value}")
        }
    } else {
        number.to_string()
    }
}

pub fn parse_decimal_input(input: &str) -> Option<Number> {
    let trimmed = input.trim();
    let normalized = if !trimmed.contains('.') && trimmed.matches(',').count() == 1 {
        trimmed.replace(',', ".")
    } else {
        trimmed.to_owned()
    };
    if normalized.is_empty()
        || normalized.contains(['e', 'E'])
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-'))
    {
        return None;
    }
    let value = normalized
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    if normalized.contains('.') {
        Number::from_f64(value)
    } else {
        normalized
            .parse::<i64>()
            .ok()
            .map(Number::from)
            .or_else(|| Number::from_f64(value))
    }
}

fn render_map(out: &mut String, map: &Map<String, Value>, fields: &[Arma3Field]) {
    for field in fields {
        if let Some(value) = map.get(field.json_name).filter(|value| !value.is_null()) {
            render_field(out, field, value);
        }
    }
}

fn render_field(out: &mut String, field: &Arma3Field, value: &Value) {
    if let Arma3FieldKind::Section(nested) = field.kind {
        out.push_str(&format!("class {}\n{{\n", field.engine_name));
        if let Some(map) = value.as_object() {
            render_map(out, map, nested);
        }
        out.push_str("};\n");
        return;
    }
    let array = matches!(
        field.kind,
        Arma3FieldKind::Strings(_)
            | Arma3FieldKind::Pair
            | Arma3FieldKind::SlowNetwork
            | Arma3FieldKind::KickTimeouts
            | Arma3FieldKind::VoteCommands
            | Arma3FieldKind::VotedAdminCommands
            | Arma3FieldKind::Channels
    );
    let rendered = match field.kind {
        Arma3FieldKind::Bool => scalar_bool(value),
        Arma3FieldKind::Text | Arma3FieldKind::Choice(_) => {
            quoted(value.as_str().unwrap_or_default())
        }
        Arma3FieldKind::Difficulty => quoted(&difficulty_name(value.as_str().unwrap_or_default())),
        Arma3FieldKind::SecretRef => quoted("<redacted secret reference>"),
        Arma3FieldKind::Integer { .. } | Arma3FieldKind::Decimal { .. } => {
            value.as_number().map(number_text).unwrap_or_default()
        }
        Arma3FieldKind::Strings(_) | Arma3FieldKind::Pair => list(value, atom_numeric_bool),
        Arma3FieldKind::SlowNetwork => list(value, scalar_bool),
        Arma3FieldKind::KickTimeouts
        | Arma3FieldKind::Channels
        | Arma3FieldKind::VoteCommands
        | Arma3FieldKind::VotedAdminCommands => list(value, |row| list(row, atom_literal_bool)),
        Arma3FieldKind::Section(_) => unreachable!("sections render as classes"),
    };
    out.push_str(field.engine_name);
    if array {
        out.push_str("[]");
    }
    out.push_str(" = ");
    out.push_str(&rendered);
    out.push_str(";\n");
}

fn list(value: &Value, item: impl Fn(&Value) -> String) -> String {
    let items = value
        .as_array()
        .map(|items| items.iter().map(&item).collect::<Vec<_>>())
        .unwrap_or_default();
    format!("{{{}}}", items.join(","))
}

fn scalar_bool(value: &Value) -> String {
    if value.as_bool().unwrap_or(false) {
        "1"
    } else {
        "0"
    }
    .into()
}

fn atom_numeric_bool(value: &Value) -> String {
    match value {
        Value::String(text) => quoted(text),
        Value::Bool(_) => scalar_bool(value),
        Value::Number(number) => number_text(number),
        _ => String::new(),
    }
}

fn atom_literal_bool(value: &Value) -> String {
    match value {
        Value::Bool(flag) => if *flag { "true" } else { "false" }.into(),
        other => atom_numeric_bool(other),
    }
}

fn difficulty_name(value: &str) -> String {
    let mut characters = value.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::{Arma3BasicSettings, Arma3ServerSettings};
    use serde_json::json;

    fn server(value: Value) -> Arma3ServerSettings {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn engine_tuples_render_documented_shapes_and_boolean_spellings() {
        let settings = server(json!({
            "role_timeout": [90, 120],
            "kick_clients_on_slow_network": [true, false, true, false],
            "kick_timeout": [[0, -1], [1, 0], [3, -2]],
            "allowed_vote_cmds": [["kick", false, true, 0.51], ["missions", false, false]],
            "allowed_voted_admin_cmds": [["", true, true], ["#debug", false, false]],
            "disable_channels": [[0, true, false], [6, false, true]],
            "timestamp_format": "short"
        }));
        assert_eq!(
            settings.render().unwrap(),
            concat!(
                "allowedVoteCmds[] = {{\"kick\",false,true,0.51},{\"missions\",false,false}};\n",
                "allowedVotedAdminCmds[] = {{\"\",true,true},{\"#debug\",false,false}};\n",
                "kickClientsOnSlowNetwork[] = {1,0,1,0};\n",
                "kickTimeout[] = {{0,-1},{1,0},{3,-2}};\n",
                "roleTimeOut[] = {90,120};\n",
                "disableChannels[] = {{0,true,false},{6,false,true}};\n",
                "timeStampFormat = \"short\";\n",
            )
        );
    }

    #[test]
    fn engine_tuples_reject_unknown_ids_duplicates_and_bad_shapes() {
        for bad in [
            json!({"disable_channels": [[7, false, false]]}),
            json!({"disable_channels": [[0, false, false], [0, true, false]]}),
            json!({"disable_channels": [[0, false, false, true, true]]}),
            json!({"kick_timeout": [[4, 0]]}),
            json!({"kick_timeout": [[1, -3]]}),
            json!({"kick_timeout": [[1, 10], [1, 20]]}),
            json!({"allowed_vote_cmds": [["kick", false]]}),
            json!({"allowed_vote_cmds": [["kick", false, true, 1.5]]}),
            json!({"allowed_vote_cmds": [["", false, true]]}),
            json!({"allowed_voted_admin_cmds": [["bad\";x=1;", false, true]]}),
            json!({"allowed_voted_admin_cmds": [["#", false, true]]}),
            json!({"motd": ["\u{1b}"]}),
            json!({"on_user_connected": "line\nbreak"}),
            json!({"allowed_html_load_uris": ["https://user:pass@repo.example.invalid/"]}),
            json!({"mission_whitelist": ["boot.sqf"]}),
            json!({"admins": ["123"]}),
        ] {
            let parsed = serde_json::from_value::<Arma3ServerSettings>(bad.clone());
            assert!(
                parsed
                    .map(|settings| settings.validate().is_err())
                    .unwrap_or(true),
                "{bad}"
            );
        }
    }

    #[test]
    fn numeric_bounds_keep_wide_bandwidth_negative_sentinels_and_plain_decimals() {
        let basic: Arma3BasicSettings = serde_json::from_value(json!({
            "max_bandwidth": u64::MAX,
            "max_custom_file_size": 0,
            "min_error_to_send": 0.0000001,
            "min_error_to_send_near": 2.0
        }))
        .unwrap();
        assert_eq!(
            basic.render().unwrap(),
            "MaxBandwidth = 18446744073709551615;\nMinErrorToSend = 0.0000001;\nMinErrorToSendNear = 2.0;\nMaxCustomFileSize = 0;\n"
        );
        assert!(
            server(json!({"max_ping": -1, "override_haze_quality": -1}))
                .validate()
                .is_ok()
        );
        assert!(serde_json::from_str::<Arma3BasicSettings>("{\"min_error_to_send\":NaN}").is_err());
        assert!(
            serde_json::from_str::<Arma3BasicSettings>("{\"min_error_to_send\":1e999}").is_err()
        );
        assert!(server(json!({"idle_fps_limit": 61})).validate().is_err());
        assert!(server(json!({"idle_fps_limit": 4})).validate().is_err());
        assert!(server(json!({"disconnect_timeout": 4})).validate().is_err());
        assert!(server(json!({"von_codec_quality": 31})).validate().is_err());
    }

    #[test]
    fn version_gates_follow_documented_introductions_only() {
        let console = server(json!({"timestamp_format_console": "short"}));
        assert!(validate_version(&console, Arma3ServerSettings::FIELDS, 220).is_err());
        assert!(validate_version(&console, Arma3ServerSettings::FIELDS, 222).is_ok());
        let http = server(
            json!({"mission_http_download_base_url": "https://repo.example.invalid/missions/"}),
        );
        assert!(validate_version(&http, Arma3ServerSettings::FIELDS, 218).is_err());
        assert!(validate_version(&http, Arma3ServerSettings::FIELDS, 220).is_ok());
        let undocumented = server(json!({"kick_timeout": [[0, -1]]}));
        assert!(validate_version(&undocumented, Arma3ServerSettings::FIELDS, 100).is_ok());
    }

    #[test]
    fn decimal_input_accepts_one_comma_and_rejects_exponents() {
        assert_eq!(parse_decimal_input("0,35"), Number::from_f64(0.35));
        assert_eq!(parse_decimal_input(" 0.5 "), Number::from_f64(0.5));
        assert_eq!(parse_decimal_input("-1"), Some(Number::from(-1)));
        assert_eq!(parse_decimal_input("1,000.5"), None);
        assert_eq!(parse_decimal_input("1,2,3"), None);
        assert_eq!(parse_decimal_input("1e5"), None);
        assert_eq!(parse_decimal_input("NaN"), None);
        assert_eq!(parse_decimal_input(""), None);
    }

    #[test]
    fn quoting_doubles_quotes_and_keeps_backslashes() {
        assert_eq!(quoted("a\\b\"c"), "\"a\\b\"\"c\"");
    }

    #[test]
    fn unknown_fields_are_refused() {
        #[derive(Serialize)]
        struct Extra {
            extra: bool,
        }
        assert!(validate_section(&Extra { extra: true }, Arma3ServerSettings::FIELDS).is_err());
    }
}
