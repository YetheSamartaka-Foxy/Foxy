use serde_json::Value;
use std::collections::BTreeSet;

use super::model::{
    ReforgerAddonChannel, ReforgerAddonDependency, ReforgerAddonMetadata, ReforgerAddonProvider,
};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_PACKAGE_VERSIONS: usize = 256;
const MAX_PACKAGE_FILES: usize = 100_000;

fn normalize_guid(value: &str) -> Result<String, String> {
    let value = value.to_ascii_lowercase();
    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid addon GUID".into());
    }
    Ok(value)
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("Missing metadata field {key}"))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    field(value, key)?
        .as_str()
        .ok_or_else(|| format!("Invalid metadata field {key}"))
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    String(String),
    Open,
    Close,
    Other(char),
}

fn tokenize_gproj(input: &str) -> Result<Vec<Token>, String> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            byte if byte.is_ascii_whitespace() => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut closed = false;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Err("Unterminated gproj comment".into());
                }
            }
            b'{' => {
                depth += 1;
                tokens.push(Token::Open);
                i += 1;
            }
            b'}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| "Unbalanced gproj braces".to_owned())?;
                tokens.push(Token::Close);
                i += 1;
            }
            quote @ (b'\'' | b'"') => {
                i += 1;
                let mut value = Vec::new();
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                        if i == bytes.len() {
                            return Err("Unterminated gproj escape".into());
                        }
                    }
                    value.push(bytes[i]);
                    i += 1;
                }
                if i == bytes.len() {
                    return Err("Unterminated gproj string".into());
                }
                tokens.push(Token::String(
                    String::from_utf8(value).map_err(|_| "Invalid gproj string encoding")?,
                ));
                i += 1;
            }
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                tokens.push(Token::Word(input[start..i].to_owned()));
            }
            byte if byte.is_ascii_punctuation() => {
                tokens.push(Token::Other(byte as char));
                i += 1;
            }
            _ => i += 1,
        }
    }
    if depth != 0 {
        return Err("Unbalanced gproj braces".into());
    }
    Ok(tokens)
}

