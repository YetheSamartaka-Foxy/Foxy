use log::{debug, info, warn};
use std::collections::HashSet;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;

/// A mission found on disk in a profile's missions/ or mpmissions/ directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorMission {
    /// Display name extracted from mission.sqm briefingName,
    /// or fallback to folder name prefix.
    pub display_name: String,
    /// World/terrain name (e.g., "Altis", "Tanoa", "Stratis").
    pub world_name: String,
    /// The raw folder name (e.g., "my_mission.Altis").
    pub folder_name: String,
    /// Name of the root mission directory ("missions" or "mpmissions").
    pub root_folder_name: String,
    /// Relative parent path below the root mission directory.
    pub relative_parent: PathBuf,
    /// Full path to the mission folder.
    pub path: PathBuf,
    /// Full path to mission.sqm inside this folder.
    pub sqm_path: PathBuf,
    /// Whether this is an MP mission (from mpmissions/) or SP (from missions/).
    pub is_multiplayer: bool,
    /// Author from mission.sqm ScenarioData, if available.
    pub author: Option<String>,
    /// Game type from ScenarioData (e.g., "Coop", "TvT").
    pub game_type: Option<String>,
    /// Max players from ScenarioData.
    pub max_players: Option<u32>,
    /// Last modification time of mission.sqm (used for sort order).
    pub last_modified: SystemTime,
}

/// Scan a profile directory for all editor missions.
///
/// Looks in both `<profile_path>/missions/` and `<profile_path>/mpmissions/`.
/// Returns missions sorted by last_modified descending (newest first).
pub fn scan_profile_missions(profile_path: &Path) -> Vec<EditorMission> {
    let mut missions = Vec::new();

    let sp_dir = profile_path.join("missions");
    if sp_dir.is_dir() {
        scan_mission_directory(&sp_dir, &sp_dir, "missions", false, &mut missions);
    }

    let mp_dir = profile_path.join("mpmissions");
    if mp_dir.is_dir() {
        scan_mission_directory(&mp_dir, &mp_dir, "mpmissions", true, &mut missions);
    }

    missions.sort_by_key(|mission| std::cmp::Reverse(mission.last_modified));

    // Avoid log spam: only emit the scan summary the first time we see a given
    // (profile, count) result. Repeated scans with an unchanged count stay quiet,
    // while a changed mission count still produces a fresh log line.
    static LOGGED_SCANS: OnceLock<Mutex<HashSet<(PathBuf, usize)>>> = OnceLock::new();
    let key = (profile_path.to_path_buf(), missions.len());
    let should_log = LOGGED_SCANS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|mut seen| seen.insert(key))
        .unwrap_or(true);

    if should_log {
        info!(
            "Scanned {} editor mission(s) from {}",
            missions.len(),
            profile_path.display()
        );
    } else {
        debug!(
            "Scanned {} editor mission(s) from {} (suppressed duplicate)",
            missions.len(),
            profile_path.display()
        );
    }
    missions
}

/// Remove editor addon dependencies from a mission.sqm `addons[]` array.
pub fn remove_mission_addon_dependencies(sqm_path: &Path) -> Result<bool, String> {
    let content =
        fs::read_to_string(sqm_path).map_err(|err| format!("Failed to read mission.sqm: {err}"))?;

    let updated = remove_addon_dependencies_from_sqm(&content)?;
    let Some(updated) = updated else {
        return Err("addons[] array was not found in mission.sqm.".to_string());
    };

    if updated == content {
        return Ok(false);
    }

    fs::write(sqm_path, updated).map_err(|err| format!("Failed to write mission.sqm: {err}"))?;

    Ok(true)
}

