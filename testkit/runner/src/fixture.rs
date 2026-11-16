use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

pub fn metadata(address: &str) -> Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let address = format!("{}/", address.trim_end_matches('/'));
    let primary = client
        .get(format!("{address}foxy_addons.json"))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(|r| r.json::<Value>());
    if let Ok(value) = primary
        && mod_count(&value) > 0
    {
        return Ok(value);
    }
    Ok(client
        .get(format!("{address}repo.json"))
        .send()?
        .error_for_status()?
        .json()?)
}

pub fn mod_count(metadata: &Value) -> usize {
    ["requiredMods", "optionalMods"]
        .iter()
        .map(|key| metadata[*key].as_array().map_or(0, Vec::len))
        .sum()
}

fn generated(case: &Value) -> Result<Value> {
    ensure!(
        case["repository"].is_object(),
        "A case needs fixture.files or repository"
    );
    let mut repositories = vec![generated_repository(&case["repository"])?];
    for extra in case["extra_repositories"].as_array().into_iter().flatten() {
        repositories.push(generated_repository(extra)?);
    }
    let spaces: Vec<Value> = case
        .get("space")
        .filter(|v| !v.is_null())
        .cloned()
        .into_iter()
        .collect();
    let mut settings = json!({"auto_recheck_on_launch":false,"auto_quick_scan_on_launch":false,"swifty_migration_offered":true});
    // `settings` overrides land in the generated settings file, so a case can
    // pin a bandwidth cap or a hash profile without a hand-written fixture.
    if let Some(overrides) = case["settings"].as_object() {
        for (key, value) in overrides {
            settings[key] = value.clone();
        }
    }
    Ok(json!({"files": {
        "settings.json":settings,
        "repositories.json":repositories, "repository_spaces.json":spaces
    }}))
}

/// One `repositories.json` entry for a `{name,address,path,space_id}` source,
/// with the repository's published addons enabled.
fn generated_repository(source: &Value) -> Result<Value> {
    let address = format!(
        "{}/",
        source["address"]
            .as_str()
            .context("Missing repository address")?
            .trim_end_matches('/')
    );
    // An origin the case declares unreachable is part of the workload (an
    // offline host in a startup graph), so it gets an empty addon list.
    let metadata = match metadata(&address) {
        Ok(value) => value,
        Err(_) if address.contains(".invalid/") || source["unreachable"] == true => json!({}),
        Err(error) => return Err(error.context("Could not fetch repository fixture metadata")),
    };
    let addons = |key: &str| -> Vec<Value> {
        metadata[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| json!([m["modName"], true]))
            .collect()
    };
    let mut repository = json!({
        "profiles":[], "selected_profile":null, "name":source["name"], "address":address,
        "path":source["path"], "additional_params":"", "addons":addons("requiredMods"),
        "optional_addons":addons("optionalMods"), "arma3_profile":null,
        "repository_space_id":source["space_id"], "repository_space_entry_address":address
    });
    for field in [
        "auto_recheck_on_launch",
        "auto_quick_scan_on_launch",
        "auto_backup_on_update",
        "warn_editor_external_addons",
        "enable_editor_mission_list",
        "enable_server_list",
        "check_server_addons_before_join",
        "check_ts3_running_before_join",
        "check_steam_running_before_launch",
        "hide_repo_image",
        "csla",
        "ef",
        "gm",
        "rf",
        "spe",
        "vn",
        "ws",
        "skip_intro",
        "no_splash",
        "world_empty",
        "load_mission_to_memory",
        "enable_ht",
        "huge_pages",
        "no_logs",
        "include_steam_addons",
    ] {
        // A case may switch a launch behaviour on for one repository (a
        // startup probe lane needs `auto_quick_scan_on_launch`).
        repository[field] = json!(source[field].as_bool().unwrap_or(false));
    }
    for field in [
        "apply_repo_json_client_parameters",
        "apply_repo_json_dlc_content",
    ] {
        repository[field] = json!(true);
    }
    for field in [
        "optional_addon_favorites",
        "optional_addon_client_side",
        "remote_client_side_addons",
        "external_addons",
        "external_addon_favorites",
        "external_addon_client_side",
        "servers",
    ] {
        repository[field] = json!([]);
    }
    for field in [
        "icon_image_path",
        "icon_image_checksum",
        "repo_image_path",
        "repo_image_checksum",
        "app_update_url",
    ] {
        repository[field] = json!("");
    }
    Ok(repository)
}

/// Process-owned state that belongs to whichever app instance is running, not
/// to the profile being measured. Copying it would hand the run a stale
/// database claim or another session's logs.
const SEED_EXCLUDED: &[&str] = &["database.lock", "database.owner", "logs", "backups"];

