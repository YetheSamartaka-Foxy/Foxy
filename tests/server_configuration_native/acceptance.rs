use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

#[path = "../server_lifecycle_native/inventory.rs"]
mod inventory;
#[path = "observation.rs"]
mod observation;
#[path = "../server_activation_native/preparation.rs"]
mod preparation;
#[path = "../server_lifecycle_native/transport.rs"]
mod transport;

use observation::Expected;
use preparation::{PreparationRevision, prepare, setup_content};

#[path = "imported.rs"]
mod imported;
#[path = "key_trust.rs"]
mod key_trust;
#[path = "mission_library.rs"]
mod mission_library;
pub(super) fn run_mission_library() -> io::Result<()> {
    mission_library::run()
}
pub(super) fn run_reconciliation() -> io::Result<()> {
    key_trust::run_reconciliation()
}
pub(super) fn run_key_trust() -> io::Result<()> {
    key_trust::run()
}
pub(super) fn run_imported() -> io::Result<()> {
    imported::run()
}

const GAME: &str = "arma3";
const PORT: u16 = 27132;
const PROFILE: &str = "Managed";
const SERVER_CONFIG: &str = "dedicated.cfg";
const BASIC_CONFIG: &str = "network.cfg";

pub(super) fn run() -> io::Result<()> {
    let source = inventory::required_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = inventory::required_directory("FOXY_ARMA_CONFIGURATION_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and configuration scratch must be separate",
        ));
    }
    let case = inventory::fresh_child(&scratch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&case, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    if case.to_string_lossy().len() > 60 {
        return Err(io::Error::other(
            "Configuration scratch path is too long for native Windows arguments",
        ));
    }
    require_port_range_free(PORT)?;
    let source_hash = inventory::tree_hash(&source)?;
    let adopted = case.join("adopted");
    inventory::copy_tree(&source, &adopted)?;
    let adopted_hash = inventory::tree_hash(&adopted)?;
    if inventory::tree_hash(&source)? != source_hash || adopted_hash != source_hash {
        return Err(io::Error::other(
            "Adopted native copy does not match source",
        ));
    }
    let config = case.join("config");
    fs::create_dir(&config)?;
    fs::write(
        config.join("games.json"),
        include_bytes!("../../examples/json/appdata/games.json"),
    )?;

    let nonce = format!("{:x}", nonce());
    let host_a = format!("FoxyCfg A \"{nonce}\" C:\\cfg");
    let host_b = format!("FoxyCfg B \"{nonce}\" C:\\cfg");
    let identity_a = format!("ConfigA_{nonce}");
    let identity_b = format!("ConfigB_{nonce}");
    let added = invoke(
        &config,
        &["server", "instance", "add", "--name", "ConfigurationNative"],
        "add instance",
    )?;
    let id = string(&added["data"]["instance"]["id"], "instance id")?.to_owned();

    let mut draft =
        invoke(&config, &["server", "instance", "show", &id], "show draft")?["data"]["instance"]
            .clone();
    draft["definition"]["settings"] = generation_a(&host_a);
    replace(&config, &case, &draft, 1, "draft-a.json")?;
    invoke(
        &config,
        &[
            "server",
            "instance",
            "adopt",
            &id,
            "--directory",
            path_arg(&adopted)?,
            "--expected-revision",
            "2",
        ],
        "adopt installation",
    )?;
    let preview_a = commit_configuration(&config, &id, 3)?;
    require_preview_files(&preview_a, "Managed.Arma3Profile")?;
    setup_content(&config, &case, &id)?;
    let prepared_a = prepare(
        &config,
        &case,
        &id,
        "alpha",
        &identity_a,
        PreparationRevision {
            deployment: 0,
            draft: 3,
            configuration: 1,
        },
    )?;
    activate(&config, &id, &prepared_a.preparation, 1, 0)?;
    let plan_a = start_plan(&config, &id)?;
    let launch = launch_paths(&plan_a.launch)?;
    let state = runtime_state(&config, &id);
    let user = launch.profiles.join(user_directory()).join(PROFILE);
    let vars = user.join(format!("{PROFILE}.vars.Arma3Profile"));
    let engine_a = launch.engine.clone();
    let engine_hash_a = inventory::tree_hash(&engine_a)?;

    let owner = transport::start_owner(&config)?;
    let expected_a = Expected {
        identity: &identity_a,
        hostname: &host_a,
        crosshair: 1,
        third_person: 0,
        tactical_ping: 3,
        parameter: 17,
        profile: "missing",
        mission: "missing",
    };
    let observed_a = run_once(&config, &id, &state, &plan_a, &expected_a)?;
    report_engine_saved_profile(&user, &preview_a)?;
    if read_ordinary(&vars)?.is_empty() {
        return Err(io::Error::other(
            "The engine did not persist the managed profile namespace",
        ));
    }

    let mut newer =
        invoke(&config, &["server", "instance", "show", &id], "show draft")?["data"]["instance"]
            .clone();
    newer["definition"]["settings"] = generation_b(&host_b);
    replace(&config, &case, &newer, 3, "draft-b.json")?;
    let still_a = start_plan(&config, &id)?;
    if still_a.generation != plan_a.generation
        || still_a.fingerprint != plan_a.fingerprint
        || still_a.runtime != plan_a.runtime
    {
        return Err(io::Error::other(
            "An uncommitted draft changed the committed start plan",
        ));
    }
    let preview_draft = invoke(
        &config,
        &["server", "configuration", "plan", &id],
        "plan newer draft configuration",
    )?;
    if preview_draft["data"]["preview"]["fingerprint"] == preview_a["fingerprint"] {
        return Err(io::Error::other(
            "The newer draft preview did not change its fingerprint",
        ));
    }
    let retained_a = Expected {
        profile: "foxy-managed-profile-retained",
        mission: "foxy-managed-mission-retained",
        ..expected_a
    };
    let observed_again = run_once(&config, &id, &state, &still_a, &retained_a)?;
    if (observed_again.aiming - observed_a.aiming).abs() > 1e-6
        || (observed_again.courage - observed_a.courage).abs() > 1e-6
    {
        return Err(io::Error::other(
            "Restarting the committed runtime changed the effective AI level",
        ));
    }
    report_engine_saved_profile(&user, &preview_a)?;

    let preview_b = commit_configuration(&config, &id, 4)?;
    let prepared_b = prepare(
        &config,
        &case,
        &id,
        "beta",
        &identity_b,
        PreparationRevision {
            deployment: 1,
            draft: 4,
            configuration: 2,
        },
    )?;
    if prepared_b.record == prepared_a.record || prepared_b.identity != identity_b {
        return Err(io::Error::other(
            "The newer committed configuration did not produce a new deployment record",
        ));
    }
    let vars_before = read_ordinary(&vars)?;
    activate(&config, &id, &prepared_b.preparation, 2, 1)?;
    if read_ordinary(&vars)? != vars_before {
        return Err(io::Error::other(
            "Activating a new generation changed the engine profile namespace",
        ));
    }
    let plan_b = start_plan(&config, &id)?;
    if plan_b.runtime == plan_a.runtime || plan_b.fingerprint == plan_a.fingerprint {
        return Err(io::Error::other("The committed generation did not change"));
    }
    launch_paths(&plan_b.launch)?;
    let observed_b = run_once(
        &config,
        &id,
        &state,
        &plan_b,
        &Expected {
            identity: &identity_b,
            hostname: &host_b,
            crosshair: 0,
            third_person: 2,
            tactical_ping: 2,
            parameter: 23,
            profile: "foxy-managed-profile-retained",
            mission: "foxy-managed-mission-retained",
        },
    )?;
    if observed_b.aiming <= observed_a.aiming || observed_b.courage <= observed_a.courage {
        return Err(io::Error::other(format!(
            "Higher committed custom AI levels did not raise final AI skill: A {}/{} B {}/{}",
            observed_a.aiming, observed_a.courage, observed_b.aiming, observed_b.courage
        )));
    }
    report_engine_saved_profile(&user, &preview_b)?;
    if inventory::tree_hash(&engine_a)? != engine_hash_a
        || inventory::tree_hash(&source)? != source_hash
        || inventory::tree_hash(&adopted)? != adopted_hash
    {
        return Err(io::Error::other(
            "Native source, adopted copy or predecessor runtime changed",
        ));
    }
    drop(owner);
    eprintln!(
        "Native configuration evidence: A aim {:.4} courage {:.4}; B aim {:.4} courage {:.4}; case {}",
        observed_a.aiming,
        observed_a.courage,
        observed_b.aiming,
        observed_b.courage,
        case.display()
    );
    Ok(())
}

