#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SPACE: &str = "reforger";
const OWNER_START_DEADLINE: Duration = Duration::from_secs(30);
const READY_DEADLINE: Duration = Duration::from_secs(240);
const OWNER_JOIN_DEADLINE: Duration = Duration::from_secs(90);
const MAX_NATIVE_LOG_BYTES: u64 = 64 * 1024 * 1024;

#[test]
#[ignore = "Requires an explicitly prepared native Reforger runtime with a compatible savepoint"]
fn native_reforger_sessions_resume_new_and_latest_preserve_full_save_tree() -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_SESSION_CONFIG")?;
    let instance = required_identifier("FOXY_NATIVE_SESSION_INSTANCE")?;
    let output = required_directory("FOXY_NATIVE_SESSION_OUTPUT")?;
    let mut evidence = Evidence::new(&output)?;
    let instance_root = config
        .join("games/reforger/server/instances")
        .join(&instance);
    let save_root = instance_root.join("deployments/runtime-state/home/profiles/profile/.save");
    let session_root = save_root.join("game");
    let (owner_code, owner_status) =
        invoke_raw(&config, &["server", "owner", "--game-space", SPACE])?;
    if owner_code != 4 {
        return Err(io::Error::other(format!(
            "Refusing session acceptance with a pre-existing owner: {owner_status}"
        )));
    }
    let listing = invoke(
        &config,
        &["server", "instance", "list", "--game-space", SPACE],
    )?;
    if !listing["data"]["catalog"]["instances"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["id"] == instance))
    {
        return Err(io::Error::other("Prepared Reforger instance is missing"));
    }
    let status = invoke(
        &config,
        &[
            "server",
            "restore",
            "status",
            &instance,
            "--game-space",
            SPACE,
        ],
    )?;
    if status
        .pointer("/data/status/pending")
        .is_some_and(|value| !value.is_null())
    {
        return Err(io::Error::other(
            "Refusing session acceptance with pending restore recovery",
        ));
    }
    let mut owner = start_parent_bound_owner(&config)?;
    wait_owner(&config, &mut owner)?;
    let start_plan = invoke(
        &config,
        &[
            "server",
            "instance",
            "start-plan",
            &instance,
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(start_plan.clone())?;
    let configuration_seed = start_plan
        .pointer("/data/plan/launch/configuration_seeds")
        .and_then(Value::as_array)
        .and_then(|seeds| {
            seeds
                .iter()
                .find(|seed| seed["reforger_session"].is_object())
        })
        .ok_or_else(|| {
            io::Error::other("Start plan omitted Reforger session configuration seed")
        })?;
    let config_path = PathBuf::from(string(
        &configuration_seed["source"],
        "runtime config source",
    )?);
    let private_config_path = PathBuf::from(string(
        &configuration_seed["destination"],
        "private config destination",
    )?);
    let original_private_config = snapshot_optional_file(&private_config_path, 1024 * 1024)?;
    let original_session_intent = snapshot_optional_file(
        &instance_root.join("deployments/session-intent.json"),
        64 * 1024,
    )?;
    let config_value: Value =
        serde_json::from_slice(&fs::read(&config_path)?).map_err(|error| {
            io::Error::other(format!("Invalid accepted runtime server.json: {error}"))
        })?;
    if !config_value.is_object() {
        return Err(io::Error::other(
            "Accepted runtime server.json is not an object",
        ));
    }
    ensure_stopped(&config, &instance, &mut evidence)?;
    let initial = session_list(&config, &instance, &mut evidence)?;
    let selected = initial["data"]["inventory"]["sessions"]
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["compatible"] == true)
        })
        .ok_or_else(|| io::Error::other("Prepared .save tree has no compatible session"))?
        .clone();
    let save_id = string(&selected["save_id"], "compatible save UUID")?.to_owned();
    let sessions = initial["data"]["inventory"]["sessions"]
        .as_array()
        .ok_or_else(|| io::Error::other("Session inventory is missing"))?;
    if sessions
        .iter()
        .filter(|session| session["save_id"].as_str() == Some(save_id.as_str()))
        .count()
        != 1
    {
        return Err(io::Error::other(
            "Selected compatible metadata UUID is not unique in the prepared inventory",
        ));
    }
    let relative = string(&selected["relative_path"], "save metadata path")?;
    let metadata_path = session_root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    let savepoint_root = metadata_path
        .parent()
        .ok_or_else(|| io::Error::other("Metadata path has no native savepoint parent"))?
        .to_owned();
    let metadata: Value = serde_json::from_slice(&fs::read(&metadata_path)?)
        .map_err(|error| io::Error::other(format!("Invalid native save metadata: {error}")))?;
    if metadata["m_Id"].as_str() != Some(save_id.as_str()) {
        return Err(io::Error::other(
            "Metadata UUID differs from the selected inventory UUID",
        ));
    }
    let blob = find_world_state_blob(&savepoint_root)?;
    let blob_uuid = blob
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Selected WorldState blob has no UTF-8 UUID filename"))?;
    if blob_uuid == save_id {
        return Err(io::Error::other(
            "Fixture does not distinguish metadata m_Id from the WorldState blob UUID",
        ));
    }
    let source_bytes = read_bounded_file(&blob, 512 * 1024 * 1024)?;
    if source_bytes.is_empty() {
        return Err(io::Error::other("Selected native WorldState blob is empty"));
    }

    let original_tree = tree_manifest(&save_root)?;
    let backup = create_native_save_backup(&config, &instance, &mut evidence)?;
    validate_archive_save_coverage(&backup["data"]["outcome"]["manifest"], &original_tree)?;
    let backup_id = string(
        &backup["data"]["outcome"]["manifest"]["backup_id"],
        "native save backup id",
    )?
    .to_owned();

    let backup_path = output.join(format!("native-session-save-backup-{}", unique_stamp()?));
    if backup_path.exists() {
        return Err(io::Error::other(
            "Refusing to overwrite existing native session backup",
        ));
    }
    if backup_path.starts_with(&save_root) || save_root.starts_with(&output) {
        return Err(io::Error::other(
            "Native session backup and save tree must be separate",
        ));
    }
    copy_tree(&save_root, &backup_path)?;
    if tree_manifest(&backup_path)? != original_tree {
        return Err(io::Error::other(
            "Private stopped backup does not exactly match the native .save tree",
        ));
    }
    let mut save_restore = SaveRestore {
        source: save_root.clone(),
        backup: backup_path.clone(),
        owner: Some(owner),
        config: config.clone(),
        instance: instance.clone(),
        active_run: None,
        private_config: private_config_path.clone(),
        original_private_config,
        session_intent: instance_root.join("deployments/session-intent.json"),
        original_session_intent,
        finished: false,
    };
    evidence.push(json!({"fixture": {"save_id": save_id, "world_state_filename_uuid_differs": true, "relative_path": relative, "backup": backup_path.file_name().and_then(|v| v.to_str()), "blob_bytes": source_bytes.len()}}))?;

    isolate_savepoint(&session_root, &metadata_path)?;
    require_only_save(&session_list(&config, &instance, &mut evidence)?, &save_id)?;
    let listed_blob = find_world_state_blob(&savepoint_root)?;
    let before_hash = hash_file(&listed_blob)?;
    fs::write(&listed_blob, b"foxy-native-session-acceptance-mutation")?;
    let mutated_hash = hash_file(&listed_blob)?;
    if before_hash == mutated_hash {
        return Err(io::Error::other(
            "Native save blob mutation did not change its hash",
        ));
    }
    apply_product_restore(
        &config,
        &instance,
        &backup_id,
        &save_root,
        true,
        &mut evidence,
    )?;
    if file_manifest(&save_root)? != only_files(&original_tree) {
        return Err(io::Error::other(
            "Product restore did not restore every byte of the archived native .save tree",
        ));
    }
    let restored_blob = find_world_state_blob(&savepoint_root)?;
    if read_bounded_file(&restored_blob, 512 * 1024 * 1024)? != source_bytes
        || hash_file(&restored_blob)? != before_hash
    {
        return Err(io::Error::other(
            "Full stopped backup did not restore exact native save bytes",
        ));
    }
    let inventory = session_list(&config, &instance, &mut evidence)?;
    require_same_sessions(&initial, &inventory)?;
    isolate_savepoint(&session_root, &metadata_path)?;
    let isolated_inventory = session_list(&config, &instance, &mut evidence)?;
    require_only_save(&isolated_inventory, &save_id)?;

    review_selected_intent(
        &config,
        &instance,
        &save_id,
        &isolated_inventory,
        &mut evidence,
    )?;
    apply_intent(
        &config,
        &instance,
        "selected",
        Some(&save_id),
        &mut evidence,
    )?;
    let planned = plan_intent(
        &config,
        &instance,
        "selected",
        Some(&save_id),
        &mut evidence,
    )?;
    if planned["data"]["plan"]["selected_save_id"] != save_id {
        return Err(io::Error::other(
            "Selected plan did not resolve to the requested UUID",
        ));
    }
    let (selected_generation, selected_args, selected_logs) =
        start_and_wait(&config, &instance, &mut evidence, &mut save_restore)?;
    if !selected_args
        .windows(2)
        .any(|pair| pair[0] == "-loadSessionSave" && pair[1] == save_id)
    {
        return Err(io::Error::other(
            "Native start plan did not request the selected UUID",
        ));
    }
    stop(
        &config,
        &instance,
        selected_generation,
        &mut evidence,
        &mut save_restore,
    )?;
    require_log_line(
        &config,
        &instance,
        selected_generation,
        "[PERSISTENCE] Session restored.",
        &selected_logs,
        &mut evidence,
    )?;
    require_fresh_save_log(
        &config,
        &instance,
        selected_generation,
        &selected_logs,
        &mut evidence,
    )?;
    let after_selected = session_list(&config, &instance, &mut evidence)?;
    require_shutdown_savepoint(&after_selected, &selected)?;

    apply_intent(&config, &instance, "new", None, &mut evidence)?;
    let new_plan = plan_intent(&config, &instance, "new", None, &mut evidence)?;
    if new_plan["data"]["plan"]["selected_save_id"]
        .as_str()
        .is_some()
    {
        return Err(io::Error::other(
            "New intent unexpectedly selected a saved UUID",
        ));
    }
    let (new_generation, new_args, new_logs) =
        start_and_wait(&config, &instance, &mut evidence, &mut save_restore)?;
    if new_args
        .iter()
        .any(|argument| argument == "-loadSessionSave")
    {
        return Err(io::Error::other("New intent passed -loadSessionSave"));
    }
    if !new_args
        .iter()
        .any(|argument| argument == "-keepSessionSave")
    {
        return Err(io::Error::other("New intent did not pass -keepSessionSave"));
    }
    let new_config_path = native_config_argument(&new_args)?;
    let new_config: Value =
        serde_json::from_slice(&fs::read(&new_config_path)?).map_err(|error| {
            io::Error::other(format!("Invalid private new-session server.json: {error}"))
        })?;
    if new_config
        .pointer("/game/gameProperties/persistence/loadSessionSave")
        .and_then(Value::as_bool)
        != Some(false)
        || new_config
            .pointer("/game/gameProperties/persistence/keepSessionSave")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(io::Error::other(
            "New intent private server.json must disable loadSessionSave and retain keepSessionSave",
        ));
    }
    stop(
        &config,
        &instance,
        new_generation,
        &mut evidence,
        &mut save_restore,
    )?;
    require_log_line(
        &config,
        &instance,
        new_generation,
        "[SaveGameManager] Starting new playthrough",
        &new_logs,
        &mut evidence,
    )?;
    require_fresh_save_log(&config, &instance, new_generation, &new_logs, &mut evidence)?;
    let after_new = session_list(&config, &instance, &mut evidence)?;
    require_new_playthrough(&after_new, &selected)?;
    apply_product_restore(
        &config,
        &instance,
        &backup_id,
        &save_root,
        false,
        &mut evidence,
    )?;
    let after_restore_files = file_manifest(&save_root)?;
    if !only_files(&original_tree)
        .iter()
        .all(|(path, value)| after_restore_files.get(path) == Some(value))
    {
        return Err(io::Error::other(
            "Product restore did not retain the complete archived native save tree",
        ));
    }
    isolate_savepoint(&session_root, &metadata_path)?;
    require_only_save(&session_list(&config, &instance, &mut evidence)?, &save_id)?;

    apply_intent(&config, &instance, "configured", None, &mut evidence)?;
    let configured_plan = plan_intent(&config, &instance, "configured", None, &mut evidence)?;
    if configured_plan["data"]["plan"]["intent"]["mode"] != "configured"
        || configured_plan["data"]["plan"]["selected_save_id"]
            .as_str()
            .is_some()
    {
        return Err(io::Error::other(
            "Configured intent did not reset persisted session selection",
        ));
    }

    apply_intent(&config, &instance, "latest", None, &mut evidence)?;
    let latest_plan = plan_intent(&config, &instance, "latest", None, &mut evidence)?;
    if latest_plan["data"]["plan"]["selected_save_id"] != save_id {
        return Err(io::Error::other(
            "Latest intent did not resolve to the only retained save UUID",
        ));
    }
    let (latest_generation, latest_args, latest_logs) =
        start_and_wait(&config, &instance, &mut evidence, &mut save_restore)?;
    if !latest_args
        .windows(2)
        .any(|pair| pair[0] == "-loadSessionSave" && pair[1] == save_id)
    {
        return Err(io::Error::other(
            "Latest start plan did not request the retained UUID",
        ));
    }
    stop(
        &config,
        &instance,
        latest_generation,
        &mut evidence,
        &mut save_restore,
    )?;
    require_log_line(
        &config,
        &instance,
        latest_generation,
        "[PERSISTENCE] Session restored.",
        &latest_logs,
        &mut evidence,
    )?;
    require_fresh_save_log(
        &config,
        &instance,
        latest_generation,
        &latest_logs,
        &mut evidence,
    )?;

    save_restore.finish()?;
    if tree_manifest(&save_root)? != original_tree {
        return Err(io::Error::other(
            "Final joined restore did not restore the complete native .save tree",
        ));
    }
    evidence.push(json!({"result":"passed", "game_space_id":SPACE, "instance_id":instance, "save_id":save_id, "selected_generation":selected_generation, "new_generation":new_generation, "latest_generation":latest_generation, "mutated_hash":mutated_hash, "restored_hash":before_hash, "all_runs_stopped":true}))?;
    Ok(())
}