fn remove_addon_dependencies_from_sqm(content: &str) -> Result<Option<String>, String> {
    let Some(range) = find_addons_array_assignment(content)? else {
        return Ok(None);
    };

    let indent = line_indent_before(content, range.start);
    let line_ending = preferred_line_ending(content);
    let replacement = format!("addons[]={line_ending}{indent}{{{line_ending}{indent}}};");

    let mut updated = String::with_capacity(content.len());
    updated.push_str(&content[..range.start]);
    updated.push_str(&replacement);
    updated.push_str(&content[range.end..]);

    Ok(Some(updated))
}

fn find_addons_array_assignment(content: &str) -> Result<Option<Range<usize>>, String> {
    let bytes = content.as_bytes();
    let mut search_from = 0;

    while let Some(start) = find_next_addons_identifier(content, search_from) {
        search_from = start + "addons".len();

        if !is_identifier_boundary(bytes.get(start.wrapping_sub(1)).copied())
            || !is_identifier_boundary(bytes.get(search_from).copied())
        {
            continue;
        }

        let mut cursor = skip_ascii_whitespace(bytes, search_from);
        if bytes.get(cursor) != Some(&b'[') {
            continue;
        }
        cursor = skip_ascii_whitespace(bytes, cursor + 1);
        if bytes.get(cursor) != Some(&b']') {
            continue;
        }
        cursor = skip_ascii_whitespace(bytes, cursor + 1);
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor = skip_ascii_whitespace(bytes, cursor + 1);
        if bytes.get(cursor) != Some(&b'{') {
            continue;
        }

        let close_brace = find_matching_brace(content, cursor)
            .ok_or_else(|| "addons[] array is missing a closing brace.".to_string())?;
        let semicolon = skip_ascii_whitespace(bytes, close_brace + 1);
        if bytes.get(semicolon) != Some(&b';') {
            return Err("addons[] array is missing a trailing semicolon.".to_string());
        }

        return Ok(Some(start..semicolon + 1));
    }

    Ok(None)
}

fn find_next_addons_identifier(content: &str, start_from: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut cursor = start_from;
    let mut state = SqmScanState::Normal;

    while cursor < bytes.len() {
        match state {
            SqmScanState::Normal => {
                if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/') {
                    state = SqmScanState::LineComment;
                    cursor += 2;
                    continue;
                }
                if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
                    state = SqmScanState::BlockComment;
                    cursor += 2;
                    continue;
                }
                if bytes[cursor] == b'"' {
                    state = SqmScanState::String;
                } else if bytes[cursor..].starts_with(b"addons")
                    && is_identifier_boundary(bytes.get(cursor.wrapping_sub(1)).copied())
                    && is_identifier_boundary(bytes.get(cursor + "addons".len()).copied())
                {
                    return Some(cursor);
                }
            }
            SqmScanState::String => match bytes[cursor] {
                b'\\' => {
                    cursor += 2;
                    continue;
                }
                b'"' if bytes.get(cursor + 1) == Some(&b'"') => {
                    cursor += 2;
                    continue;
                }
                b'"' => state = SqmScanState::Normal,
                _ => {}
            },
            SqmScanState::LineComment => {
                if bytes[cursor] == b'\n' {
                    state = SqmScanState::Normal;
                }
            }
            SqmScanState::BlockComment => {
                if bytes[cursor] == b'*' && bytes.get(cursor + 1) == Some(&b'/') {
                    state = SqmScanState::Normal;
                    cursor += 2;
                    continue;
                }
            }
        }

        cursor += 1;
    }

    None
}