fn generation_a(hostname: &str) -> Value {
    json!({
        "hostname": hostname,
        "game_port": PORT,
        "max_players": 1,
        "persistent": true,
        "auto_init": true,
        "battleye": false,
        "verify_signatures": "v2",
        "mission_cycle": [{
            "template": super::mission::TEMPLATE,
            "difficulty": "custom",
            "params": {"Probe": 17}
        }],
        "basic": {
            "min_bandwidth": 131072,
            "max_bandwidth": 10_000_000_000u64,
            "max_msg_send": 128,
            "max_size_guaranteed": 512,
            "max_size_nonguaranteed": 256,
            "min_error_to_send": 0.001,
            "min_error_to_send_near": 0.01,
            "max_custom_file_size": 0,
            "sockets": {"max_packet_size": 1400}
        },
        "server": {
            "admins": ["76561197960265728"],
            "motd": ["Foxy native configuration", ""],
            "motd_interval": 2.5,
            "kick_duplicate": true,
            "allowed_file_patching": 0,
            "file_patching_exceptions": [],
            "allowed_load_file_extensions": ["sqf", "txt", "hpp", "ext"],
            "allowed_preprocess_file_extensions": ["sqf", "sqs", "hpp", "ext", "inc", "cpp", "txt", "fsm"],
            "allowed_html_load_extensions": ["htm", "html", "txt"],
            "allowed_html_load_uris": [],
            "headless_clients": ["127.0.0.1"],
            "local_client": ["127.0.0.1"],
            "vote_threshold": 0.33,
            "vote_mission_players": 1,
            "allowed_vote_cmds": [["kick", false, true, 0.51], ["missions", false, false]],
            "allowed_voted_admin_cmds": [["#kick", true, true]],
            "loopback": true,
            "upnp": false,
            "steam_protocol_max_data_size": 1024,
            "disconnect_timeout": 90,
            "max_desync": 150,
            "max_ping": 200,
            "max_packet_loss": 50,
            "kick_clients_on_slow_network": [false, false, false, false],
            "kick_timeout": [[0, -1], [1, 180], [2, 180], [3, 0]],
            "lobby_idle_timeout": 300,
            "role_timeout": [90, 120],
            "briefing_timeout": [60, 90],
            "voting_timeout": [60, 90],
            "debriefing_timeout": [45, 60],
            "skip_lobby": false,
            "drawing_in_map": true,
            "force_rotor_lib_simulation": 0,
            "allow_profile_glasses": true,
            "zeus_composition_script_level": 2,
            "override_haze_quality": -1,
            "disable_von": false,
            "von_codec": 1,
            "von_codec_quality": 30,
            "disable_channels": [[0, true, false], [6, false, true]],
            "timestamp_format": "short",
            "timestamp_format_console": "short",
            "enable_player_diag": false,
            "call_ext_report_limit": 1000,
            "idle_fps_limit": 10,
            "auto_select_mission": true,
            "random_mission_order": false,
            "missions_to_server_restart": 0,
            "double_id_detected": "",
            "on_user_connected": "",
            "on_user_disconnected": "",
            "on_user_kicked": "",
            "on_hacked_data": "",
            "on_unsigned_data": "",
            "anti_flood": {"cycle_time": 0.5, "cycle_limit": 400, "cycle_hard_limit": 4000, "enable_kick": false},
            "advanced_options": {"log_object_not_found": true, "skip_description_parsing": false, "ignore_mission_load_errors": false, "queue_size_log_g": 1000000}
        },
        "difficulty": difficulty(1, 0, 3, 0.35, 0.25),
        "configuration": {"server": SERVER_CONFIG, "basic": BASIC_CONFIG, "profile_name": PROFILE}
    })
}

