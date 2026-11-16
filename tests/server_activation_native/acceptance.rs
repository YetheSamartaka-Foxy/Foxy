use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[path = "interrupt.rs"]
mod interrupt;
#[path = "preparation.rs"]
mod preparation;
use super::inventory::{snapshot_selected_payload, snapshot_tree};
use preparation::{PreparationRevision, Prepared, prepare, setup_content};

const GAME: &str = "arma3";
const MISSION: &str = "foxy_native.vr";

pub(super) fn run() -> io::Result<()> {
    let source = required_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = required_directory("FOXY_ARMA_ACTIVATION_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and activation scratch must be separate",
        ));
    }
    let source_inventory = snapshot_selected_payload(&source)?;
    let case = fresh_child(&scratch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&case, fs::Permissions::from_mode(0o700))?;
    }
    let config = case.join("config");
    fs::create_dir(&config)?;
    fs::write(
        config.join("games.json"),
        include_bytes!("../../examples/json/appdata/games.json"),
    )?;
    let first_identity = format!("FoxyNativeAlpha_{}", fresh_nonce());
    let created = invoke(
        &config,
        &["server", "instance", "add", "--name", "NativeActivation"],
        "create instance",
    )?;
    let id = string(&created["data"]["instance"]["id"], "instance id")?;
    let mut instance = invoke(&config, &["server", "instance", "show", id], "show draft")?["data"]
        ["instance"]
        .clone();
    let settings = &mut instance["definition"]["settings"];
    settings["hostname"] = first_identity.clone().into();
    settings["game_port"] = 27102.into();
    settings["max_players"] = 1.into();
    settings["persistent"] = true.into();
    settings["auto_init"] = true.into();
    settings["battleye"] = false.into();
    settings["mission_cycle"] = serde_json::json!([{"template":MISSION,"difficulty":"regular"}]);
    let draft_file = config.join("draft.json");
    fs::write(&draft_file, serde_json::to_vec(&instance)?)?;
    invoke(
        &config,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            path_arg(&draft_file)?,
            "--expected-revision",
            "1",
        ],
        "configure draft",
    )?;
    invoke(
        &config,
        &[
            "server",
            "instance",
            "adopt",
            id,
            "--directory",
            path_arg(&source)?,
            "--expected-revision",
            "2",
        ],
        "adopt native source",
    )?;
    setup_content(&config, &case, id)?;
    let configuration = invoke(
        &config,
        &["server", "configuration", "plan", id],
        "plan configuration",
    )?;
    let data = &configuration["data"];
    invoke(
        &config,
        &[
            "server",
            "configuration",
            "commit",
            id,
            "--expected-revision",
            "3",
            "--expected-generation",
            &number_string(&data["expected_generation"], "configuration generation")?,
            "--expected-fingerprint",
            string(&data["preview"]["fingerprint"], "configuration fingerprint")?,
        ],
        "commit configuration",
    )?;

    let first = prepare(
        &config,
        &case,
        id,
        "alpha",
        &first_identity,
        PreparationRevision {
            deployment: 0,
            draft: 3,
            configuration: 1,
        },
    )?;
    let plan_a = activation_plan(&config, id, &first, 1, 0)?;
    let runtime_a = string(
        &plan_a["data"]["plan"]["runtime"]["runtime_id"],
        "runtime A id",
    )?;
    activation_apply(&config, id, &first, 1, 0, &plan_a)?;
    let a_dir = PathBuf::from(string(
        &plan_a["data"]["plan"]["runtime_directory"],
        "runtime A directory",
    )?)
    .join("engine");
    let a_inventory = snapshot_selected_payload(&a_dir)?;
    let first_a = run_active_engine(
        &config,
        &case,
        &source,
        id,
        &first.identity,
        &first.identity,
        1,
    )?;
    assert_ready(&first_a);
    assert!(!first_a.profile_readback && !first_a.mission_readback);
    let restart_a = run_active_engine(
        &config,
        &case,
        &source,
        id,
        &first.identity,
        &first.identity,
        2,
    )?;
    assert!(restart_a.profile_readback && restart_a.mission_readback);

    let second_identity = format!("FoxyNativeBeta_{}", fresh_nonce());
    let (draft_revision, configuration_generation) =
        update_configuration(&config, id, 3, 1, &second_identity)?;
    let second = prepare(
        &config,
        &case,
        id,
        "beta",
        &second_identity,
        PreparationRevision {
            deployment: 1,
            draft: draft_revision,
            configuration: configuration_generation,
        },
    )?;
    assert_ne!(first.identity, second.identity);
    let plan_b = activation_plan(&config, id, &second, 2, 1)?;
    let runtime_b = string(
        &plan_b["data"]["plan"]["runtime"]["runtime_id"],
        "runtime B id",
    )?;
    assert_ne!(runtime_a, runtime_b);
    activation_apply(&config, id, &second, 2, 1, &plan_b)?;
    let first_b = run_active_engine(
        &config,
        &case,
        &source,
        id,
        &second.identity,
        &second.identity,
        3,
    )?;
    assert_ready(&first_b);
    assert!(!first_b.profile_readback && !first_b.mission_readback);
    assert_eq!(snapshot_selected_payload(&a_dir)?, a_inventory);
    let saved_namespaces = snapshot_tree(&first_b.runtime_state)?;

    let selection = invoke(
        &config,
        &[
            "server",
            "deploy",
            "select-plan",
            id,
            &first.record,
            "--expected-deployment-generation",
            "2",
        ],
        "review predecessor selection",
    )?;
    invoke(
        &config,
        &[
            "server",
            "deploy",
            "select",
            id,
            &first.record,
            "--expected-deployment-generation",
            "2",
            "--expected-plan-fingerprint",
            string(
                &selection["data"]["plan"]["fingerprint"],
                "selection fingerprint",
            )?,
            "--yes",
        ],
        "select predecessor",
    )?;
    let rollback_plan = activation_plan(&config, id, &first, 3, 2)?;
    assert_eq!(
        string(
            &rollback_plan["data"]["plan"]["runtime"]["runtime_id"],
            "rollback runtime id"
        )?,
        runtime_a
    );
    activation_apply(&config, id, &first, 3, 2, &rollback_plan)?;
    assert_eq!(snapshot_tree(&first_b.runtime_state)?, saved_namespaces);
    assert_eq!(snapshot_selected_payload(&a_dir)?, a_inventory);
    let rollback_a = run_active_engine(
        &config,
        &case,
        &source,
        id,
        &first.identity,
        &first.identity,
        4,
    )?;
    assert_ready(&rollback_a);
    assert!(!rollback_a.profile_readback && !rollback_a.mission_readback);
    let after_restart_a = run_active_engine(
        &config,
        &case,
        &source,
        id,
        &first.identity,
        &first.identity,
        5,
    )?;
    assert!(after_restart_a.profile_readback && after_restart_a.mission_readback);
    assert_eq!(snapshot_selected_payload(&source)?, source_inventory);
    assert_eq!(snapshot_selected_payload(&a_dir)?, a_inventory);
    interrupt::run(interrupt::InterruptContext {
        config: &config,
        case: &case,
        source: &source,
        id,
        predecessor: &first,
        predecessor_engine: &a_dir,
        predecessor_inventory: &a_inventory,
        draft_revision,
        configuration_generation,
    })?;
    Ok(())
}

