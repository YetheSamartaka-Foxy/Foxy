use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::transport;

pub(super) struct PreparedInstance {
    pub config_root: PathBuf,
    pub id: String,
    pub name: String,
    pub installation_id: String,
    pub runtime_id: String,
    pub engine_root: PathBuf,
    pub profile_argument_root: PathBuf,
    pub profile_root: PathBuf,
    pub engine_log_root: PathBuf,
    pub server_config: PathBuf,
    pub instance_root: PathBuf,
    pub a2s_port: u16,
    pub expected_mods: Value,
}

pub(super) fn prepare_instance(
    config: &Path,
    case: &Path,
    source: &Path,
    hostname: &str,
    game_port: u16,
    a2s_port: u16,
    with_addons: bool,
) -> io::Result<PreparedInstance> {
    let added = transport::invoke(
        config,
        &["server", "instance", "add", "--name", hostname],
        0,
    )?;
    let id = value_string(&added["data"]["instance"]["id"], "instance id")?.to_owned();
    let mut instance = transport::invoke(config, &["server", "instance", "show", &id], 0)?["data"]
        ["instance"]
        .clone();
    let settings = &mut instance["definition"]["settings"];
    settings["name"] = hostname.into();
    settings["bind_port"] = game_port.into();
    settings["bind_address"] = Ipv4Addr::LOCALHOST.to_string().into();
    settings["a2s_port"] = a2s_port.into();
    settings["max_players"] = 4.into();
    settings["visible"] = false.into();
    settings["scenario_id"] = super::SCENARIO.into();
    settings["addons"] = serde_json::json!([]);
    if with_addons {
        let addon = required_guid("FOXY_REFORGER_ADDON_ID")?;
        let version = std::env::var("FOXY_REFORGER_ADDON_VERSION").ok();
        settings["addons"] =
            serde_json::json!([{"addon_id":addon,"version":version,"required":null}]);
    }
    let draft = case.join(format!("draft_{}.json", id));
    fs::write(&draft, serde_json::to_vec(&instance)?)?;
    let draft_path = path_arg(&draft)?;
    transport::invoke(
        config,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            &draft_path,
            "--expected-revision",
            "1",
        ],
        0,
    )?;
    let source_path = path_arg(source)?;
    transport::invoke(
        config,
        &[
            "server",
            "instance",
            "adopt",
            &id,
            "--directory",
            &source_path,
            "--expected-revision",
            "2",
        ],
        0,
    )?;
    let installation_id =
        value_string(
            &transport::invoke(config, &["server", "instance", "show", &id], 0)?["data"]["instance"]
                ["installation_id"],
            "installation id",
        )?
        .to_owned();
    save_configuration(config, &id)?;
    let stage_id = save_content_stage(config, case, &id, with_addons)?;
    save_select_prepare_activate(config, &id, &stage_id)?;
    let plan = transport::invoke(config, &["server", "instance", "start-plan", &id], 0)?;
    let plan = &plan["data"]["plan"];
    let runtime_id = value_string(&plan["runtime_id"], "runtime id")?.to_owned();
    let executable = PathBuf::from(value_string(
        &plan["launch"]["executable"],
        "planned executable",
    )?);
    let engine_root = executable
        .parent()
        .ok_or_else(|| io::Error::other("Planned executable has no engine directory"))?
        .to_path_buf();
    let arguments = plan["launch"]["arguments"]
        .as_array()
        .ok_or_else(|| io::Error::other("Start plan omitted launch arguments"))?;
    let profile_argument_root = argument_path(arguments, "-profile")?;
    let profile_root = profile_argument_root.join("profile");
    let server_config = argument_path(arguments, "-config")?;
    let engine_log_root = argument_path(arguments, "-logsDir")?;
    let instance_root = config
        .join("games")
        .join("reforger")
        .join("server")
        .join("instances")
        .join(&id);
    let expected_mods = if with_addons {
        let selected = transport::invoke(config, &["server", "reforger-addons", "list", &id], 0)?;
        let realized = selected["data"]["lock"]["realized"]
            .as_array()
            .ok_or_else(|| io::Error::other("Selected addon lock omitted its realized set"))?;
        serde_json::Value::Array(realized.iter().map(|item| serde_json::json!({"modId":item["addon_id"].as_str().unwrap_or_default().to_ascii_uppercase(),"version":item["version"],"required":true})).collect())
    } else {
        serde_json::json!([])
    };
    Ok(PreparedInstance {
        config_root: config.to_path_buf(),
        id,
        name: hostname.to_owned(),
        installation_id,
        runtime_id,
        engine_root,
        profile_argument_root,
        profile_root,
        engine_log_root,
        server_config,
        instance_root,
        a2s_port,
        expected_mods,
    })
}

