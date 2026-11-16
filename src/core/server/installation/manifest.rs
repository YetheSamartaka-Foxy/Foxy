use std::collections::HashSet;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 16;
const MAX_TOKENS: usize = 20_000;
const MAX_STRING_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct ManifestObservation {
    pub app_id: u32,
    pub build_id: String,
    pub install_dir: String,
    pub branch: Option<String>,
    pub branch_conflict: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    String(String),
    Open,
    Close,
}

#[derive(Debug, PartialEq, Eq)]
enum Value {
    Scalar(String),
    Object(Vec<(String, Value)>),
}

pub(super) fn parse_manifest(bytes: &[u8], expected_app: u32) -> Option<ManifestObservation> {
    if bytes.len() > MAX_BYTES || expected_app == 0 {
        return None;
    }
    let tokens = tokenize(bytes)?;
    let mut cursor = 0;
    let (root_key, root_value) = parse_pair(&tokens, &mut cursor, 0)?;
    if root_key != "AppState" || cursor != tokens.len() {
        return None;
    }
    let Value::Object(root) = root_value else {
        return None;
    };
    let app_id_text = direct_scalar(&root, "appid")?;
    if !positive_digits(app_id_text, 10) {
        return None;
    }
    let app_id = app_id_text.parse::<u32>().ok()?;
    if app_id == 0 || app_id != expected_app {
        return None;
    }
    let build_id = direct_scalar(&root, "buildid")?;
    if !positive_digits(build_id, 20) {
        return None;
    }
    let install_dir = direct_scalar(&root, "installdir")?;
    if !safe_component(install_dir) {
        return None;
    }

    let mut branch_values = Vec::new();
    for key in ["UserConfig", "MountedConfig"] {
        if let Some(value) = direct_value(&root, key) {
            let Value::Object(config) = value else {
                return None;
            };
            if let Some(value) = direct_value(config, "BetaKey") {
                let Value::Scalar(branch) = value else {
                    return None;
                };
                branch_values.push(if branch.is_empty() {
                    "public"
                } else {
                    branch.as_str()
                });
            }
        }
    }
    let branch_conflict = branch_values.windows(2).any(|pair| pair[0] != pair[1]);
    let branch = if branch_conflict {
        None
    } else {
        branch_values
            .first()
            .and_then(|value| valid_branch(value).then(|| (*value).to_owned()))
    };
    Some(ManifestObservation {
        app_id,
        build_id: build_id.to_owned(),
        install_dir: install_dir.to_owned(),
        branch,
        branch_conflict,
    })
}

fn tokenize(bytes: &[u8]) -> Option<Vec<Token>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b if b.is_ascii_whitespace() => pos += 1,
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'{' => {
                tokens.push(Token::Open);
                pos += 1;
            }
            b'}' => {
                tokens.push(Token::Close);
                pos += 1;
            }
            b'"' => {
                pos += 1;
                let mut value = String::new();
                loop {
                    let byte = *bytes.get(pos)?;
                    match byte {
                        b'"' => {
                            pos += 1;
                            break;
                        }
                        b'\\' => {
                            pos += 1;
                            let escaped = *bytes.get(pos)?;
                            match escaped {
                                b'\\' => value.push('\\'),
                                b'"' => value.push('"'),
                                b'n' => value.push('\n'),
                                b'r' => value.push('\r'),
                                b't' => value.push('\t'),
                                _ => return None,
                            }
                            pos += 1;
                        }
                        0x00..=0x1f => return None,
                        _ => {
                            let ch = text[pos..].chars().next()?;
                            value.push(ch);
                            pos += ch.len_utf8();
                        }
                    }
                    if value.len() > MAX_STRING_BYTES {
                        return None;
                    }
                }
                tokens.push(Token::String(value));
            }
            _ => return None,
        }
        if tokens.len() > MAX_TOKENS {
            return None;
        }
    }
    Some(tokens)
}

fn parse_pair(tokens: &[Token], cursor: &mut usize, depth: usize) -> Option<(String, Value)> {
    let Token::String(key) = tokens.get(*cursor)? else {
        return None;
    };
    let key = key.clone();
    *cursor += 1;
    let value = match tokens.get(*cursor)? {
        Token::String(value) => {
            *cursor += 1;
            Value::Scalar(value.clone())
        }
        Token::Open => {
            if depth >= MAX_DEPTH {
                return None;
            }
            *cursor += 1;
            let mut entries = Vec::new();
            let mut keys = HashSet::new();
            loop {
                match tokens.get(*cursor)? {
                    Token::Close => {
                        *cursor += 1;
                        break;
                    }
                    Token::String(key) => {
                        if !keys.insert(key.to_ascii_lowercase()) {
                            return None;
                        }
                        entries.push(parse_pair(tokens, cursor, depth + 1)?);
                    }
                    _ => return None,
                }
            }
            Value::Object(entries)
        }
        Token::Close => return None,
    };
    Some((key, value))
}

fn direct_value<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    entries
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

fn direct_scalar<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    match direct_value(entries, key)? {
        Value::Scalar(value) => Some(value),
        Value::Object(_) => None,
    }
}