fn generation_b(hostname: &str) -> Value {
    let mut settings = generation_a(hostname);
    settings["mission_cycle"][0]["params"]["Probe"] = 23.into();
    settings["basic"]["min_error_to_send"] = 0.002.into();
    settings["server"]["motd"] = json!([]);
    settings["difficulty"] = difficulty(0, 2, 2, 0.65, 0.55);
    settings
}

fn difficulty(crosshair: u8, third: u8, ping: u8, skill: f64, precision: f64) -> Value {
    json!({
        "options": {
            "reduced_damage": false,
            "group_indicators": 0,
            "friendly_tags": 0,
            "enemy_tags": 0,
            "detected_mines": 0,
            "commands": 1,
            "waypoints": 1,
            "tactical_ping": ping,
            "weapon_info": 1,
            "stance_indicator": 1,
            "stamina_bar": false,
            "weapon_crosshair": crosshair == 1,
            "vision_aid": false,
            "third_person_view": third,
            "camera_shake": true,
            "score_table": true,
            "death_messages": true,
            "von_id": true,
            "map_content_friendly": false,
            "map_content_enemy": false,
            "map_content_mines": false,
            "auto_report": false,
            "multiple_saves": false
        },
        "ai_level_preset": 3,
        "custom_ai": {"skill_ai": skill, "precision_ai": precision}
    })
}