fn save_configuration(config: &Path, id: &str) -> io::Result<()> {
    let plan = transport::invoke(config, &["server", "configuration", "plan", id], 0)?;
    let generation = number_string(
        &plan["data"]["expected_generation"],
        "configuration generation",
    )?;
    let fingerprint = value_string(
        &plan["data"]["preview"]["fingerprint"],
        "configuration fingerprint",
    )?
    .to_owned();
    let stale_arguments = [
        "server",
        "configuration",
        "commit",
        id,
        "--expected-revision",
        "2",
        "--expected-generation",
        &generation,
        "--expected-fingerprint",
        &fingerprint,
    ];
    let (code, _) = transport::invoke_raw(config, &stale_arguments)?;
    if code != 2 {
        return Err(io::Error::other(
            "Stale Reforger configuration revision was not refused",
        ));
    }
    let arguments = [
        "server",
        "configuration",
        "commit",
        id,
        "--expected-revision",
        "3",
        "--expected-generation",
        &generation,
        "--expected-fingerprint",
        &fingerprint,
    ];
    let mut dry = vec!["--dry-run"];
    dry.extend(arguments);
    transport::invoke(config, &dry, 0)?;
    transport::invoke(config, &arguments, 0)?;
    Ok(())
}

fn save_content_stage(
    config: &Path,
    case: &Path,
    id: &str,
    with_addons: bool,
) -> io::Result<String> {
    let bindings_file = case.join(format!("bindings_{}.json", id));
    fs::write(&bindings_file, b"[]")?;
    let bindings_path = path_arg(&bindings_file)?;
    let binding_plan = transport::invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            id,
            "--bindings",
            &bindings_path,
        ],
        0,
    )?;
    let binding_fingerprint = value_string(
        &binding_plan["data"]["plan"]["fingerprint"],
        "empty bindings fingerprint",
    )?;
    transport::invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "save",
            id,
            "--bindings",
            &bindings_path,
            "--expected-plan-fingerprint",
            binding_fingerprint,
        ],
        0,
    )?;
    if with_addons {
        acquire_addons(config, id)?;
    }
    let stage_plan = transport::invoke(config, &["server", "content", "stage", "plan", id], 0)?;
    let plan = &stage_plan["data"]["plan"];
    let stage_id = value_string(&plan["stage_id"], "empty stage id")?.to_owned();
    let fingerprint = value_string(&plan["fingerprint"], "empty stage fingerprint")?;
    transport::invoke(
        config,
        &[
            "server",
            "content",
            "stage",
            "apply",
            id,
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        0,
    )?;
    let verified = transport::invoke(
        config,
        &["server", "content", "stage", "verify", id, &stage_id],
        0,
    )?;
    if !with_addons
        && (verified["data"]["stage"]["files"] != 0 || verified["data"]["stage"]["bytes"] != 0)
    {
        return Err(io::Error::other("Reforger payload stage was not empty"));
    }
    Ok(stage_id)
}