fn find_matching_brace(content: &str, open_brace: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut cursor = open_brace;
    let mut depth = 0usize;
    let mut state = SqmScanState::Normal;

    while cursor < bytes.len() {
        match state {
            SqmScanState::Normal => match bytes[cursor] {
                b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                    state = SqmScanState::LineComment;
                    cursor += 2;
                    continue;
                }
                b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                    state = SqmScanState::BlockComment;
                    cursor += 2;
                    continue;
                }
                b'"' => state = SqmScanState::String,
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(cursor);
                    }
                }
                _ => {}
            },
            SqmScanState::String => match bytes[cursor] {
                b'\\' => {
                    cursor += 2;
                    continue;
                }
                b'"' if bytes.get(cursor + 1) == Some(&b'"') => {
                    cursor += 2;
                    continue;
                }
                b'"' => state = SqmScanState::Normal,
                _ => {}
            },
            SqmScanState::LineComment => {
                if bytes[cursor] == b'\n' {
                    state = SqmScanState::Normal;
                }
            }
            SqmScanState::BlockComment => {
                if bytes[cursor] == b'*' && bytes.get(cursor + 1) == Some(&b'/') {
                    state = SqmScanState::Normal;
                    cursor += 2;
                    continue;
                }
            }
        }

        cursor += 1;
    }

    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SqmScanState {
    Normal,
    String,
    LineComment,
    BlockComment,
}

fn skip_ascii_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        cursor += 1;
    }
    cursor
}