fn positive_digits(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.bytes().any(|byte| byte != b'0')
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.trim() == value
        && !value.ends_with('.')
        && !value.bytes().any(|byte| {
            matches!(
                byte,
                b'/' | b'\\' | b':' | b'<' | b'>' | b'|' | b'?' | b'*' | b'"'
            ) || byte < 0x20
                || byte == 0x7f
        })
}

fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Option<ManifestObservation> {
        parse_manifest(source.as_bytes(), 1234)
    }

    const BASE: &str =
        "\"AppState\" { \"appid\" \"1234\" \"buildid\" \"98765\" \"installdir\" \"GameServer\" }";

    #[test]
    fn parses_public_and_beta_observations() {
        let public = parse("\"AppState\" { \"appid\" \"1234\" \"buildid\" \"98765\" \"installdir\" \"GameServer\" \"UserConfig\" { \"BetaKey\" \"\" } }").unwrap();
        assert_eq!(public.branch.as_deref(), Some("public"));
        assert!(!public.branch_conflict);
        let equivalent = parse(r#""AppState" { "appid" "1234" "buildid" "98765" "installdir" "GameServer" "UserConfig" { "BetaKey" "" } "MountedConfig" { "BetaKey" "public" } }"#).unwrap();
        assert_eq!(equivalent.branch.as_deref(), Some("public"));
        assert!(!equivalent.branch_conflict);
        let beta = parse("\"AppState\" { \"appid\" \"1234\" \"buildid\" \"98765\" \"installdir\" \"GameServer\" \"UserConfig\" { \"BetaKey\" \"stable_branch-1\" } }").unwrap();
        assert_eq!(beta.branch.as_deref(), Some("stable_branch-1"));
    }

    #[test]
    fn decodes_ignored_windows_metadata_and_sanitizes_decoded_fields() {
        let source = r#""AppState" { "appid" "1234" "buildid" "98765" "installdir" "GameServer" "LauncherPath" "C:\\Steam\\Launcher.exe" }"#;
        assert!(parse(source).is_some());
        let escaped_separator =
            r#""AppState" { "appid" "1234" "buildid" "98765" "installdir" "Game\\..\\escape" }"#;
        assert!(parse(escaped_separator).is_none());
        assert!(parse(r#""AppState" { "appid" "1234" "buildid" "1" "installdir" "x" "LauncherPath" "bad\q" }"#).is_none());
        assert!(parse(r#""AppState" { "appid" "1234" "buildid" "1" "installdir" "x" "LauncherPath" "truncated\" }"#).is_none());
    }

    #[test]
    fn ignores_nested_impostors_and_detects_branch_disagreement() {
        let nested = parse("\"AppState\" { \"appid\" \"1234\" \"buildid\" \"98765\" \"installdir\" \"GameServer\" \"Other\" { \"BetaKey\" \"secret\" } }").unwrap();
        assert_eq!(nested.branch, None);
        let conflict = parse("\"AppState\" { \"appid\" \"1234\" \"buildid\" \"98765\" \"installdir\" \"GameServer\" \"UserConfig\" { \"BetaKey\" \"public\" } \"MountedConfig\" { \"BetaKey\" \"beta\" } }").unwrap();
        assert_eq!(conflict.branch, None);
        assert!(conflict.branch_conflict);
        assert!(parse(BASE).unwrap().branch.is_none());
    }

    #[test]
    fn rejects_wrong_app_duplicate_keys_and_unsafe_install_dir() {
        assert!(parse_manifest(BASE.replace("1234", "1235").as_bytes(), 1234).is_none());
        assert!(parse("\"AppState\" { \"appid\" \"1234\" \"APPID\" \"1234\" \"buildid\" \"1\" \"installdir\" \"x\" }").is_none());
        assert!(parse(&BASE.replace("GameServer", "../escape")).is_none());
        assert!(parse(&BASE.replace("GameServer", "..\\escape")).is_none());
        for value in [
            "C:",
            "Server:stream",
            "Server.",
            "Server ",
            " Server",
            "Server*",
            "Server?",
        ] {
            assert!(
                parse(&BASE.replace("GameServer", value)).is_none(),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_malformed_trailing_and_resource_excess() {
        for source in [
            "\"AppState\" { \"appid\" \"1234\"",
            "\"AppState\" { } trailing",
            "\"AppState\" { \"appid\" }",
            "\"AppState\" { \"appid\" \"1234\" } }",
        ] {
            assert!(parse(source).is_none(), "{source}");
        }
        let deep = format!(
            "\"AppState\" {{ \"appid\" \"1234\" {} \"x\" \"v\" {} \"buildid\" \"1\" \"installdir\" \"x\" }}",
            "\"n\" { ".repeat(16),
            "}".repeat(16)
        );
        assert!(parse(&deep).is_none());
        let many = format!(
            "\"AppState\" {{ \"appid\" \"1234\" {} \"buildid\" \"1\" \"installdir\" \"x\" }}",
            (0..10_000)
                .map(|n| format!("\"k{n}\" \"v\" "))
                .collect::<String>()
        );
        assert!(parse(&many).is_none());
        assert!(parse_manifest(&vec![b' '; MAX_BYTES + 1], 1234).is_none());
        assert!(
            parse(&format!(
                "\"AppState\" {{ \"appid\" \"1234\" \"buildid\" \"1\" \"installdir\" \"{}\" }}",
                "x".repeat(MAX_STRING_BYTES + 1)
            ))
            .is_none()
        );
    }
}