fn required_guid(name: &str) -> io::Result<String> {
    let value = std::env::var(name)
        .map_err(|_| io::Error::other("An explicit native addon or dependency GUID is required"))?;
    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other(
            "Native addon GUID must contain exactly 16 hex digits",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn acquire_addons(config: &Path, id: &str) -> io::Result<()> {
    let requested = required_guid("FOXY_REFORGER_ADDON_ID")?;
    let dependency = required_guid("FOXY_REFORGER_DEPENDENCY_ID")?;
    let plan = transport::invoke(config, &["server", "reforger-addons", "plan", id], 0)?;
    if plan["data"]["plan"]["blockers"]
        .as_array()
        .is_none_or(|items| !items.is_empty())
    {
        return Err(io::Error::other(
            "Native addon acquisition review is blocked",
        ));
    }
    let fingerprint = value_string(
        &plan["data"]["plan"]["fingerprint"],
        "addon review fingerprint",
    )?;
    let args = [
        "server",
        "reforger-addons",
        "acquire",
        id,
        "--expected-plan-fingerprint",
        fingerprint,
    ];
    let mut dry = vec!["--dry-run"];
    dry.extend(args);
    transport::invoke(config, &dry, 0)?;
    let (code, _) = transport::invoke_raw(config, &args)?;
    if code != 2 {
        return Err(io::Error::other("Addon acquisition did not enforce --yes"));
    }
    let mut apply = vec!["--yes"];
    apply.extend(args);
    let result = transport::invoke(config, &apply, 0)?;
    let lock = &result["data"]["lock"];
    let realized = lock["realized"]
        .as_array()
        .ok_or_else(|| io::Error::other("Native acquisition omitted its lock"))?;
    for expected in [&requested, &dependency] {
        if !realized.iter().any(|item| {
            item["addon_id"].as_str() == Some(expected)
                && item["version"]
                    .as_str()
                    .is_some_and(|version| !version.is_empty())
                && item["provider"] == "bohemia_workshop"
        }) {
            return Err(io::Error::other(
                "Native acquisition did not retain the requested addon and dependency with exact provider versions",
            ));
        }
    }
    if !realized
        .iter()
        .find(|item| item["addon_id"] == requested)
        .is_some_and(|item| {
            item["dependencies"]
                .as_array()
                .is_some_and(|items| items.iter().any(|edge| edge["addon_id"] == dependency))
        })
    {
        return Err(io::Error::other(
            "The exact native dependency edge was not retained",
        ));
    }
    fs::write(
        config.join("native-addon-acquisition.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(())
}

fn save_select_prepare_activate(config: &Path, id: &str, stage_id: &str) -> io::Result<()> {
    let composition = transport::invoke(
        config,
        &[
            "server",
            "deploy",
            "plan",
            id,
            "--stage",
            stage_id,
            "--expected-revision",
            "3",
            "--expected-configuration-generation",
            "1",
        ],
        0,
    )?;
    if value_string(
        &composition["data"]["plan"]["fingerprint"],
        "deployment composition fingerprint",
    )?
    .is_empty()
    {
        return Err(io::Error::other(
            "Deployment composition fingerprint is empty",
        ));
    }
    let record_plan = transport::invoke(
        config,
        &[
            "server",
            "deploy",
            "prepare",
            id,
            "--stage",
            stage_id,
            "--expected-revision",
            "3",
            "--expected-configuration-generation",
            "1",
            "--expected-deployment-generation",
            "0",
        ],
        0,
    )?;
    let plan = &record_plan["data"]["plan"];
    let record_id = value_string(&plan["record_id"], "deployment record id")?.to_owned();
    let record_fingerprint = value_string(&plan["fingerprint"], "record fingerprint")?.to_owned();
    let mut dry = vec!["--dry-run"];
    dry.extend(record_commit_args(id, stage_id, &record_fingerprint));
    transport::invoke(config, &dry, 0)?;
    transport::invoke(
        config,
        &record_commit_args(id, stage_id, &record_fingerprint),
        0,
    )?;

    let selection = transport::invoke(
        config,
        &[
            "server",
            "deploy",
            "select-plan",
            id,
            &record_id,
            "--expected-deployment-generation",
            "1",
        ],
        0,
    )?;
    let selection_fingerprint = value_string(
        &selection["data"]["plan"]["fingerprint"],
        "selection fingerprint",
    )?
    .to_owned();
    let select_args = [
        "server",
        "deploy",
        "select",
        id,
        &record_id,
        "--expected-deployment-generation",
        "1",
        "--expected-plan-fingerprint",
        &selection_fingerprint,
    ];
    let (code, _) = transport::invoke_raw(config, &select_args)?;
    if code != 2 {
        return Err(io::Error::other(
            "Unconfirmed Reforger record selection was not refused",
        ));
    }
    let mut dry = vec!["--dry-run"];
    dry.extend(select_args);
    transport::invoke(config, &dry, 0)?;
    let mut confirmed = vec!["--yes"];
    confirmed.extend(select_args);
    transport::invoke(config, &confirmed, 0)?;

    let runtime_plan = transport::invoke(
        config,
        &[
            "server",
            "deploy",
            "runtime",
            "plan",
            id,
            "--expected-deployment-generation",
            "1",
        ],
        0,
    )?;
    let runtime_fingerprint = value_string(
        &runtime_plan["data"]["plan"]["fingerprint"],
        "runtime preparation fingerprint",
    )?;
    let runtime_build_args = [
        "server",
        "deploy",
        "runtime",
        "build",
        id,
        "--expected-deployment-generation",
        "1",
        "--expected-plan-fingerprint",
        runtime_fingerprint,
    ];
    let mut dry = vec!["--dry-run"];
    dry.extend(runtime_build_args);
    transport::invoke(config, &dry, 0)?;
    let runtime_build = transport::invoke(config, &runtime_build_args, 0)?;
    let preparation = value_string(
        &runtime_build["data"]["report"]["preparation_id"],
        "runtime preparation id",
    )?
    .to_owned();
    let activation = transport::invoke(
        config,
        &[
            "server",
            "deploy",
            "activation",
            "plan",
            id,
            &preparation,
            "--expected-deployment-generation",
            "1",
            "--expected-activation-generation",
            "0",
        ],
        0,
    )?;
    let activation_fingerprint = value_string(
        &activation["data"]["plan"]["fingerprint"],
        "activation fingerprint",
    )?
    .to_owned();
    let activation_args = [
        "server",
        "deploy",
        "activation",
        "apply",
        id,
        &preparation,
        "--expected-deployment-generation",
        "1",
        "--expected-activation-generation",
        "0",
        "--expected-plan-fingerprint",
        &activation_fingerprint,
    ];
    let mut dry = vec!["--dry-run"];
    dry.extend(activation_args);
    transport::invoke(config, &dry, 0)?;
    let (code, _) = transport::invoke_raw(config, &activation_args)?;
    if code != 2 {
        return Err(io::Error::other(
            "Unconfirmed Reforger activation was not refused",
        ));
    }
    let mut confirmed = vec!["--yes"];
    confirmed.extend(activation_args);
    transport::invoke(config, &confirmed, 0)?;
    Ok(())
}

fn record_commit_args<'a>(id: &'a str, stage: &'a str, fingerprint: &'a str) -> [&'a str; 14] {
    [
        "server",
        "deploy",
        "commit",
        id,
        "--stage",
        stage,
        "--expected-revision",
        "3",
        "--expected-configuration-generation",
        "1",
        "--expected-deployment-generation",
        "0",
        "--expected-plan-fingerprint",
        fingerprint,
    ]
}

fn argument_path(arguments: &[Value], flag: &str) -> io::Result<PathBuf> {
    let index = arguments
        .iter()
        .position(|value| value.as_str() == Some(flag))
        .ok_or_else(|| io::Error::other("Start plan omitted a required runtime path flag"))?;
    arguments
        .get(index + 1)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("Start plan runtime path is invalid"))
}

fn path_arg(path: &Path) -> io::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("Native test path is not representable"))
}

fn value_string<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}

fn number_string(value: &Value, label: &str) -> io::Result<String> {
    value
        .as_u64()
        .map(|number| number.to_string())
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}