pub(super) fn run_active_engine(
    config: &Path,
    case: &Path,
    source: &Path,
    id: &str,
    identity: &str,
    hostname: &str,
    generation: u64,
) -> io::Result<super::engine::EngineProof> {
    let plan = invoke(
        config,
        &["server", "instance", "launch-plan", id],
        "plan active native runtime",
    )?;
    assert_eq!(plan["data"]["spawned"], false);
    assert_eq!(plan["data"]["runtime_materialized"], true);
    assert_eq!(plan["data"]["engine_layout"], "native_isolated_copy");
    super::engine::consume(
        &plan["data"]["native_runtime"],
        source,
        id,
        identity,
        hostname,
        generation,
        &case.join("console-logs"),
    )
}

pub(super) fn assert_ready(proof: &super::engine::EngineProof) {
    assert!(proof.mission_seen && proof.content_isolated && proof.save_requested);
}

pub(super) fn update_configuration(
    config: &Path,
    id: &str,
    current_revision: u64,
    current_generation: u64,
    hostname: &str,
) -> io::Result<(u64, u64)> {
    let shown = invoke(
        config,
        &["server", "instance", "show", id],
        "show draft before hostname update",
    )?;
    let mut instance = shown["data"]["instance"].clone();
    assert_eq!(instance["revision"], current_revision);
    instance["definition"]["settings"]["hostname"] = hostname.into();
    let input = config.join(format!("draft-revision-{current_revision}.json"));
    fs::write(&input, serde_json::to_vec(&instance)?)?;
    let changed = invoke(
        config,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            path_arg(&input)?,
            "--expected-revision",
            &current_revision.to_string(),
        ],
        "save nonce hostname",
    )?;
    let next_revision = changed["data"]["instance"]["revision"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Updated draft revision missing"))?;
    let plan = invoke(
        config,
        &["server", "configuration", "plan", id],
        "plan nonce hostname configuration",
    )?;
    let data = &plan["data"];
    assert_eq!(data["expected_generation"], current_generation);
    let next_generation = current_generation + 1;
    invoke(
        config,
        &[
            "server",
            "configuration",
            "commit",
            id,
            "--expected-revision",
            &number_string(
                &data["preview"]["instance_revision"],
                "configuration draft revision",
            )?,
            "--expected-generation",
            &current_generation.to_string(),
            "--expected-fingerprint",
            string(&data["preview"]["fingerprint"], "configuration fingerprint")?,
        ],
        "commit nonce hostname configuration",
    )?;
    Ok((next_revision, next_generation))
}