/// Copy a real Foxy configuration directory into the isolated config directory
/// so a case can measure an already-populated profile (a warm database, real
/// repositories, real spaces) instead of bootstrapping one.
///
/// The source is only read. `guards.require_no_other_foxy` is what keeps the
/// copy from being torn by a live app writing into it.
pub fn seed(source: &Path, config: &Path) -> Result<u64> {
    ensure!(
        source.is_dir(),
        "config_seed {} is not a directory",
        source.display()
    );
    let mut copied = 0;
    for entry in walkdir::WalkDir::new(source)
        .into_iter()
        .filter_entry(|entry| {
            !SEED_EXCLUDED.contains(&entry.file_name().to_string_lossy().as_ref())
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let target = config.join(entry.path().strip_prefix(source)?);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        copied += fs::copy(entry.path(), &target)?;
    }
    Ok(copied)
}

pub fn install(case: &Value, config: &Path, run: &Path) -> Result<()> {
    let seeded = case.get("config_seed").and_then(Value::as_str).is_some();
    let fixture = match case.get("fixture").filter(|v| v["files"].is_object()) {
        Some(value) => value.clone(),
        // A seeded config directory already carries the profile; generating a
        // flat legacy fixture over it would trigger the game-space migration
        // and replace the very state the case is measuring.
        None if seeded => return Ok(()),
        None => {
            create_run_folders(case, run)?;
            generated(case)?
        }
    };
    let files = fixture["files"]
        .as_object()
        .context("fixture.files must be an object")?;
    validate_files(files)?;
    fs::create_dir_all(config)?;
    write_json(&run.join("fixture.json"), &fixture)?;
    for (name, value) in files {
        write_json(&config.join(name), value)?;
    }
    Ok(())
}

pub fn install_config_files(case: &Value, config: &Path) -> Result<()> {
    let Some(files) = case.get("config_files").and_then(Value::as_object) else {
        return Ok(());
    };
    fs::create_dir_all(config)?;
    let root = fs::canonicalize(config)?;
    for (relative, value) in files {
        let path = Path::new(relative);
        ensure!(
            !relative.is_empty()
                && !relative.starts_with('/')
                && !relative.starts_with('\\')
                && !relative.contains('\\')
                && !relative.contains(':')
                && relative
                    .split('/')
                    .all(|part| { !part.is_empty() && part != "." && part != ".." })
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                && path
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
            "Config seed paths must be relative JSON files"
        );
        let destination = config.join(path);
        let parent = destination.parent().context("Config seed has no parent")?;
        fs::create_dir_all(parent)?;
        ensure!(
            fs::canonicalize(parent)?.starts_with(&root),
            "Config seed parent resolves outside the isolated config directory"
        );
        if let Ok(metadata) = fs::symlink_metadata(&destination) {
            ensure!(
                metadata.file_type().is_file(),
                "Config seed cannot replace a link or non-file"
            );
        }
        write_json(&destination, value)?;
    }
    Ok(())
}

/// Creates the generated repository and space folders that live inside the run
/// directory, empty, as a person picks a fresh folder: a missing folder makes
/// the app's first check stop at its moved-folder guard.
fn create_run_folders(case: &Value, run: &Path) -> Result<()> {
    let extra_paths = case["extra_repositories"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|extra| &extra["path"]);
    let paths = std::iter::once(&case["repository"]["path"])
        .chain(extra_paths)
        .chain(std::iter::once(&case["space"]["shared_path"]))
        .filter_map(Value::as_str)
        .map(Path::new)
        .filter(|path| {
            path.starts_with(run)
                && !path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        });
    for path in paths {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

fn validate_files(files: &serde_json::Map<String, Value>) -> Result<()> {
    for (name, value) in files {
        ensure!(
            [
                "settings.json",
                "repositories.json",
                "repository_spaces.json"
            ]
            .contains(&name.as_str()),
            "Fixture file {name:?} is not allowed"
        );
        if name != "settings.json" {
            ensure!(
                value.is_array(),
                "Fixture {name} must be an array, including when empty or containing one item"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_fixture_lists_extra_repositories_after_the_primary() {
        let case = json!({
            "repository": {"name": "A", "address": "https://example.invalid/a", "path": "X:\\p", "space_id": null},
            "extra_repositories": [
                {"name": "B", "address": "https://example.invalid/b/", "path": "X:\\p", "space_id": null}
            ]
        });
        let fixture = generated(&case).expect("fixture");
        let repositories = fixture["files"]["repositories.json"]
            .as_array()
            .expect("repositories array");
        assert_eq!(repositories.len(), 2);
        assert_eq!(repositories[0]["name"], "A");
        assert_eq!(repositories[0]["address"], "https://example.invalid/a/");
        assert_eq!(repositories[1]["name"], "B");
        assert_eq!(repositories[1]["path"], "X:\\p");
    }
    #[test]
    fn fixture_arrays_keep_shape_and_cannot_escape_root() {
        assert!(
            validate_files(
                json!({"repositories.json":[{}],"repository_spaces.json":[]})
                    .as_object()
                    .unwrap()
            )
            .is_ok()
        );
        assert!(validate_files(json!({"repositories.json":{}}).as_object().unwrap()).is_err());
        assert!(validate_files(json!({"../settings.json":{}}).as_object().unwrap()).is_err());
    }
    #[test]
    fn only_repository_folders_inside_the_run_are_created() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        let outside = root.path().join("outside");
        let case = json!({
            "repository": {"path": run.join("payload")},
            "extra_repositories": [
                {"path": outside.join("elsewhere")},
                {"path": run.join("..").join("escaped")}
            ],
            "space": {"shared_path": run.join("space")}
        });
        create_run_folders(&case, &run).unwrap();
        assert!(run.join("payload").is_dir());
        assert!(run.join("space").is_dir());
        assert!(!outside.exists());
        assert!(!root.path().join("escaped").exists());
    }

    #[test]
    fn config_file_seeds_write_only_inside_the_isolated_root() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        install_config_files(
            &json!({"config_files":{"games/space-b/game_settings.json":{"future":7}}}),
            &config,
        )
        .unwrap();
        assert_eq!(
            crate::case::read_json(&config.join("games/space-b/game_settings.json")).unwrap()["future"],
            7
        );
        assert!(
            install_config_files(
                &json!({"config_files":{"../outside.json":{"bad":true}}}),
                &config,
            )
            .is_err()
        );
        assert!(!root.path().join("outside.json").exists());
    }
}