fn replace(
    config: &Path,
    case: &Path,
    instance: &Value,
    revision: u64,
    name: &str,
) -> io::Result<()> {
    let file = case.join(name);
    fs::write(&file, serde_json::to_vec(instance)?)?;
    invoke(
        config,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            path_arg(&file)?,
            "--expected-revision",
            &revision.to_string(),
        ],
        "replace draft",
    )?;
    Ok(())
}

fn commit_configuration(config: &Path, id: &str, revision: u64) -> io::Result<Value> {
    let plan = invoke(
        config,
        &["server", "configuration", "plan", id],
        "plan configuration",
    )?;
    let preview = plan["data"]["preview"].clone();
    invoke(
        config,
        &[
            "server",
            "configuration",
            "commit",
            id,
            "--expected-revision",
            &revision.to_string(),
            "--expected-generation",
            &number(
                &plan["data"]["expected_generation"],
                "configuration generation",
            )?
            .to_string(),
            "--expected-fingerprint",
            string(&preview["fingerprint"], "configuration fingerprint")?,
        ],
        "commit configuration",
    )?;
    Ok(preview)
}

fn require_preview_files(preview: &Value, profile: &str) -> io::Result<()> {
    let names = preview["files"]
        .as_array()
        .ok_or_else(|| io::Error::other("Preview files are missing"))?
        .iter()
        .filter_map(|file| file["relative_path"].as_str())
        .collect::<Vec<_>>();
    for expected in [SERVER_CONFIG, BASIC_CONFIG, profile, "launch-intent.json"] {
        if !names.contains(&expected) {
            return Err(io::Error::other(format!(
                "Committed preview is missing {expected}: {names:?}"
            )));
        }
    }
    if names.contains(&"server.cfg") || names.contains(&"basic.cfg") {
        return Err(io::Error::other(
            "Default configuration names were also rendered",
        ));
    }
    Ok(())
}

fn report_engine_saved_profile(user: &Path, preview: &Value) -> io::Result<()> {
    let expected = preview["files"]
        .as_array()
        .and_then(|files| {
            files
                .iter()
                .find(|file| file["relative_path"] == format!("{PROFILE}.Arma3Profile"))
        })
        .and_then(|file| file["contents"].as_str())
        .ok_or_else(|| io::Error::other("Preview lacks the managed profile"))?;
    let actual = String::from_utf8_lossy(&read_ordinary(
        &user.join(format!("{PROFILE}.Arma3Profile")),
    )?)
    .replace("\r\n", "\n");
    let difficulty = |text: &str| {
        text.find("class DifficultyPresets").map(|start| {
            text[start..]
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| line.replace(' ', ""))
                .collect::<Vec<_>>()
        })
    };
    let wanted = difficulty(expected).unwrap_or_default();
    let found = difficulty(&actual).unwrap_or_default();
    if found.is_empty() || !wanted.iter().all(|line| found.contains(line)) {
        eprintln!(
            "Engine-saved profile does not repeat the generated difficulty verbatim; runtime observation remains the acceptance proof"
        );
    }
    Ok(())
}

fn activate(
    config: &Path,
    id: &str,
    preparation: &str,
    selection: u64,
    activation: u64,
) -> io::Result<()> {
    let selection = selection.to_string();
    let activation = activation.to_string();
    let plan = invoke(
        config,
        &[
            "server",
            "deploy",
            "activation",
            "plan",
            id,
            preparation,
            "--expected-deployment-generation",
            &selection,
            "--expected-activation-generation",
            &activation,
        ],
        "plan activation",
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
            preparation,
            "--expected-deployment-generation",
            &selection,
            "--expected-activation-generation",
            &activation,
            "--expected-plan-fingerprint",
            string(
                &plan["data"]["plan"]["fingerprint"],
                "activation fingerprint",
            )?,
        ],
        "apply activation",
    )?;
    Ok(())
}

struct StartPlan {
    generation: String,
    fingerprint: String,
    runtime: String,
    launch: Value,
}

