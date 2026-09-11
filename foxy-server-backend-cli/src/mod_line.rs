use std::path::Path;

use crate::types::{DLC_CODES, DlcContent, ProcessedMod, RepoConfig, RepoGame, ResolvedMod};

/// Shaping options for the generated server launch line.
#[derive(Debug, Clone, Copy)]
pub struct ModLineOptions<'a> {
    /// Path prefix for each mod folder, matching the server-side layout
    /// (e.g. `mods/` for `-mod=mods/@cba_a3`, or the `-addonsDir` root).
    pub prefix: &'a str,
    /// Include optional mods alongside the required ones.
    pub include_optional: bool,
}

/// The server launch line for a generated repository, shaped for the game the
/// config names: Arma 3 gets `-mod=`, Arma Reforger `-addonsDir ... -addons ...`.
/// `sources` are the resolved mods the repository was built from; Reforger mod
/// ids are read from their `.gproj` files.
pub fn build_server_launch_line(
    config: &RepoConfig,
    mods: &[ProcessedMod],
    sources: &[ResolvedMod],
    options: ModLineOptions<'_>,
) -> String {
    match config.game {
        RepoGame::Arma3 => build_mod_line(config.dlc_content.as_ref(), mods, options),
        RepoGame::Reforger => build_reforger_addons_line(mods, sources, options),
    }
}

/// Config keys that only mean something for Arma 3; a Reforger config that
/// sets them is accepted but the operator is told they have no effect.
pub fn game_config_warnings(config: &RepoConfig, sources: &[ResolvedMod]) -> Vec<String> {
    let mut warnings = Vec::new();
    if config.game != RepoGame::Reforger {
        return warnings;
    }
    if config.dlc_content.is_some() {
        warnings.push(
            "dlcContent lists Arma 3 Creator DLCs; Arma Reforger has none and Foxy ignores it"
                .to_string(),
        );
    }
    let client_side: Vec<&str> = sources
        .iter()
        .filter(|m| m.client_side)
        .map(|m| m.mod_name.as_str())
        .collect();
    if !client_side.is_empty() {
        warnings.push(format!(
            "clientSide is set on {}; an Arma Reforger server activates exactly its own mod set, so Foxy offers no client-side marking for it",
            client_side.join(", ")
        ));
    }
    warnings
}

/// Builds the `-addonsDir <root> -addons <id,...>` launch parameters for an
/// Arma Reforger server. `-addons` takes mod ids separated by commas; the id is
/// the `.gproj` GUID, then the `.gproj` project ID, then a Workshop
/// `ServerData.json` id, and finally the folder name, matching the client.
/// Disabled and client-side mods are skipped like in the Arma 3 line.
pub fn build_reforger_addons_line(
    mods: &[ProcessedMod],
    sources: &[ResolvedMod],
    options: ModLineOptions<'_>,
) -> String {
    let mut ids: Vec<String> = Vec::new();
    for m in mods {
        if !m.enabled || m.client_side {
            continue;
        }
        if !m.is_required && !options.include_optional {
            continue;
        }
        let source = sources
            .iter()
            .find(|source| source.mod_name == m.mod_name)
            .map(|source| source.source_path.as_path());
        let id = source
            .and_then(reforger_mod_id)
            .unwrap_or_else(|| m.mod_name.clone());
        if !ids.iter().any(|known| known.eq_ignore_ascii_case(&id)) {
            ids.push(id);
        }
    }

    let prefix = options.prefix.trim().trim_end_matches(['/', '\\']);
    let addons_dir = if prefix.is_empty() {
        ".".to_string()
    } else {
        prefix.replace('\\', "/")
    };
    format!("-addonsDir {} -addons {}", addons_dir, ids.join(","))
}

/// The `-addons` value for a Reforger addon folder: `.gproj` GUID, `.gproj`
/// project ID, then `ServerData.json` id. `None` when none of them is present.
pub fn reforger_mod_id(dir: &Path) -> Option<String> {
    let gproj = read_gproj_ids(dir);
    if let Some(guid) = gproj.as_ref().and_then(|ids| ids.guid.clone()) {
        return Some(guid);
    }
    if let Some(project_id) = gproj.as_ref().and_then(|ids| ids.project_id.clone()) {
        return Some(project_id);
    }
    read_server_data_id(dir)
}