fn activation_plan(
    config: &Path,
    id: &str,
    prepared: &Prepared,
    selection: u64,
    activation: u64,
) -> io::Result<Value> {
    invoke(
        config,
        &[
            "server",
            "deploy",
            "activation",
            "plan",
            id,
            &prepared.preparation,
            "--expected-deployment-generation",
            &selection.to_string(),
            "--expected-activation-generation",
            &activation.to_string(),
        ],
        "plan native activation",
    )
}

fn activation_apply(
    config: &Path,
    id: &str,
    prepared: &Prepared,
    selection: u64,
    activation: u64,
    plan: &Value,
) -> io::Result<()> {
    let result = invoke(
        config,
        &[
            "--yes",
            "server",
            "deploy",
            "activation",
            "apply",
            id,
            &prepared.preparation,
            "--expected-deployment-generation",
            &selection.to_string(),
            "--expected-activation-generation",
            &activation.to_string(),
            "--expected-plan-fingerprint",
            string(
                &plan["data"]["plan"]["fingerprint"],
                "activation fingerprint",
            )?,
        ],
        "apply native activation",
    )?;
    assert_eq!(result["data"]["outcome"]["status"], "committed");
    Ok(())
}

fn invoke(config: &Path, args: &[&str], operation: &str) -> io::Result<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(config)
        .arg("--json")
        .args(args)
        .args(["--game-space", GAME]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output()?;
    if output.stdout.len() > 1024 * 1024 || output.stderr.len() > 64 * 1024 {
        return Err(io::Error::other("Foxy CLI output exceeded bounds"));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| io::Error::other(format!("{operation}: invalid JSON response")))?;
    if !output.status.success() || value["ok"] != true {
        return Err(io::Error::other(format!(
            "{operation} failed with exit code {}",
            output.status.code().unwrap_or(-1)
        )));
    }
    eprintln!("{operation}: complete");
    Ok(value)
}

fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os(name).ok_or_else(|| io::Error::other(format!("Set {name}")))?,
    );
    if !path.is_absolute() || !fs::symlink_metadata(&path)?.is_dir() {
        return Err(io::Error::other(format!(
            "{name} must name an existing absolute directory"
        )));
    }
    path.canonicalize()
        .map_err(|_| io::Error::other(format!("{name} directory could not be resolved")))
}

fn fresh_child(parent: &Path) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let child = parent.join(format!("a_{:016x}", rand::random::<u64>()));
        match fs::create_dir(&child) {
            Ok(()) => return Ok(child),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(
        "Could not create a fresh activation scratch child",
    ))
}

fn fresh_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}
fn path_arg(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other("Path is not representable"))
}
fn string<'a>(value: &'a Value, name: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}

fn number_string(value: &Value, name: &str) -> io::Result<String> {
    value
        .as_u64()
        .map(|number| number.to_string())
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}
