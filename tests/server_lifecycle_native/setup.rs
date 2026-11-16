use super::preparation::Prepared;
use super::{MISSION, invoke_cli as invoke, number_string, path_arg, string};
use std::{fs, io, path::Path};

pub(super) fn configure(
    config: &Path,
    case: &Path,
    id: &str,
    install: &Path,
    port: u16,
    hostname: &str,
) -> io::Result<()> {
    let mut instance =
        invoke(config, &["server", "instance", "show", id], 0)?["data"]["instance"].clone();
    let settings = &mut instance["definition"]["settings"];
    settings["hostname"] = hostname.into();
    settings["game_port"] = port.into();
    settings["max_players"] = 1.into();
    settings["persistent"] = true.into();
    settings["auto_init"] = true.into();
    settings["battleye"] = false.into();
    settings["mission_cycle"] = serde_json::json!([{"template":MISSION,"difficulty":"regular"}]);
    let file = case.join("draft.json");
    fs::write(&file, serde_json::to_vec(&instance)?)?;
    invoke(
        config,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            path_arg(&file)?,
            "--expected-revision",
            "1",
        ],
        0,
    )?;
    invoke(
        config,
        &[
            "server",
            "instance",
            "adopt",
            id,
            "--directory",
            path_arg(install)?,
            "--expected-revision",
            "2",
        ],
        0,
    )?;
    let planned = invoke(config, &["server", "configuration", "plan", id], 0)?;
    invoke(
        config,
        &[
            "server",
            "configuration",
            "commit",
            id,
            "--expected-revision",
            "3",
            "--expected-generation",
            &number_string(&planned["data"]["expected_generation"])?,
            "--expected-fingerprint",
            string(
                &planned["data"]["preview"]["fingerprint"],
                "configuration fingerprint",
            )?,
        ],
        0,
    )?;
    Ok(())
}

pub(super) fn update_configuration(
    config: &Path,
    id: &str,
    hostname: &str,
    port: u16,
) -> io::Result<()> {
    let mut instance =
        invoke(config, &["server", "instance", "show", id], 0)?["data"]["instance"].clone();
    instance["definition"]["settings"]["hostname"] = hostname.into();
    instance["definition"]["settings"]["game_port"] = port.into();
    let file = config.join("updated-draft.json");
    fs::write(&file, serde_json::to_vec(&instance)?)?;
    invoke(
        config,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            path_arg(&file)?,
            "--expected-revision",
            "3",
        ],
        0,
    )?;
    let plan = invoke(config, &["server", "configuration", "plan", id], 0)?;
    invoke(
        config,
        &[
            "server",
            "configuration",
            "commit",
            id,
            "--expected-revision",
            "4",
            "--expected-generation",
            &number_string(&plan["data"]["expected_generation"])?,
            "--expected-fingerprint",
            string(
                &plan["data"]["preview"]["fingerprint"],
                "configuration fingerprint",
            )?,
        ],
        0,
    )?;
    Ok(())
}

pub(super) fn bind_existing_content(config: &Path, case: &Path, id: &str) -> io::Result<()> {
    let bindings = case.join("bindings.json");
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"server_only","order":0}]"#,
    )?;
    let plan = invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            id,
            "--bindings",
            path_arg(&bindings)?,
        ],
        0,
    )?;
    invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "save",
            id,
            "--bindings",
            path_arg(&bindings)?,
            "--expected-plan-fingerprint",
            string(
                &plan["data"]["plan"]["fingerprint"],
                "content bindings fingerprint",
            )?,
        ],
        0,
    )?;
    Ok(())
}

pub(super) fn commit_runtime(
    config: &Path,
    id: &str,
    prepared: &Prepared,
    selection: u64,
    activation: u64,
) -> io::Result<()> {
    let plan = invoke(
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
        0,
    )?;
    invoke(
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
        0,
    )?;
    Ok(())
}

pub(super) fn select_predecessor(config: &Path, id: &str, prepared: &Prepared) -> io::Result<()> {
    let selection = invoke(
        config,
        &[
            "server",
            "deploy",
            "select-plan",
            id,
            &prepared.record,
            "--expected-deployment-generation",
            "2",
        ],
        0,
    )?;
    invoke(
        config,
        &[
            "server",
            "deploy",
            "select",
            id,
            &prepared.record,
            "--expected-deployment-generation",
            "2",
            "--expected-plan-fingerprint",
            string(
                &selection["data"]["plan"]["fingerprint"],
                "selection fingerprint",
            )?,
            "--yes",
        ],
        0,
    )?;
    let activation = invoke(
        config,
        &[
            "server",
            "deploy",
            "activation",
            "plan",
            id,
            &prepared.preparation,
            "--expected-deployment-generation",
            "3",
            "--expected-activation-generation",
            "2",
        ],
        0,
    )?;
    invoke(
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
            "3",
            "--expected-activation-generation",
            "2",
            "--expected-plan-fingerprint",
            string(
                &activation["data"]["plan"]["fingerprint"],
                "activation fingerprint",
            )?,
        ],
        0,
    )?;
    Ok(())
}