fn apply_intent(
    config: &Path,
    instance: &str,
    mode: &str,
    save_id: Option<&str>,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let plan = plan_intent(config, instance, mode, save_id, evidence)?;
    let generation = number(
        &plan["data"]["plan"]["expected_generation"],
        "session generation",
    )?
    .to_string();
    let fingerprint = string(&plan["data"]["plan"]["fingerprint"], "session fingerprint")?;
    let mut args = vec!["server", "session", "apply", instance, "--mode", mode];
    if let Some(id) = save_id {
        args.extend(["--save-id", id]);
    }
    args.extend([
        "--expected-generation",
        &generation,
        "--expected-plan-fingerprint",
        fingerprint,
        "--yes",
        "--game-space",
        SPACE,
    ]);
    let applied = invoke(config, &args)?;
    evidence.push(applied)?;
    Ok(())
}

fn create_native_save_backup(
    config: &Path,
    instance: &str,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let backup = invoke(
        config,
        &[
            "server",
            "backup",
            "create",
            instance,
            "--no-retention",
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(backup.clone())?;
    let manifest = &backup["data"]["outcome"]["manifest"];
    if manifest["game_space_id"] != SPACE
        || manifest["instance_id"] != instance
        || manifest["consistency"]["mode"] != "stopped_verified"
        || manifest["consistency"]["stopped_verified"] != true
        || manifest["provider_set"]["writer_stopped_verified"] != true
        || manifest["provider_set"]["reforger_save_tree_version"] != 1
    {
        return Err(io::Error::other(format!(
            "Native save backup lacks stopped Reforger save-tree coverage: {manifest}"
        )));
    }
    let backup_id = string(&manifest["backup_id"], "native save backup id")?;
    let verified = invoke(
        config,
        &[
            "server",
            "backup",
            "verify",
            instance,
            backup_id,
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(verified.clone())?;
    if verified["data"]["verification"]["valid"] != true {
        return Err(io::Error::other(
            "Native save backup archive did not verify",
        ));
    }
    Ok(backup)
}

fn validate_archive_save_coverage(
    manifest: &Value,
    original_tree: &BTreeMap<String, String>,
) -> io::Result<()> {
    let expected = only_files(original_tree);
    let entries = manifest["entries"]
        .as_array()
        .ok_or_else(|| io::Error::other("Native save backup omitted archive entries"))?;
    let prefix = "saves/home/profiles/profile/.save/";
    let archived: BTreeMap<String, (u64, String)> = entries
        .iter()
        .filter(|entry| entry["set"] == "save")
        .filter_map(|entry| {
            let path = entry["archive_path"].as_str()?;
            let relative = path.strip_prefix(prefix)?;
            Some((
                relative.to_owned(),
                (
                    entry["size_bytes"].as_u64()?,
                    entry["blake3"].as_str()?.to_owned(),
                ),
            ))
        })
        .collect();
    if archived.len() != expected.len() {
        return Err(io::Error::other(format!(
            "Backup archives {} .save files; expected {}",
            archived.len(),
            expected.len()
        )));
    }
    for (path, fingerprint) in expected {
        let (size, hash) = fingerprint
            .split_once(':')
            .and_then(|(prefix, rest)| (prefix == "file").then_some(rest))
            .and_then(|rest| rest.split_once(':'))
            .ok_or_else(|| io::Error::other("Invalid native save file fingerprint"))?;
        let size = size.parse::<u64>().map_err(io::Error::other)?;
        if archived.get(&path) != Some(&(size, hash.to_owned())) {
            return Err(io::Error::other(format!(
                "Backup archive does not exactly cover native save file {path}"
            )));
        }
    }
    Ok(())
}

fn apply_product_restore(
    config: &Path,
    instance: &str,
    backup_id: &str,
    save_root: &Path,
    dry_run_first: bool,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let planned = invoke(
        config,
        &[
            "server",
            "restore",
            "plan",
            instance,
            "--backup-id",
            backup_id,
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(planned.clone())?;
    let plan = &planned["data"]["plan"];
    if plan["blockers"]
        .as_array()
        .is_none_or(|blockers| !blockers.is_empty())
    {
        return Err(io::Error::other(format!(
            "Compatible native save restore plan has blockers: {}",
            plan["blockers"]
        )));
    }
    let fingerprint = string(&plan["fingerprint"], "native save restore fingerprint")?;
    if dry_run_first {
        let before = file_manifest(save_root)?;
        let dry = invoke(
            config,
            &[
                "--dry-run",
                "server",
                "restore",
                "apply",
                instance,
                "--backup-id",
                backup_id,
                "--expected-plan-fingerprint",
                fingerprint,
                "--game-space",
                SPACE,
            ],
        )?;
        evidence.push(dry.clone())?;
        if dry["data"]["dry_run"] != true
            || dry["data"]["writes_performed"] != false
            || file_manifest(save_root)? != before
        {
            return Err(io::Error::other(
                "Product restore dry run changed the isolated native save tree",
            ));
        }
    }
    let applied = invoke(
        config,
        &[
            "server",
            "restore",
            "apply",
            instance,
            "--backup-id",
            backup_id,
            "--expected-plan-fingerprint",
            fingerprint,
            "--yes",
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(applied.clone())?;
    if applied["data"]["writes_performed"] != true {
        return Err(io::Error::other("Product restore did not report writes"));
    }
    let status = invoke(
        config,
        &[
            "server",
            "restore",
            "status",
            instance,
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(status.clone())?;
    if !status["data"]["status"]["pending"].is_null() {
        return Err(io::Error::other(
            "Product save restore left a pending journal",
        ));
    }
    Ok(())
}

fn isolate_savepoint(session_root: &Path, retained_metadata: &Path) -> io::Result<()> {
    let mut metadata = Vec::new();
    let mut stack = vec![session_root.to_owned()];
    let mut visited = 0usize;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            visited += 1;
            if visited > 16_384 {
                return Err(io::Error::other(
                    "Native save tree exceeds the isolation entry bound",
                ));
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other(
                    "Symlink found while isolating native savepoints",
                ));
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() && entry.file_name() == "meta-info.json" {
                metadata.push(entry.path());
            }
        }
    }
    if !metadata.iter().any(|path| path == retained_metadata) {
        return Err(io::Error::other(
            "Selected metadata disappeared before savepoint isolation",
        ));
    }
    let mut removed = BTreeMap::new();
    for path in metadata {
        if path == retained_metadata {
            continue;
        }
        let savepoint = path
            .parent()
            .ok_or_else(|| io::Error::other("Metadata path has no savepoint parent"))?;
        removed.insert(savepoint.to_owned(), ());
    }
    for savepoint in removed.into_keys() {
        if !savepoint.starts_with(session_root)
            || savepoint == retained_metadata.parent().unwrap_or(session_root)
        {
            return Err(io::Error::other("Unsafe savepoint isolation path"));
        }
        fs::remove_dir_all(savepoint)?;
    }
    Ok(())
}

fn review_selected_intent(
    config: &Path,
    instance: &str,
    save_id: &str,
    before: &Value,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let plan = plan_intent(config, instance, "selected", Some(save_id), evidence)?;
    let generation = number(
        &plan["data"]["plan"]["expected_generation"],
        "session generation",
    )?;
    let generation_text = generation.to_string();
    let fingerprint = string(&plan["data"]["plan"]["fingerprint"], "session fingerprint")?;
    let dry = invoke(
        config,
        &[
            "--dry-run",
            "server",
            "session",
            "apply",
            instance,
            "--mode",
            "selected",
            "--save-id",
            save_id,
            "--expected-generation",
            &generation_text,
            "--expected-plan-fingerprint",
            fingerprint,
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(dry.clone())?;
    if dry["data"]["dry_run"] != true || dry["data"]["writes_performed"] != false {
        return Err(io::Error::other(
            "Selected intent dry run reported writes or omitted dry_run",
        ));
    }
    assert_intent_unchanged(config, instance, before, evidence)?;

    let stale_fingerprint_args = vec![
        "server",
        "session",
        "apply",
        instance,
        "--mode",
        "selected",
        "--save-id",
        save_id,
        "--expected-generation",
        &generation_text,
        "--expected-plan-fingerprint",
        "stale-session-fingerprint",
        "--yes",
        "--game-space",
        SPACE,
    ];
    let (fingerprint_code, rejected_fingerprint) = invoke_raw(config, &stale_fingerprint_args)?;
    evidence.push(rejected_fingerprint.clone())?;
    if fingerprint_code == 0 {
        return Err(io::Error::other(
            "Stale selected-intent fingerprint was accepted",
        ));
    }
    assert_intent_unchanged(config, instance, before, evidence)?;

    let stale_generation = generation
        .checked_add(1)
        .ok_or_else(|| io::Error::other("Session generation overflow"))?
        .to_string();
    let stale_generation_args = vec![
        "server",
        "session",
        "apply",
        instance,
        "--mode",
        "selected",
        "--save-id",
        save_id,
        "--expected-generation",
        &stale_generation,
        "--expected-plan-fingerprint",
        fingerprint,
        "--yes",
        "--game-space",
        SPACE,
    ];
    let (generation_code, rejected_generation) = invoke_raw(config, &stale_generation_args)?;
    evidence.push(rejected_generation.clone())?;
    if generation_code == 0 {
        return Err(io::Error::other(
            "Stale selected-intent generation was accepted",
        ));
    }
    assert_intent_unchanged(config, instance, before, evidence)
}

fn assert_intent_unchanged(
    config: &Path,
    instance: &str,
    before: &Value,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let current = session_list(config, instance, evidence)?;
    if current["data"]["inventory"]["generation"] != before["data"]["inventory"]["generation"]
        || current["data"]["inventory"]["intent"] != before["data"]["inventory"]["intent"]
    {
        return Err(io::Error::other(
            "Rejected/dry-run intent apply changed persisted intent state",
        ));
    }
    Ok(())
}

fn plan_intent(
    config: &Path,
    instance: &str,
    mode: &str,
    save_id: Option<&str>,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let inventory = session_list(config, instance, evidence)?;
    let generation = inventory["data"]["inventory"]["generation"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Missing session generation"))?
        .to_string();
    let mut args = vec!["server", "session", "plan", instance, "--mode", mode];
    if let Some(id) = save_id {
        args.extend(["--save-id", id]);
    }
    args.extend(["--expected-generation", &generation, "--game-space", SPACE]);
    let planned = invoke(config, &args)?;
    evidence.push(planned.clone())?;
    if planned["data"]["plan"]["blockers"]
        .as_array()
        .is_none_or(|blockers| !blockers.is_empty())
    {
        return Err(io::Error::other(format!(
            "Session plan has blockers: {}",
            planned["data"]["plan"]["blockers"]
        )));
    }
    Ok(planned)
}

fn session_list(config: &Path, instance: &str, evidence: &mut Evidence) -> io::Result<Value> {
    current_inventory(config, instance, evidence).map(|(value, _)| value)
}
fn current_inventory(
    config: &Path,
    instance: &str,
    evidence: &mut Evidence,
) -> io::Result<(Value, Value)> {
    let value = invoke(
        config,
        &["server", "session", "list", instance, "--game-space", SPACE],
    )?;
    let inventory = value["data"]["inventory"].clone();
    evidence.push(value.clone())?;
    Ok((value, inventory))
}
fn require_only_save(value: &Value, save_id: &str) -> io::Result<()> {
    let sessions = value["data"]["inventory"]["sessions"]
        .as_array()
        .ok_or_else(|| io::Error::other("Session inventory is missing"))?;
    if sessions.len() != 1 || sessions[0]["save_id"] != save_id || sessions[0]["compatible"] != true
    {
        return Err(io::Error::other(
            "Native save inventory is not exactly the one compatible target UUID",
        ));
    }
    Ok(())
}
fn require_shutdown_savepoint(inventory: &Value, original: &Value) -> io::Result<()> {
    let sessions = inventory["data"]["inventory"]["sessions"]
        .as_array()
        .ok_or_else(|| io::Error::other("Session inventory is missing after selected shutdown"))?;
    let playthrough = number(&original["playthrough"], "original playthrough")?;
    let savepoint = number(&original["savepoint"], "original savepoint")?;
    if !sessions.iter().any(|session| {
        session["playthrough"]
            .as_u64()
            .is_some_and(|value| value == playthrough)
            && session["savepoint"]
                .as_u64()
                .is_some_and(|value| value > savepoint)
    }) {
        return Err(io::Error::other(
            "Selected shutdown did not emit a later savepoint in the resumed playthrough",
        ));
    }
    Ok(())
}
fn require_new_playthrough(inventory: &Value, original: &Value) -> io::Result<()> {
    let sessions = inventory["data"]["inventory"]["sessions"]
        .as_array()
        .ok_or_else(|| io::Error::other("Session inventory is missing after New shutdown"))?;
    let original_playthrough = number(&original["playthrough"], "original playthrough")?;
    let retained = string(&original["save_id"], "original save UUID")?;
    if !sessions
        .iter()
        .any(|session| session["save_id"] == retained)
        || !sessions.iter().any(|session| {
            session["playthrough"]
                .as_u64()
                .is_some_and(|value| value > original_playthrough)
        })
    {
        return Err(io::Error::other(
            "New playthrough shutdown did not retain the prior save and emit a later playthrough",
        ));
    }
    Ok(())
}
fn start_and_wait(
    config: &Path,
    instance: &str,
    evidence: &mut Evidence,
    save_restore: &mut SaveRestore,
) -> io::Result<(u64, Vec<String>, RunLogBaseline)> {
    let log_baseline = RunLogBaseline::capture(config, instance, evidence)?;
    let plan = invoke(
        config,
        &[
            "server",
            "instance",
            "start-plan",
            instance,
            "--game-space",
            SPACE,
        ],
    )?;
    let generation = number(
        &plan["data"]["plan"]["activation_generation"],
        "activation generation",
    )?
    .to_string();
    let fingerprint = string(
        &plan["data"]["plan"]["fingerprint"],
        "activation fingerprint",
    )?
    .to_owned();
    let arguments = plan
        .pointer("/data/plan/launch/arguments")
        .or_else(|| plan.pointer("/data/plan/arguments"))
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("Start plan omitted native arguments"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let started = invoke(
        config,
        &[
            "server",
            "instance",
            "start",
            instance,
            "--expected-activation-generation",
            &generation,
            "--expected-plan-fingerprint",
            &fingerprint,
            "--yes",
            "--game-space",
            SPACE,
        ],
    )?;
    let run = number(
        &started["data"]["outcome"]["status"]["run_generation"],
        "run generation",
    )?;
    save_restore.active_run = Some(run);
    evidence.push(started.clone())?;
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let report = invoke(
            config,
            &[
                "server",
                "instance",
                "monitor",
                instance,
                "--game-space",
                SPACE,
            ],
        )?;
        let monitoring = &report["data"]["monitoring"];
        if monitoring["query_ready"] == true && monitoring["status"]["run_generation"] == run {
            evidence.push(report)?;
            return Ok((run, arguments, log_baseline));
        }
        evidence.push(report)?;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Run {run} did not become query-ready"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}
fn stop(
    config: &Path,
    instance: &str,
    run: u64,
    evidence: &mut Evidence,
    save_restore: &mut SaveRestore,
) -> io::Result<Value> {
    let value = graceful_stop(config, instance, run)?;
    save_restore.active_run = None;
    evidence.push(value.clone())?;
    Ok(value)
}
fn graceful_stop(config: &Path, instance: &str, run: u64) -> io::Result<Value> {
    let run = run.to_string();
    let value = invoke(
        config,
        &[
            "server",
            "instance",
            "stop",
            instance,
            "--expected-run-generation",
            &run,
            "--timeout-ms",
            "60000",
            "--yes",
            "--game-space",
            SPACE,
        ],
    )?;
    require_clean_stop(&value)?;
    Ok(value)
}
fn require_clean_stop(value: &Value) -> io::Result<()> {
    if value["data"]["outcome"]["completed"] != true
        || value["data"]["outcome"]["status"]["cleanup_verified"] != true
    {
        return Err(io::Error::other(format!(
            "Graceful stop did not verify cleanup: {value}"
        )));
    }
    Ok(())
}

fn require_log_line(
    config: &Path,
    instance: &str,
    run: u64,
    needle: &str,
    baseline: &RunLogBaseline,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let log = runtime_root(config, instance).join("home/runtime/console.log");
    let generation_log = generation_log(config, instance, run, baseline, evidence)?;
    let console_signature = file_signature(&log)?;
    if baseline
        .console
        .as_ref()
        .is_some_and(|previous| previous == &console_signature)
    {
        return Err(io::Error::other(format!(
            "Native console.log was not rewritten for run {run}"
        )));
    }
    let run_text = read_bounded_text(&generation_log, MAX_NATIVE_LOG_BYTES)?;
    if !run_text.contains(needle) {
        return Err(io::Error::other(format!(
            "Fresh generation {run} log did not contain expected engine evidence: {needle}"
        )));
    }
    Ok(())
}
fn require_fresh_save_log(
    config: &Path,
    instance: &str,
    run: u64,
    baseline: &RunLogBaseline,
    evidence: &mut Evidence,
) -> io::Result<()> {
    require_log_line(
        config,
        instance,
        run,
        "Save completed successfully",
        baseline,
        evidence,
    )
}
fn generation_log(
    config: &Path,
    instance: &str,
    run: u64,
    baseline: &RunLogBaseline,
    evidence: &mut Evidence,
) -> io::Result<PathBuf> {
    let path = runtime_root(config, instance).join(format!("run_{}_{run}.log", baseline.owner_id));
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| io::Error::other("Invalid owner/generation log name"))?;
    if baseline.run_logs.iter().any(|old| old == name) {
        return Err(io::Error::other(
            "Owner/generation log existed before this run",
        ));
    }
    let current = file_signature(&path)?;
    evidence.push(json!({"native_log": {"generation": run, "name": name, "size": current.0, "blake3": current.1, "modified_unix_nanos": current.2}}))?;
    let text = read_bounded_text(&path, MAX_NATIVE_LOG_BYTES)?;
    if text.is_empty() {
        return Err(io::Error::other("Current generation log is empty"));
    }
    Ok(path)
}
fn runtime_root(config: &Path, instance: &str) -> PathBuf {
    config
        .join("games/reforger/server/instances")
        .join(instance)
        .join("deployments/runtime-state")
}
fn ensure_stopped(config: &Path, instance: &str, evidence: &mut Evidence) -> io::Result<()> {
    let status = invoke(
        config,
        &[
            "server",
            "instance",
            "status",
            instance,
            "--game-space",
            SPACE,
        ],
    )?;
    evidence.push(status.clone())?;
    if status.pointer("/data/status/process_exists") == Some(&Value::Bool(true)) {
        return Err(io::Error::other(
            "Refusing to inspect or change native saves while the prepared instance is running",
        ));
    }
    Ok(())
}
fn native_config_argument(arguments: &[String]) -> io::Result<PathBuf> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == "-config")
        .map(|pair| PathBuf::from(&pair[1]))
        .ok_or_else(|| io::Error::other("Native start plan omitted -config"))
}

fn find_world_state_blob(root: &Path) -> io::Result<PathBuf> {
    let mut matches = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other("Symlink found in native savepoint"));
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_owned();
                if relative.parent() == Some(Path::new("WorldState"))
                    && relative
                        .extension()
                        .is_some_and(|extension| extension == "blob")
                {
                    matches.push(entry.path());
                }
            }
        }
    }
    if matches.len() != 1 {
        return Err(io::Error::other(format!(
            "Expected exactly one direct WorldState blob in the selected savepoint, found {}",
            matches.len()
        )));
    }
    Ok(matches.remove(0))
}
fn read_bounded_file(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > max_bytes {
        return Err(io::Error::other(
            "Native WorldState blob exceeds the 512 MiB acceptance bound",
        ));
    }
    fs::read(path)
}
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    copy_tree_bounded(source, target, &mut 0)
}
fn copy_tree_bounded(source: &Path, target: &Path, total: &mut u64) -> io::Result<()> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if kind.is_symlink() {
            return Err(io::Error::other(
                "Symlink found while copying native .save tree",
            ));
        }
        if kind.is_dir() {
            copy_tree_bounded(&entry.path(), &destination, total)?;
        } else if kind.is_file() {
            *total = total
                .checked_add(entry.metadata()?.len())
                .ok_or_else(|| io::Error::other("Native .save tree size overflow"))?;
            if *total > 2 * 1024 * 1024 * 1024 {
                return Err(io::Error::other(
                    "Native .save tree exceeds the 2 GiB acceptance bound",
                ));
            }
            fs::copy(entry.path(), destination)?;
        } else {
            return Err(io::Error::other("Unsupported entry in native .save tree"));
        }
    }
    Ok(())
}
fn restore_tree(source: &Path, backup: &Path) -> io::Result<()> {
    if source.exists() {
        fs::remove_dir_all(source)?;
    }
    copy_tree(backup, source)
}
fn tree_manifest(root: &Path) -> io::Result<BTreeMap<String, String>> {
    fn visit(
        root: &Path,
        directory: &Path,
        result: &mut BTreeMap<String, String>,
        bytes: &mut u64,
    ) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other(
                    "Symlink found in native .save tree inventory",
                ));
            }
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            let key = relative
                .to_str()
                .ok_or_else(|| io::Error::other("Non-UTF-8 path in native .save tree"))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            if result.len() >= 100_000 {
                return Err(io::Error::other(
                    "Native .save tree exceeds the 100,000-entry acceptance bound",
                ));
            }
            if kind.is_dir() {
                result.insert(key, "directory".into());
                visit(root, &path, result, bytes)?;
            } else if kind.is_file() {
                *bytes = bytes
                    .checked_add(entry.metadata()?.len())
                    .ok_or_else(|| io::Error::other("Native .save tree size overflow"))?;
                if *bytes > 2 * 1024 * 1024 * 1024 {
                    return Err(io::Error::other(
                        "Native .save tree exceeds the 2 GiB acceptance bound",
                    ));
                }
                let size = entry.metadata()?.len();
                result.insert(key, format!("file:{size}:{}", hash_file(&path)?));
            } else {
                return Err(io::Error::other(
                    "Special file in native .save tree inventory",
                ));
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result, &mut 0)?;
    Ok(result)
}
fn file_manifest(root: &Path) -> io::Result<BTreeMap<String, String>> {
    Ok(only_files(&tree_manifest(root)?))
}
fn only_files(manifest: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    manifest
        .iter()
        .filter(|(_, value)| value.starts_with("file:"))
        .map(|(path, value)| (path.clone(), value.clone()))
        .collect()
}
fn require_same_sessions(before: &Value, after: &Value) -> io::Result<()> {
    if before["data"]["inventory"]["sessions"] != after["data"]["inventory"]["sessions"] {
        return Err(io::Error::other(
            "Product restore did not reproduce the original native save inventory",
        ));
    }
    Ok(())
}
fn snapshot_optional_file(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > max_bytes
            {
                return Err(io::Error::other(
                    "Refusing unsafe or oversized private runtime metadata",
                ));
            }
            Ok(Some(fs::read(path)?))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn restore_optional_file(path: &Path, original: &Option<Vec<u8>>) -> io::Result<()> {
    if let Some(bytes) = original {
        if let Ok(metadata) = fs::symlink_metadata(path)
            && (metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(io::Error::other(
                "Refusing to replace unsafe private runtime metadata",
            ));
        }
        fs::write(path, bytes)?;
    } else {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(io::Error::other(
                        "Refusing to remove unsafe private runtime metadata",
                    ));
                }
                fs::remove_file(path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
struct SaveRestore {
    source: PathBuf,
    backup: PathBuf,
    owner: Option<OwnedChild>,
    config: PathBuf,
    instance: String,
    active_run: Option<u64>,
    private_config: PathBuf,
    original_private_config: Option<Vec<u8>>,
    session_intent: PathBuf,
    original_session_intent: Option<Vec<u8>>,
    finished: bool,
}
impl SaveRestore {
    fn stop_active(&mut self) -> io::Result<()> {
        let Some(run) = self.active_run else {
            return Ok(());
        };
        graceful_stop(&self.config, &self.instance, run)?;
        self.active_run = None;
        Ok(())
    }
    fn close_owner(&mut self) -> io::Result<()> {
        if let Some(owner) = self.owner.as_mut() {
            owner.close_and_wait()?;
        }
        self.owner.take();
        Ok(())
    }
    fn restore_state(&self) -> io::Result<()> {
        restore_tree(&self.source, &self.backup)?;
        restore_optional_file(&self.private_config, &self.original_private_config)?;
        restore_optional_file(&self.session_intent, &self.original_session_intent)
    }
    fn finish(&mut self) -> io::Result<()> {
        self.stop_active()?;
        self.close_owner()?;
        self.restore_state()?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for SaveRestore {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let stopped = self.stop_active().is_ok();
        let joined = self.close_owner().is_ok();
        if stopped && joined {
            let _ = self.restore_state();
        }
    }
}

fn file_signature(path: &Path) -> io::Result<(u64, String, u128)> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_NATIVE_LOG_BYTES {
        return Err(io::Error::other(
            "Native log exceeds the 64 MiB acceptance bound",
        ));
    }
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    Ok((metadata.len(), hash_file(path)?, modified))
}
fn read_bounded_text(path: &Path, max_bytes: u64) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::other(
            "Native log exceeds the acceptance size bound",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}
fn unique_stamp() -> io::Result<u128> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos())
}
fn start_parent_bound_owner(config: &Path) -> io::Result<OwnedChild> {
    let mut command = base_command(config);
    command
        .args([
            "server",
            "supervise",
            "--parent-bound",
            "--game-space",
            SPACE,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(OwnedChild(Some(command.spawn()?)))
}
fn wait_owner(config: &Path, child: &mut OwnedChild) -> io::Result<()> {
    let deadline = Instant::now() + OWNER_START_DEADLINE;
    loop {
        if child
            .0
            .as_mut()
            .and_then(|process| process.try_wait().ok().flatten())
            .is_some()
        {
            return Err(io::Error::other(
                "Parent-bound owner exited before readiness",
            ));
        }
        let (code, value) = invoke_raw(config, &["server", "owner", "--game-space", SPACE])?;
        if code == 0 && value.pointer("/data/owner/database_claimed") == Some(&Value::Bool(true)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owner did not become ready",
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}
struct OwnedChild(Option<Child>);
impl OwnedChild {
    fn close_and_wait(&mut self) -> io::Result<()> {
        let Some(child) = self.0.as_mut() else {
            return Ok(());
        };
        drop(child.stdin.take());
        let result = wait_bounded(child);
        if result.is_ok() {
            self.0.take();
        }
        result
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            drop(child.stdin.take());
            let _ = wait_bounded(&mut child);
        }
    }
}
fn wait_bounded(child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + OWNER_JOIN_DEADLINE;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "Parent-bound owner exited unsuccessfully: {status}"
                )))
            };
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Parent-bound owner did not exit after stdin closed",
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

struct Evidence {
    path: PathBuf,
    values: Vec<Value>,
}
impl Evidence {
    fn new(output: &Path) -> io::Result<Self> {
        let path = output.join("server_sessions_native.json");
        if path.exists() {
            return Err(io::Error::other(
                "Refusing to overwrite existing native session evidence",
            ));
        }
        Ok(Self {
            path,
            values: Vec::new(),
        })
    }
    fn push(&mut self, value: Value) -> io::Result<()> {
        self.values.push(value);
        fs::write(&self.path, serde_json::to_vec_pretty(&self.values)?)
    }
}
impl Drop for Evidence {
    fn drop(&mut self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.values) {
            let _ = fs::write(&self.path, bytes);
        }
    }
}
struct RunLogBaseline {
    owner_id: String,
    console: Option<(u64, String, u128)>,
    run_logs: Vec<String>,
}
impl RunLogBaseline {
    fn capture(config: &Path, instance: &str, evidence: &mut Evidence) -> io::Result<Self> {
        let owner = invoke(config, &["server", "owner", "--game-space", SPACE])?;
        let owner_id = string(
            &owner["data"]["owner"]["scope"]["owner_id"],
            "current owner id",
        )?
        .to_owned();
        evidence.push(owner)?;
        let root = runtime_root(config, instance);
        let console_path = root.join("home/runtime/console.log");
        let console = if console_path.exists() {
            Some(file_signature(&console_path)?)
        } else {
            None
        };
        let mut run_logs = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_file() && name.starts_with("run_") && name.ends_with(".log") {
                run_logs.push(name);
            }
        }
        run_logs.sort();
        evidence.push(json!({"log_baseline": {"console": console.as_ref().map(|signature| json!({"size": signature.0, "blake3": &signature.1, "modified_unix_nanos": signature.2})), "run_logs": run_logs}}))?;
        Ok(Self {
            owner_id,
            console,
            run_logs,
        })
    }
}
fn required_identifier(name: &str) -> io::Result<String> {
    let value = std::env::var(name).map_err(io::Error::other)?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(io::Error::other(format!("Invalid {name}")));
    }
    Ok(value)
}
fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os(name).ok_or_else(|| io::Error::other(format!("Missing {name}")))?,
    );
    if !path.is_absolute() || !path.is_dir() {
        return Err(io::Error::other(format!(
            "{name} must be an existing absolute directory"
        )));
    }
    path.canonicalize()
}
fn string<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}
fn number(value: &Value, label: &str) -> io::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}
fn invoke(config: &Path, args: &[&str]) -> io::Result<Value> {
    let (code, value) = invoke_raw(config, args)?;
    if code != 0 || value["ok"] != true {
        return Err(io::Error::other(format!(
            "CLI exit {code}, args={args:?}: {value}"
        )));
    }
    Ok(value)
}
fn invoke_raw(config: &Path, args: &[&str]) -> io::Result<(i32, Value)> {
    let mut command = base_command(config);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output()?;
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        io::Error::other(format!(
            "CLI emitted invalid JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ))
    })?;
    Ok((output.status.code().unwrap_or(-1), value))
}
fn base_command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--quiet", "--no-progress", "--config-dir"])
        .arg(config)
        .args(["--json"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