fn top_level_value(tokens: &[Token], key: &str) -> Result<Vec<String>, String> {
    let mut depth = 0usize;
    let mut found = None;
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Open => depth += 1,
            Token::Close => depth -= 1,
            Token::Word(word) if depth == 1 && word == key => {
                if found.is_some() {
                    return Err(format!("Duplicate gproj {key} block"));
                }
                if tokens.get(i + 1) != Some(&Token::Open) {
                    return Err(format!("Malformed gproj {key} block"));
                }
                let mut values = Vec::new();
                let mut nested = 1usize;
                i += 2;
                while i < tokens.len() && nested > 0 {
                    match &tokens[i] {
                        Token::Open => {
                            return Err(format!("Nested structure in gproj {key} block"));
                        }
                        Token::Close => nested -= 1,
                        Token::String(value) if nested == 1 => values.push(value.clone()),
                        Token::Word(_) if nested == 1 => {
                            return Err(format!("Unknown value in gproj {key} block"));
                        }
                        Token::Other(_) if nested == 1 => {
                            return Err(format!("Unknown syntax in gproj {key} block"));
                        }
                        _ => {}
                    }
                    i += 1;
                }
                if nested != 0 {
                    return Err(format!("Unclosed gproj {key} block"));
                }
                found = Some(values);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    found.ok_or_else(|| format!("Missing gproj {key} block"))
}

fn project_guid(tokens: &[Token]) -> Result<String, String> {
    let mut depth = 0usize;
    let mut found = None;
    for pair in tokens.windows(2) {
        if pair[0] == Token::Open {
            depth += 1;
            continue;
        }
        if pair[0] == Token::Close {
            depth -= 1;
            continue;
        }
        if depth == 1 && pair[0] == Token::Word("GUID".into()) {
            let Token::String(value) = &pair[1] else {
                return Err("Malformed project GUID".into());
            };
            if found.replace(normalize_guid(value)?).is_some() {
                return Err("Duplicate project GUID".into());
            }
        }
    }
    found.ok_or_else(|| "Missing project GUID".into())
}

pub fn parse_metadata(
    server_data: &[u8],
    gproj: &str,
    channel: ReforgerAddonChannel,
    provider: ReforgerAddonProvider,
    base_guids: &[String],
) -> Result<ReforgerAddonMetadata, String> {
    if server_data.len() > MAX_INPUT_BYTES || gproj.len() > MAX_INPUT_BYTES {
        return Err("Addon metadata input exceeds its byte limit".into());
    }
    let server_data = server_data
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(server_data);
    let gproj = gproj.strip_prefix('\u{feff}').unwrap_or(gproj);
    let json: Value =
        serde_json::from_slice(server_data).map_err(|_| "Invalid server addon metadata JSON")?;
    let id = normalize_guid(text(&json, "id")?)?;
    let revision = field(&json, "revision")?;
    let version = text(revision, "version")?;
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("Invalid addon version".into());
    }
    if field(revision, "corrupted")?.as_bool() != Some(false) {
        return Err("Addon revision is corrupted or has unknown corruption state".into());
    }
    if let Some(supplied_constraints) = revision.get("dependencies") {
        let supplied_constraints = supplied_constraints
            .as_array()
            .ok_or_else(|| "Invalid addon dependency metadata".to_owned())?;
        if !supplied_constraints.is_empty() {
            return Err("Versioned provider dependency metadata requires review; its constraints cannot be discarded.".into());
        }
    }
    let tokens = tokenize_gproj(gproj)?;
    if project_guid(&tokens)? != id {
        return Err("Addon project GUID does not match server metadata".into());
    }
    let bases = base_guids
        .iter()
        .map(|value| normalize_guid(value))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut dependencies = BTreeSet::new();
    for value in top_level_value(&tokens, "Dependencies")? {
        let dep = normalize_guid(&value)?;
        if !bases.contains(&dep) {
            dependencies.insert(dep);
        }
    }
    let scenarios = field(revision, "scenarios")?
        .as_array()
        .ok_or_else(|| "Invalid addon scenarios".to_owned())?
        .iter()
        .map(|scenario| {
            scenario
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "Unknown addon scenario representation".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if scenarios
        .iter()
        .any(|scenario| !super::super::definition::scenario_resource_valid(scenario))
    {
        return Err("Invalid addon scenario resource".into());
    }
    Ok(ReforgerAddonMetadata {
        addon_id: id,
        version: version.to_owned(),
        channel,
        provider,
        dependencies: dependencies
            .into_iter()
            .map(|addon_id| ReforgerAddonDependency {
                addon_id,
                version: None,
            })
            .collect(),
        scenarios,
    })
}

pub fn package_files(meta: &[u8], id: &str, version: &str) -> Result<Vec<(String, u64)>, String> {
    if meta.len() > MAX_INPUT_BYTES {
        return Err("Addon package metadata exceeds its byte limit".into());
    }
    let expected_id = normalize_guid(id)?;
    let meta = meta.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(meta);
    let json: Value =
        serde_json::from_slice(meta).map_err(|_| "Invalid addon package metadata JSON")?;
    let package_meta = field(&json, "meta")?;
    if normalize_guid(text(package_meta, "id")?)? != expected_id {
        return Err("Addon package GUID mismatch".into());
    }
    let selected_revision = field(package_meta, "selectedRev")?
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| "Invalid addon selected revision index".to_owned())?;
    let versions = field(package_meta, "versions")?
        .as_array()
        .ok_or_else(|| "Invalid addon package versions".to_owned())?;
    if versions.len() > MAX_PACKAGE_VERSIONS {
        return Err("Addon package has too many revisions".into());
    }
    let revision = versions
        .get(selected_revision)
        .ok_or_else(|| "Selected addon package revision is out of range".to_owned())?;
    if text(revision, "version")? != version {
        return Err("Addon package selected revision mismatch".into());
    }
    if field(revision, "corrupted")?.as_bool() != Some(false) {
        return Err("Selected addon package revision is corrupted".into());
    }
    let files = field(field(revision, "package")?, "files")?
        .as_array()
        .ok_or_else(|| "Invalid addon package files".to_owned())?;
    if files.len() > MAX_PACKAGE_FILES {
        return Err("Addon package has too many files".into());
    }
    let mut result = Vec::with_capacity(files.len());
    let mut seen = BTreeSet::new();
    let mut total_size = 0u64;
    for file in files {
        if field(file, "isDownloaded")?.as_bool() != Some(true)
            || field(file, "isCorrupted")?.as_bool() != Some(false)
        {
            return Err("Addon package is incomplete or corrupted".into());
        }
        let name = text(file, "name")?;
        if !safe_relative_name(name) || !seen.insert(name.to_ascii_lowercase()) {
            return Err("Unsafe or duplicate addon package filename".into());
        }
        let size = field(file, "size")?
            .as_u64()
            .ok_or_else(|| "Invalid addon package file size".to_owned())?;
        total_size = total_size
            .checked_add(size)
            .ok_or_else(|| "Addon package total size overflow".to_owned())?;
        result.push((name.to_owned(), size));
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

fn safe_relative_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.chars().any(char::is_control)
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "aaaaaaaaaaaaaaaa";
    const DEP: &str = "bbbbbbbbbbbbbbbb";
    const BASE: &str = "cccccccccccccccc";
    const SCENARIO: &str = "{0123456789ABCDEF}missions/main.conf";

    #[test]
    fn reads_only_top_level_gproj_dependencies_and_filters_explicit_base() {
        let json = format!(
            "{{\"id\":\"{ID}\",\"revision\":{{\"version\":\"1.4.3\",\"corrupted\":false,\"dependencies\":[],\"scenarios\":[\"{SCENARIO}\"]}}}}"
        );
        let gproj = format!(
            "GameProject {{ ID 'RepoAlpha' GUID '{ID}' Dependencies {{ // {DEP}\n '{DEP}' '{BASE}' }} Configurations {{ Nested {{ Dependencies {{ '{BASE}' }} }} }} }}"
        );
        let parsed = parse_metadata(
            json.as_bytes(),
            &gproj,
            ReforgerAddonChannel::Stable,
            ReforgerAddonProvider::Local,
            &[BASE.into()],
        )
        .unwrap();
        assert_eq!(
            parsed.dependencies,
            vec![ReforgerAddonDependency {
                addon_id: DEP.into(),
                version: None
            }]
        );
        assert_eq!(parsed.scenarios, vec![SCENARIO]);
    }

    #[test]
    fn rejects_project_mismatch_and_unrecognized_dependency_syntax() {
        let json = format!(
            "{{\"id\":\"{ID}\",\"revision\":{{\"version\":\"1\",\"corrupted\":false,\"scenarios\":[]}}}}"
        );
        assert!(
            parse_metadata(
                json.as_bytes(),
                "GameProject { ID 'RepoAlpha' GUID 'bbbbbbbbbbbbbbbb' Dependencies {} }",
                ReforgerAddonChannel::Stable,
                ReforgerAddonProvider::Local,
                &[]
            )
            .is_err()
        );
        let invalid =
            format!("GameProject {{ ID 'RepoAlpha' GUID '{ID}' Dependencies {{ Unknown }} }}");
        assert!(
            parse_metadata(
                json.as_bytes(),
                &invalid,
                ReforgerAddonChannel::Stable,
                ReforgerAddonProvider::Local,
                &[]
            )
            .is_err()
        );
        let duplicate =
            format!("GameProject {{ ID 'RepoAlpha' GUID '{ID}' GUID '{ID}' Dependencies {{}} }}");
        assert!(
            parse_metadata(
                json.as_bytes(),
                &duplicate,
                ReforgerAddonChannel::Stable,
                ReforgerAddonProvider::Local,
                &[]
            )
            .is_err()
        );
        let punctuation =
            format!("GameProject {{ ID 'RepoAlpha' GUID '{ID}' Dependencies {{ ['{DEP}'] }} }}");
        assert!(
            parse_metadata(
                json.as_bytes(),
                &punctuation,
                ReforgerAddonChannel::Stable,
                ReforgerAddonProvider::Local,
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn preserves_self_dependency_for_cycle_rejection() {
        let json = format!(
            "{{\"id\":\"{ID}\",\"revision\":{{\"version\":\"1\",\"corrupted\":false,\"scenarios\":[]}}}}"
        );
        let gproj =
            format!("GameProject {{ ID 'RepoAlpha' GUID '{ID}' Dependencies {{ '{ID}' }} }}");
        let parsed = parse_metadata(
            json.as_bytes(),
            &gproj,
            ReforgerAddonChannel::Stable,
            ReforgerAddonProvider::Local,
            &[],
        )
        .unwrap();
        assert_eq!(
            parsed.dependencies,
            vec![ReforgerAddonDependency {
                addon_id: ID.into(),
                version: None
            }]
        );
    }

    #[test]
    fn accepts_utf8_bom_and_requires_complete_safe_package_file_list() {
        let server = format!(
            "\u{feff}{{\"id\":\"{ID}\",\"revision\":{{\"version\":\"1\",\"corrupted\":false,\"scenarios\":[]}}}}"
        );
        assert!(
            parse_metadata(
                server.as_bytes(),
                &format!("\u{feff}GameProject {{ ID 'RepoAlpha' GUID '{ID}' Dependencies {{}} }}"),
                ReforgerAddonChannel::Experimental,
                ReforgerAddonProvider::BohemiaWorkshop,
                &[]
            )
            .is_ok()
        );
        let good = format!(
            "{{\"meta\":{{\"id\":\"{ID}\",\"selectedRev\":0,\"versions\":[{{\"version\":\"1\",\"corrupted\":false,\"package\":{{\"files\":[{{\"name\":\"addons/a.pak\",\"size\":7,\"isCorrupted\":false,\"isDownloaded\":true,\"filePath\":\"ignored\"}}]}}}}]}}}}"
        );
        assert_eq!(
            package_files(good.as_bytes(), ID, "1").unwrap(),
            vec![("addons/a.pak".into(), 7)]
        );
        let incomplete = good.replace("\"isDownloaded\":true", "\"isDownloaded\":false");
        assert!(package_files(incomplete.as_bytes(), ID, "1").is_err());
        let traversal = good.replace("addons/a.pak", "../a.pak");
        assert!(package_files(traversal.as_bytes(), ID, "1").is_err());
    }
}