fn is_identifier_boundary(byte: Option<u8>) -> bool {
    !byte.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn line_indent_before(content: &str, index: usize) -> &str {
    let line_start = content[..index].rfind('\n').map_or(0, |pos| pos + 1);
    let indent_end = content[line_start..index]
        .find(|ch: char| !ch.is_ascii_whitespace() || ch == '\r' || ch == '\n')
        .map_or(index, |offset| line_start + offset);
    &content[line_start..indent_end]
}

fn preferred_line_ending(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Recursively scan a directory below missions/ or mpmissions/ for mission folders.
fn scan_mission_directory(
    dir: &Path,
    root_dir: &Path,
    root_folder_name: &str,
    is_multiplayer: bool,
    out: &mut Vec<EditorMission>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read mission directory {}: {}", dir.display(), e);
            return;
        }
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let folder_name = entry.file_name().to_string_lossy().to_string();

        let sqm_path = entry_path.join("mission.sqm");
        if !sqm_path.is_file() {
            scan_mission_directory(&entry_path, root_dir, root_folder_name, is_multiplayer, out);
            continue;
        }

        let world_name = match folder_name.rsplit_once('.') {
            Some((_, world)) if !world.is_empty() => world.to_string(),
            _ => {
                debug!(
                    "Skipping {} - cannot determine world from folder name",
                    folder_name
                );
                continue;
            }
        };

        let last_modified = fs::metadata(&sqm_path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let metadata = parse_mission_sqm_metadata(&sqm_path);

        let desc_ext_name = parse_description_ext_name(&entry_path.join("description.ext"));

        let display_name = desc_ext_name
            .or(metadata.briefing_name.clone())
            .unwrap_or_else(|| {
                folder_name
                    .rsplit_once('.')
                    .map(|(prefix, _)| prefix.to_string())
                    .unwrap_or_else(|| folder_name.clone())
            });

        out.push(EditorMission {
            display_name,
            world_name,
            folder_name,
            root_folder_name: root_folder_name.to_string(),
            relative_parent: entry_path
                .parent()
                .and_then(|parent| parent.strip_prefix(root_dir).ok())
                .unwrap_or(Path::new(""))
                .to_path_buf(),
            path: entry_path,
            sqm_path,
            is_multiplayer,
            author: metadata.author,
            game_type: metadata.game_type,
            max_players: metadata.max_players,
            last_modified,
        });
    }
}

/// Lightweight metadata extracted from mission.sqm.
#[derive(Debug, Default)]
struct MissionSqmMetadata {
    briefing_name: Option<String>,
    author: Option<String>,
    game_type: Option<String>,
    max_players: Option<u32>,
}

/// Parse mission.sqm for key metadata fields.
///
/// This is a lightweight parser that reads the file as text and uses
/// simple line-by-line pattern matching to extract the fields we need.
/// Only reads the first ~8KB to keep it fast for large mission files.
fn parse_mission_sqm_metadata(sqm_path: &Path) -> MissionSqmMetadata {
    let mut meta = MissionSqmMetadata::default();

    let content = match fs::read_to_string(sqm_path) {
        Ok(c) => c,
        Err(_) => return meta,
    };

    let content = if content.len() > 8192 {
        &content[..8192]
    } else {
        &content
    };

    for line in content.lines() {
        let trimmed = line.trim();

        if meta.briefing_name.is_none()
            && let Some(val) = extract_quoted_value(trimmed, "briefingName")
        {
            meta.briefing_name = Some(val);
        }

        if meta.author.is_none()
            && let Some(val) = extract_quoted_value(trimmed, "author")
        {
            meta.author = Some(val);
        }

        if let Some(val) = extract_quoted_value(trimmed, "gameType") {
            meta.game_type = Some(val);
        }

        if let Some(val) = extract_numeric_value(trimmed, "maxPlayers") {
            meta.max_players = Some(val);
        }
    }

    meta
}

/// Extract a quoted string value from a line like `key="value";`
fn extract_quoted_value(line: &str, key: &str) -> Option<String> {
    let pattern = format!("{}=\"", key);
    if let Some(start) = line.find(&pattern) {
        let rest = &line[start + pattern.len()..];
        if let Some(end) = rest.find('"') {
            let value = &rest[..end];
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Extract a numeric value from a line like `key=42;`
fn extract_numeric_value(line: &str, key: &str) -> Option<u32> {
    let pattern = format!("{}=", key);
    if let Some(start) = line.find(&pattern) {
        let rest = &line[start + pattern.len()..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return num_str.parse().ok();
    }
    None
}

/// Parse description.ext for mission name overrides.
fn parse_description_ext_name(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;

    for key in &["onLoadName", "briefingName"] {
        if let Some(val) = extract_quoted_value_ext(&content, key) {
            return Some(val);
        }
    }

    None
}

/// Extract a value from description.ext.
/// Format: `key = "value";` (spaces around = are common here)
fn extract_quoted_value_ext(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('"')
                    && let Some(end) = rest.find('"')
                {
                    let value = &rest[..end];
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Format a SystemTime as a human-readable relative date string.
pub fn format_mission_date(time: SystemTime) -> String {
    let now = SystemTime::now();
    let duration = match now.duration_since(time) {
        Ok(d) => d,
        Err(_) => return "just now".to_string(),
    };

    let secs = duration.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 86400 * 30 {
        format!("{}d ago", secs / 86400)
    } else {
        let days = secs / 86400;
        if days < 365 {
            format!("{}mo ago", days / 30)
        } else {
            format!("{}y ago", days / 365)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_world_from_folder_name() {
        assert_eq!(
            "patrol_mission.Altis".rsplit_once('.').map(|(_, w)| w),
            Some("Altis")
        );
        assert_eq!(
            "co08_rescue.Tanoa".rsplit_once('.').map(|(_, w)| w),
            Some("Tanoa")
        );
        assert_eq!(
            "my.complex.name.Stratis".rsplit_once('.').map(|(_, w)| w),
            Some("Stratis")
        );
    }

    #[test]
    fn scan_profile_missions_finds_nested_missions() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir
            .path()
            .join("missions")
            .join("Campaign")
            .join("chapter_one.Stratis");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("mission.sqm"),
            r#"class ScenarioData { briefingName="Nested Mission"; };"#,
        )
        .unwrap();

        let missions = scan_profile_missions(dir.path());

        assert_eq!(missions.len(), 1);
        assert_eq!(missions[0].display_name, "Nested Mission");
        assert_eq!(missions[0].folder_name, "chapter_one.Stratis");
        assert_eq!(missions[0].root_folder_name, "missions");
        assert_eq!(missions[0].relative_parent, PathBuf::from("Campaign"));
    }

    #[test]
    fn remove_addon_dependencies_clears_populated_array() {
        let content = r#"version=54;
addons[]=
{
    "ace_main",
    "rhs_main"
};
addonsAuto[]=
{
    "keep_this"
};
"#;

        let updated = remove_addon_dependencies_from_sqm(content)
            .unwrap()
            .unwrap();

        assert!(updated.contains("addons[]=\n{\n};"));
        assert!(!updated.contains("ace_main"));
        assert!(!updated.contains("rhs_main"));
        assert!(updated.contains("addonsAuto[]="));
        assert!(updated.contains("keep_this"));
    }

    #[test]
    fn remove_addon_dependencies_preserves_indent_and_line_endings() {
        let content =
            "class Mission\r\n{\r\n\taddons[]=\r\n\t{\r\n\t\t\"ace_main\"\r\n\t};\r\n};\r\n";

        let updated = remove_addon_dependencies_from_sqm(content)
            .unwrap()
            .unwrap();

        assert!(updated.contains("\taddons[]=\r\n\t{\r\n\t};"));
        assert!(updated.ends_with("};\r\n"));
    }

    #[test]
    fn remove_addon_dependencies_handles_already_empty_array() {
        let content = "addons[]=\n{\n};\n";

        let updated = remove_addon_dependencies_from_sqm(content)
            .unwrap()
            .unwrap();

        assert_eq!(updated, content);
    }

    #[test]
    fn remove_addon_dependencies_ignores_comments_and_strings() {
        let content = r#"// addons[]={"commented"};
class Note
{
    text="addons[]={""quoted""};";
};
addons[]=
{
    "real_dependency"
};
"#;

        let updated = remove_addon_dependencies_from_sqm(content)
            .unwrap()
            .unwrap();

        assert!(updated.contains("// addons[]={\"commented\"};"));
        assert!(updated.contains("text=\"addons[]={\"\"quoted\"\"};\";"));
        assert!(!updated.contains("real_dependency"));
        assert!(updated.ends_with("addons[]=\n{\n};\n"));
    }

    #[test]
    fn remove_mission_addon_dependencies_updates_file() {
        let dir = tempfile::tempdir().unwrap();
        let sqm_path = dir.path().join("mission.sqm");
        fs::write(
            &sqm_path,
            "addons[]=\n{\n    \"ace_main\"\n};\nclass ScenarioData {};",
        )
        .unwrap();

        assert!(remove_mission_addon_dependencies(&sqm_path).unwrap());

        let updated = fs::read_to_string(sqm_path).unwrap();
        assert_eq!(updated, "addons[]=\n{\n};\nclass ScenarioData {};");
    }

    #[test]
    fn remove_addon_dependencies_reports_missing_array() {
        let content = "addonsAuto[]={\"keep\"};";

        assert!(
            remove_addon_dependencies_from_sqm(content)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn extract_quoted_value_basic() {
        assert_eq!(
            extract_quoted_value("briefingName=\"Operation Thunderstrike\";", "briefingName"),
            Some("Operation Thunderstrike".to_string())
        );
    }

    #[test]
    fn extract_quoted_value_empty() {
        assert_eq!(
            extract_quoted_value("briefingName=\"\";", "briefingName"),
            None
        );
    }

    #[test]
    fn extract_numeric_value_basic() {
        assert_eq!(
            extract_numeric_value("maxPlayers=8;", "maxPlayers"),
            Some(8)
        );
    }

    #[test]
    fn extract_numeric_value_missing() {
        assert_eq!(extract_numeric_value("minPlayers=1;", "maxPlayers"), None);
    }

    #[test]
    fn extract_quoted_value_ext_with_spaces() {
        let content = "onLoadName = \"My Mission\";";
        assert_eq!(
            extract_quoted_value_ext(content, "onLoadName"),
            Some("My Mission".to_string())
        );
    }

    #[test]
    fn format_mission_date_recent() {
        let now = SystemTime::now();
        assert_eq!(format_mission_date(now), "just now");
    }
}