#[derive(Default)]
struct GprojIds {
    guid: Option<String>,
    project_id: Option<String>,
}

/// `.gproj` is an Enfusion config block; both ids are quoted values on their
/// own line, so a line scan avoids a parser for two fields.
fn read_gproj_ids(dir: &Path) -> Option<GprojIds> {
    let path = find_gproj_file(dir)?;
    let raw = std::fs::read_to_string(path).ok()?;
    let mut ids = GprojIds::default();
    for line in raw.lines() {
        let line = line.trim();
        if let Some(value) = quoted_value_after_key(line, "GUID") {
            ids.guid = Some(value.trim_matches(['{', '}']).to_ascii_uppercase());
        } else if let Some(value) = quoted_value_after_key(line, "ID") {
            ids.project_id = Some(value);
        }
    }
    (ids.guid.is_some() || ids.project_id.is_some()).then_some(ids)
}

fn find_gproj_file(dir: &Path) -> Option<std::path::PathBuf> {
    let preferred = dir.join("addon.gproj");
    if preferred.is_file() {
        return Some(preferred);
    }
    let mut candidates: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("gproj"))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

fn quoted_value_after_key(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let value = rest.strip_prefix('"')?.split('"').next()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn read_server_data_id(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join("ServerData.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let id = value.get("id")?.as_str()?.trim();
    (!id.is_empty()).then(|| id.to_ascii_uppercase())
}

/// Builds the `-mod=` launch parameter for a generated repository.
///
/// Creator DLC codes come first (Arma resolves them from the game install),
/// then the repository mod folders. Disabled and client-side mods are never
/// emitted: client-side mods are not meant to be loaded by a server.
pub fn build_mod_line(
    dlc_content: Option<&DlcContent>,
    mods: &[ProcessedMod],
    options: ModLineOptions<'_>,
) -> String {
    let mut entries: Vec<String> = Vec::new();

    if let Some(dlc) = dlc_content {
        for code in DLC_CODES {
            if dlc.is_enabled(code) {
                entries.push(code.to_string());
            }
        }
    }

    let prefix = normalize_prefix(options.prefix);
    for m in mods {
        if !m.enabled || m.client_side {
            continue;
        }
        if !m.is_required && !options.include_optional {
            continue;
        }
        entries.push(format!("{}{}", prefix, m.mod_name));
    }

    let mut line = String::from("-mod=");
    for entry in &entries {
        line.push_str(entry);
        line.push(';');
    }
    line
}

fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{}/", trimmed.replace('\\', "/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Checksums;

    fn processed(name: &str, is_required: bool, enabled: bool, client_side: bool) -> ProcessedMod {
        ProcessedMod {
            mod_name: name.to_string(),
            checksums: Checksums::default(),
            files: Vec::new(),
            is_required,
            enabled,
            client_side,
        }
    }

    fn options(prefix: &str) -> ModLineOptions<'_> {
        ModLineOptions {
            prefix,
            include_optional: false,
        }
    }

    fn resolved(name: &str, source: &Path, client_side: bool) -> ResolvedMod {
        ResolvedMod {
            mod_name: name.to_string(),
            source_path: source.to_path_buf(),
            is_required: true,
            enabled: true,
            client_side,
        }
    }

    fn write_gproj(root: &Path, folder: &str, body: &str) -> std::path::PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).expect("addon dir");
        std::fs::write(dir.join("addon.gproj"), body).expect("gproj");
        dir
    }

    #[test]
    fn empty_repository_yields_bare_flag() {
        assert_eq!(build_mod_line(None, &[], options("")), "-mod=");
    }

    #[test]
    fn reforger_line_reads_gproj_ids_with_fallbacks() {
        let root = tempfile::tempdir().expect("root");
        let guid = write_gproj(
            root.path(),
            "GCUnits",
            "GameProject {\n ID \"GlobalConflictsUnits\"\n GUID \"6156f2f771d5d73d\"\n TITLE \"Units\"\n}\n",
        );
        let project_only = write_gproj(
            root.path(),
            "ProjectOnly",
            "GameProject {\n ID \"ProjectOnly\"\n TITLE \"No GUID\"\n}\n",
        );
        let workshop = root.path().join("596ABCDEF0123456");
        std::fs::create_dir_all(&workshop).expect("workshop dir");
        std::fs::write(
            workshop.join("ServerData.json"),
            r#"{"id":"596abcdef0123456","name":"Capture"}"#,
        )
        .expect("server data");
        let bare = root.path().join("BareFolder");
        std::fs::create_dir_all(&bare).expect("bare dir");

        let mods = [
            processed("GCUnits", true, true, false),
            processed("ProjectOnly", true, true, false),
            processed("596ABCDEF0123456", true, true, false),
            processed("BareFolder", true, true, false),
            processed("ClientOnly", true, true, true),
            processed("Disabled", true, false, false),
        ];
        let sources = [
            resolved("GCUnits", &guid, false),
            resolved("ProjectOnly", &project_only, false),
            resolved("596ABCDEF0123456", &workshop, false),
            resolved("BareFolder", &bare, false),
            resolved("ClientOnly", &guid, true),
            resolved("Disabled", &guid, false),
        ];

        assert_eq!(
            build_reforger_addons_line(&mods, &sources, options("mods\\reforger\\")),
            "-addonsDir mods/reforger -addons 6156F2F771D5D73D,ProjectOnly,596ABCDEF0123456,BareFolder"
        );
        assert_eq!(
            build_reforger_addons_line(&mods[..1], &sources[..1], options("")),
            "-addonsDir . -addons 6156F2F771D5D73D"
        );
    }

    #[test]
    fn server_launch_line_follows_the_config_game() {
        let root = tempfile::tempdir().expect("root");
        let dir = write_gproj(
            root.path(),
            "Mod",
            "GameProject {\n ID \"Mod\"\n GUID \"88037E46AD234C72\"\n}\n",
        );
        let mods = [processed("Mod", true, true, false)];
        let sources = [resolved("Mod", &dir, false)];
        let mut config: RepoConfig =
            serde_json::from_str(r#"{"repoName":"Test","basePath":".","dlcContent":["gm"]}"#)
                .expect("config");

        assert_eq!(
            build_server_launch_line(&config, &mods, &sources, options("")),
            "-mod=gm;Mod;"
        );
        assert!(game_config_warnings(&config, &sources).is_empty());

        config.game = RepoGame::Reforger;
        assert_eq!(
            build_server_launch_line(&config, &mods, &sources, options("")),
            "-addonsDir . -addons 88037E46AD234C72"
        );
        let warnings = game_config_warnings(&config, &sources);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("dlcContent"));
    }

    #[test]
    fn dlc_codes_precede_mod_folders() {
        let dlc = DlcContent {
            ws: true,
            gm: true,
            ..DlcContent::default()
        };
        let mods = [processed("@cba_a3", true, true, false)];
        assert_eq!(
            build_mod_line(Some(&dlc), &mods, options("mods")),
            "-mod=gm;ws;mods/@cba_a3;"
        );
    }

    #[test]
    fn prefix_is_normalized_to_forward_slashes() {
        let mods = [processed("@ace", true, true, false)];
        assert_eq!(
            build_mod_line(None, &mods, options("a3\\mods\\")),
            "-mod=a3/mods/@ace;"
        );
    }

    #[test]
    fn disabled_and_client_side_mods_are_skipped() {
        let mods = [
            processed("@cba_a3", true, true, false),
            processed("@disabled", true, false, false),
            processed("@client", true, true, true),
        ];
        assert_eq!(build_mod_line(None, &mods, options("")), "-mod=@cba_a3;");
    }

    #[test]
    fn optional_mods_are_opt_in() {
        let mods = [
            processed("@cba_a3", true, true, false),
            processed("@extra", false, true, false),
        ];
        assert_eq!(build_mod_line(None, &mods, options("")), "-mod=@cba_a3;");
        assert_eq!(
            build_mod_line(
                None,
                &mods,
                ModLineOptions {
                    prefix: "",
                    include_optional: true,
                },
            ),
            "-mod=@cba_a3;@extra;"
        );
    }
}