fn start_plan(config: &Path, id: &str) -> io::Result<StartPlan> {
    let value = invoke(
        config,
        &["server", "instance", "start-plan", id],
        "start plan",
    )?;
    let plan = &value["data"]["plan"];
    Ok(StartPlan {
        generation: number(&plan["activation_generation"], "activation generation")?.to_string(),
        fingerprint: string(&plan["fingerprint"], "start fingerprint")?.to_owned(),
        runtime: string(&plan["runtime_id"], "runtime id")?.to_owned(),
        launch: plan["launch"].clone(),
    })
}

struct LaunchPaths {
    engine: PathBuf,
    profiles: PathBuf,
}

fn launch_paths(launch: &Value) -> io::Result<LaunchPaths> {
    let arguments = launch["arguments"]
        .as_array()
        .ok_or_else(|| io::Error::other("Launch arguments are missing"))?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let value = |flag: &str| {
        arguments
            .iter()
            .find_map(|argument| argument.strip_prefix(flag))
            .ok_or_else(|| io::Error::other(format!("Start plan lacks {flag}")))
    };
    for (flag, name) in [("-config=", SERVER_CONFIG), ("-cfg=", BASIC_CONFIG)] {
        let path = PathBuf::from(value(flag)?);
        if path.file_name().and_then(|name| name.to_str()) != Some(name) || !path.is_file() {
            return Err(io::Error::other(format!(
                "Start plan does not reference the committed {name}"
            )));
        }
    }
    if value("-name=")? != PROFILE {
        return Err(io::Error::other(
            "Start plan does not select the managed profile",
        ));
    }
    let profiles = PathBuf::from(value("-profiles=")?);
    let seed = &launch["profile_seed"];
    let destination = string(&seed["destination"], "profile seed destination")?;
    let destination = PathBuf::from(destination.strip_prefix(r"\\?\").unwrap_or(destination));
    if destination
        != profiles
            .join(user_directory())
            .join(PROFILE)
            .join(format!("{PROFILE}.Arma3Profile"))
    {
        return Err(io::Error::other(format!(
            "Profile seed targets an unexpected file: {}",
            destination.display()
        )));
    }
    let executable = PathBuf::from(string(&launch["executable"], "planned executable")?);
    Ok(LaunchPaths {
        engine: executable
            .parent()
            .ok_or_else(|| io::Error::other("Executable has no parent"))?
            .to_path_buf(),
        profiles,
    })
}

fn user_directory() -> &'static str {
    if cfg!(windows) { "Users" } else { "home" }
}

fn run_once(
    config: &Path,
    id: &str,
    state: &Path,
    plan: &StartPlan,
    expected: &Expected<'_>,
) -> io::Result<observation::Observed> {
    let before = observation::snapshot(state)?;
    invoke(
        config,
        &[
            "server",
            "instance",
            "start",
            id,
            "--expected-activation-generation",
            &plan.generation,
            "--expected-plan-fingerprint",
            &plan.fingerprint,
            "--yes",
        ],
        "start",
    )?;
    let status = invoke(config, &["server", "instance", "status", id], "status")?;
    let run = number(
        &status["data"]["status"]["run_generation"],
        "run generation",
    )?;
    let observed = observation::wait(state, &before, expected, Duration::from_secs(240));
    let stop = invoke(
        config,
        &[
            "server",
            "instance",
            "stop",
            id,
            "--expected-run-generation",
            &run.to_string(),
            "--timeout-ms",
            "120000",
            "--yes",
        ],
        "stop",
    );
    let observed = observed?;
    let stop = stop?;
    if stop["data"]["outcome"]["status"]["forced"] != false {
        return Err(io::Error::other(format!(
            "Graceful stop was forced: {stop}"
        )));
    }
    Ok(observed)
}

fn runtime_state(config: &Path, id: &str) -> PathBuf {
    config
        .join("games")
        .join(GAME)
        .join("server/instances")
        .join(id)
        .join("deployments/runtime-state")
}

fn read_ordinary(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(io::Error::other(format!(
            "Profile file is not an ordinary bounded file: {}",
            path.display()
        )));
    }
    fs::read(path)
}

fn require_port_range_free(port: u16) -> io::Result<()> {
    let mut reservations = Vec::new();
    for current in port..=port + 4 {
        reservations.push((
            std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, current))?,
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, current))?,
        ));
    }
    Ok(())
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn invoke(config: &Path, args: &[&str], operation: &str) -> io::Result<Value> {
    transport::invoke(config, args, 0)
        .map_err(|error| io::Error::other(format!("{operation}: {error}")))
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

fn number(value: &Value, name: &str) -> io::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}
