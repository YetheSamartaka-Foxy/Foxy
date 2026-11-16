use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::preparation::{PreparationRevision, prepare_stage};
use super::setup::commit_runtime;
use super::{
    inventory, invoke, invoke_raw, nonce, number, path_arg, require_port_range_free, start,
    start_plan, status, string, transport,
};

pub(super) const PORT: u16 = 27132;
pub(super) const SECRET: &str = "FoxyHc Join 7731";
const TEMPLATE: &str = "foxy_hc.vr";

pub(super) struct Prepared {
    pub(super) started_at: Instant,
    pub(super) source: PathBuf,
    pub(super) source_hash: blake3::Hash,
    pub(super) case: PathBuf,
    pub(super) config: PathBuf,
    pub(super) id: String,
    pub(super) installation: String,
    pub(super) identity: String,
    pub(super) first: Value,
    pub(super) second: Value,
    pub(super) profiles: PathBuf,
}

pub(super) fn prepare(
    scratch_variable: &str,
    clients: [&[&str]; 2],
    overrides: [&[&str]; 2],
) -> io::Result<Prepared> {
    let started_at = Instant::now();
    let source = inventory::required_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = inventory::required_directory(scratch_variable)?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and headless scratch must be separate",
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
            "Headless scratch path is too long for native Windows arguments",
        ));
    }
    let source_hash = inventory::tree_hash(&source)?;
    let adopted = case.join("adopted");
    inventory::copy_tree(&source, &adopted)?;
    if inventory::tree_hash(&adopted)? != source_hash {
        return Err(io::Error::other("Adopted copy does not match the source"));
    }
    let config = case.join("config");
    fs::create_dir(&config)?;
    fs::write(
        config.join("games.json"),
        include_bytes!("../../examples/json/appdata/games.json"),
    )?;
    require_port_range_free(PORT)?;
    let identity = format!("FoxyHc_{:x}", nonce());
    let id = string(
        &invoke(
            &config,
            &["server", "instance", "add", "--name", "HeadlessNative"],
            "add instance",
        )?["data"]["instance"]["id"],
        "instance id",
    )?
    .to_owned();

    let import = case.join("password.cfg");
    fs::write(&import, format!("password = \"{SECRET}\";\n"))?;
    let imported = invoke(
        &config,
        &[
            "--yes",
            "server",
            "configuration",
            "import",
            &id,
            "--expected-revision",
            "1",
            "--server",
            path_arg(&import)?,
            "--managed",
            "--apply",
        ],
        "import protected join password",
    )?;
    fs::remove_file(&import)?;
    no_secret(&imported, "configuration import")?;
    let mut instance =
        invoke(&config, &["server", "instance", "show", &id], "show")?["data"]["instance"].clone();
    if !instance["definition"]["settings"]["server"]["password_ref"].is_string() {
        return Err(io::Error::other(
            "Managed import did not keep a protected join password reference",
        ));
    }
    let revision = number(&instance["revision"], "draft revision")?;
    let settings = &mut instance["definition"]["settings"];
    settings["hostname"] = identity.as_str().into();
    settings["game_port"] = PORT.into();
    settings["max_players"] = 4.into();
    settings["persistent"] = true.into();
    settings["auto_init"] = true.into();
    settings["battleye"] = false.into();
    settings["mission_cycle"] = json!([{"template": TEMPLATE, "difficulty": "regular"}]);
    let draft = case.join("draft.json");
    fs::write(&draft, serde_json::to_vec(&instance)?)?;
    invoke(
        &config,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            path_arg(&draft)?,
            "--expected-revision",
            &revision.to_string(),
        ],
        "replace draft",
    )?;
    let mut revision = revision + 1;
    for arguments in clients {
        let expected = revision.to_string();
        let mut args = vec![
            "server",
            "headless",
            "add",
            &id,
            "--expected-revision",
            &expected,
        ];
        args.extend_from_slice(arguments);
        invoke(&config, &args, "add headless client")?;
        revision += 1;
    }
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
            &revision.to_string(),
        ],
        "adopt",
    )?;
    revision += 1;
    let installation = string(
        &invoke(&config, &["server", "instance", "show", &id], "show")?["data"]["instance"]
            ["installation_id"],
        "installation id",
    )?
    .to_owned();
    let configuration = invoke(
        &config,
        &["server", "configuration", "plan", &id],
        "plan configuration",
    )?;
    let server_cfg = configuration["data"]["preview"]["files"]
        .as_array()
        .and_then(|files| {
            files
                .iter()
                .find(|file| file["relative_path"] == "server.cfg")
        })
        .and_then(|file| file["contents"].as_str())
        .ok_or_else(|| io::Error::other("Configuration plan has no server.cfg"))?;
    if !server_cfg.contains("headlessClients[] = {\"127.0.0.1\"};") || server_cfg.contains(SECRET) {
        return Err(io::Error::other(
            "Rendered server.cfg must admit loopback HCs and keep the password redacted",
        ));
    }
    invoke(
        &config,
        &[
            "server",
            "configuration",
            "commit",
            &id,
            "--expected-revision",
            &revision.to_string(),
            "--expected-generation",
            &number(&configuration["data"]["expected_generation"], "generation")?.to_string(),
            "--expected-fingerprint",
            string(
                &configuration["data"]["preview"]["fingerprint"],
                "configuration fingerprint",
            )?,
        ],
        "commit configuration",
    )?;
    write_content(&case, &identity)?;
    register_content(&config, &case, &id)?;
    let stage_plan = invoke(
        &config,
        &["server", "content", "stage", "plan", &id],
        "plan content stage",
    )?;
    let stage_id = string(&stage_plan["data"]["plan"]["stage_id"], "stage id")?.to_owned();
    invoke(
        &config,
        &[
            "server",
            "content",
            "stage",
            "apply",
            &id,
            "--expected-plan-fingerprint",
            string(
                &stage_plan["data"]["plan"]["fingerprint"],
                "stage fingerprint",
            )?,
        ],
        "apply content stage",
    )?;
    let prepared = prepare_stage(
        &config,
        &id,
        &stage_id,
        PreparationRevision {
            deployment: 0,
            draft: revision,
            configuration: 1,
        },
        identity.clone(),
    )?;
    commit_runtime(&config, &id, &prepared, 1, 0)?;

    let reviewed = invoke(
        &config,
        &["server", "headless", "plan", &id],
        "review headless plans",
    )?;
    no_secret(&reviewed, "headless plan")?;
    let plans = reviewed["data"]["headless_clients"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (first, second) = match plans.as_slice() {
        [first, second] => (first.clone(), second.clone()),
        _ => return Err(io::Error::other("Expected two committed headless plans")),
    };
    check_plan(&first, "HC1", "-mod=foxy-content/@mod_01", overrides[0])?;
    check_plan(&second, "HC2", "-mod=foxy-content/@mod_02", overrides[1])?;
    let start_review = invoke(
        &config,
        &["server", "instance", "start-plan", &id],
        "start plan",
    )?;
    no_secret(&start_review, "start plan")?;
    let profiles = PathBuf::from(string(
        &start_review["data"]["plan"]["launch"]["profiles"],
        "server profiles",
    )?);
    let server_arguments = start_review["data"]["plan"]["launch"]["arguments"].to_string();
    if !server_arguments.contains("-mod=foxy-content/@mod_01")
        || !server_arguments.contains("-serverMod=foxy-content/@mod_02")
    {
        return Err(io::Error::other(
            "Server plan must load the required and server-only sources",
        ));
    }
    Ok(Prepared {
        started_at,
        source,
        source_hash,
        case,
        config,
        id,
        installation,
        identity,
        first,
        second,
        profiles,
    })
}

pub(in super::super) fn run() -> io::Result<()> {
    let Prepared {
        started_at,
        source,
        source_hash,
        case,
        config,
        id,
        installation,
        identity,
        first,
        second,
        profiles,
    } = prepare(
        "FOXY_ARMA_HEADLESS_SCRATCH",
        [
            &["--name", "HC1", "--order", "0", "--limit-fps", "50"],
            &[
                "--name",
                "HC2",
                "--order",
                "1",
                "--no-inherit-required",
                "--include",
                "RepoBeta",
                "--cpu-count",
                "2",
                "--ex-threads",
                "3",
                "--max-mem",
                "2048",
            ],
        ],
        [
            &["-limitFPS=50"],
            &["-cpuCount=2", "-exThreads=3", "-maxMem=2048"],
        ],
    )?;
    let owner = transport::start_owner(&config)?;
    let plan = start_plan(&config, &id)?;
    start(&config, &id, &plan)?;
    let run = number(
        &status(&config, &id)?["data"]["status"]["run_generation"],
        "run generation",
    )?;
    let run_arg = run.to_string();
    let headless_start = [
        "--yes",
        "server",
        "headless",
        "start",
        id.as_str(),
        "--expected-run-generation",
        run_arg.as_str(),
    ];
    let (code, refused) = invoke_raw(&config, &headless_start)?;
    if code != 4 || !refused.to_string().contains("query port") {
        return Err(io::Error::other(format!(
            "Headless start before query readiness must be refused; exit {code}, {refused}"
        )));
    }
    let premature = elapsed(started_at);
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let current = status(&config, &id)?;
        if current["data"]["monitoring"]["query_ready"] == true {
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "Server never became query-ready: {}",
                current["data"]["monitoring"]
            )));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let launched = invoke(&config, &headless_start, "start headless clients")?;
    no_secret(&launched, "headless start")?;
    if number(
        &launched["data"]["outcome"]["status"]["headless_running"],
        "headless running",
    )? != 2
    {
        return Err(io::Error::other(format!(
            "Expected two running headless clients: {launched}"
        )));
    }
    let repeated = invoke(&config, &headless_start, "repeat headless start")?;
    if number(
        &repeated["data"]["outcome"]["status"]["headless_running"],
        "headless running",
    )? != 2
        || !repeated.to_string().contains("already running")
    {
        return Err(io::Error::other(format!(
            "A repeated start must not duplicate headless clients: {repeated}"
        )));
    }
    let consumers = invoke(
        &config,
        &["server", "installation", "consumers", &installation],
        "inspect consumers",
    )?;
    let mut registered: Vec<String> =
        consumers["data"]["consumer_inspection"]["recorded_consumers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|record| record["session"]["instance_id"].as_str().map(str::to_owned))
            .collect();
    registered.sort();
    let mut expected_consumers = vec![
        id.clone(),
        format!("{id}-{}", string(&first["id"], "HC1 id")?),
        format!("{id}-{}", string(&second["id"], "HC2 id")?),
    ];
    expected_consumers.sort();
    if registered != expected_consumers {
        return Err(io::Error::other(format!(
            "Unexpected consumer registrations while running: {registered:?}"
        )));
    }

    let hc_roots = [
        PathBuf::from(string(&first["state_root"], "HC1 root")?),
        PathBuf::from(string(&second["state_root"], "HC2 root")?),
    ];
    let evidence = wait_for_evidence(&profiles, &hc_roots, &identity, Duration::from_secs(240))?;
    for root in &hc_roots {
        let parameter = root.join("private/parameters.txt");
        if fs::read(&parameter)? != format!("-password={SECRET}\n").as_bytes() {
            return Err(io::Error::other("Parameter file content is not exact"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&parameter)?.permissions().mode() & 0o777 != 0o600 {
                return Err(io::Error::other("Parameter file is not owner-only"));
            }
        }
    }
    let connected = elapsed(started_at);

    let stopped = invoke(
        &config,
        &[
            "server",
            "instance",
            "stop",
            &id,
            "--expected-run-generation",
            &run_arg,
            "--timeout-ms",
            "60000",
            "--yes",
        ],
        "stop server",
    )?;
    let outcome = &stopped["data"]["outcome"];
    if outcome["completed"] != true
        || outcome["status"]["process_exists"] != false
        || outcome["status"]["cleanup_verified"] != true
        || outcome["status"]["forced"] != false
        || outcome["status"].get("headless_running").is_some()
    {
        return Err(io::Error::other(format!(
            "Graceful stop did not clean up every owned process: {stopped}"
        )));
    }
    let after = invoke(
        &config,
        &["server", "installation", "consumers", &installation],
        "inspect consumers after stop",
    )?;
    if after["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .is_none_or(|records| !records.is_empty())
    {
        return Err(io::Error::other(format!(
            "Consumer registrations remain after stop: {after}"
        )));
    }
    let runtime_state = profiles
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Server profiles have no runtime-state root"))?;
    for file in logs(&profiles)?
        .into_iter()
        .chain(logs(runtime_state)?)
        .chain(hc_roots.iter().flat_map(|root| {
            logs(&root.join("home/profiles"))
                .unwrap_or_default()
                .into_iter()
                .chain(logs(root).unwrap_or_default())
        }))
    {
        if String::from_utf8_lossy(&fs::read(&file)?).contains(SECRET) {
            return Err(io::Error::other(format!(
                "Join password leaked into {}",
                file.display()
            )));
        }
    }
    drop(owner);
    if inventory::tree_hash(&source)? != source_hash {
        return Err(io::Error::other("Native source changed during acceptance"));
    }
    println!(
        "Native headless acceptance passed for case {}: refusal before readiness at {premature:.1}s, both clients connected by {connected:.1}s; {}",
        case.display(),
        evidence
    );
    Ok(())
}

pub(super) fn elapsed(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

pub(super) fn no_secret(value: &Value, label: &str) -> io::Result<()> {
    if value.to_string().contains(SECRET) {
        return Err(io::Error::other(format!(
            "Join password leaked into {label}"
        )));
    }
    Ok(())
}

fn check_plan(plan: &Value, name: &str, mods: &str, overrides: &[&str]) -> io::Result<()> {
    let arguments: Vec<String> = plan["arguments"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let expected_name = format!("-name={name}");
    let required = [
        "-client",
        "-connect=127.0.0.1",
        &format!("-port={PORT}"),
        &expected_name,
        mods,
    ];
    let complete = required
        .iter()
        .chain(overrides)
        .all(|needle| arguments.iter().any(|argument| argument == needle))
        && arguments
            .iter()
            .any(|argument| argument.starts_with("-par="))
        && arguments
            .iter()
            .any(|argument| argument.starts_with("-profiles="));
    let dedicated = arguments.iter().any(|argument| {
        [
            "-config=",
            "-cfg=",
            "-serverMod",
            "-autoInit",
            "-keysFolder",
            "-password",
        ]
        .iter()
        .any(|prefix| argument.starts_with(prefix))
    });
    if plan["name"] != name || plan["join_password"] != true || !complete || dedicated {
        return Err(io::Error::other(format!(
            "Unexpected headless plan for {name}: {arguments:?}"
        )));
    }
    Ok(())
}

fn write_content(case: &Path, identity: &str) -> io::Result<()> {
    let alpha = case.join("content-alpha/@mod_01");
    fs::create_dir_all(alpha.join("addons"))?;
    fs::create_dir_all(alpha.join("MPMissions"))?;
    let beta = case.join("content-beta/@mod_02");
    fs::create_dir_all(beta.join("addons"))?;
    for (root, prefix, class) in [
        (&alpha, "foxy_hc_alpha", "FoxyNativeAlpha"),
        (&beta, "foxy_hc_beta", "FoxyNativeBeta"),
    ] {
        let config = format!(
            "class CfgPatches {{ class {class} {{ units[]={{}}; weapons[]={{}}; requiredVersion=0.1; requiredAddons[]={{}}; }}; }};\n"
        );
        super::super::mission::create_file(
            &root.join(format!("addons/{prefix}.pbo")),
            &super::super::mission::archive(&[("config.cpp", config.as_bytes())], Some(prefix)),
        )?;
    }
    let patches = "isClass (configFile >> \"CfgPatches\" >> \"FoxyNativeAlpha\"), isClass (configFile >> \"CfgPatches\" >> \"FoxyNativeBeta\")";
    let server = format!(
        "diag_log format [\"FOXY_HC|{identity}|server|patches|%1|%2\", {patches}];\naddMissionEventHandler [\"PlayerConnected\", {{ params [\"_id\", \"_uid\", \"_name\", \"_jip\", \"_owner\"]; diag_log format [\"FOXY_HC|{identity}|server|connected|%1|%2\", _name, _owner]; }}];\n[] spawn {{ waitUntil {{ sleep 1; ({{ isPlayer _x }} count (entities \"HeadlessClient_F\")) >= 2 }}; diag_log format [\"FOXY_HC|{identity}|server|slots|%1\", count ((entities \"HeadlessClient_F\") select {{ isPlayer _x }})]; }};\n"
    );
    let local = format!(
        "if (!hasInterface && !isServer) then {{ diag_log format [\"FOXY_HC|{identity}|local|%1|%2|%3\", profileName, {patches}]; }};\n"
    );
    let description = b"onLoadName = \"Foxy headless proof\";\nrespawn = 3;\nrespawnDelay = 1;\nclass Header { gameType = \"COOP\"; minPlayers = 1; maxPlayers = 3; };\n";
    let mission = b"version=12;\nclass Mission {\naddOns[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\",\"A3_Modules_F\"};\naddOnsAuto[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\",\"A3_Modules_F\"};\nrandomSeed=1;\nclass Intel { year=2035; month=6; day=24; hour=12; minute=0; };\nclass Groups { items=3;\nclass Item0 { side=\"WEST\"; class Vehicles { items=1; class Item0 { position[]={500,0,500}; id=0; side=\"WEST\"; vehicle=\"B_Soldier_F\"; player=\"PLAY CDG\"; leader=1; skill=0.5; }; }; };\nclass Item1 { side=\"LOGIC\"; class Vehicles { items=1; class Item0 { position[]={510,0,510}; id=1; side=\"LOGIC\"; vehicle=\"HeadlessClient_F\"; player=\"PLAY CDG\"; leader=1; skill=0.5; text=\"SlotA\"; }; }; };\nclass Item2 { side=\"LOGIC\"; class Vehicles { items=1; class Item0 { position[]={520,0,520}; id=2; side=\"LOGIC\"; vehicle=\"HeadlessClient_F\"; player=\"PLAY CDG\"; leader=1; skill=0.5; text=\"SlotB\"; }; }; };\n};\n};\n";
    super::super::mission::create_file(
        &alpha.join(format!("MPMissions/{TEMPLATE}.pbo")),
        &super::super::mission::archive(
            &[
                ("mission.sqm", mission),
                ("description.ext", description),
                ("initServer.sqf", server.as_bytes()),
                ("init.sqf", local.as_bytes()),
            ],
            None,
        ),
    )
}

fn register_content(config: &Path, case: &Path, id: &str) -> io::Result<()> {
    for (name, folder, addon) in [
        ("RepoAlpha", "content-alpha", "@mod_01"),
        ("RepoBeta", "content-beta", "@mod_02"),
    ] {
        invoke(
            config,
            &[
                "server",
                "content",
                "add-folder",
                "--id",
                name,
                "--path",
                path_arg(&case.join(folder))?,
            ],
            "register content folder",
        )?;
        invoke(
            config,
            &[
                "server",
                "content",
                "add-profile",
                "--name",
                name,
                "--folder",
                name,
                "--url",
                "",
                "--local-path",
                path_arg(&case.join(format!("profile-{folder}")))?,
                "--supply",
                "already-present",
                "--addon",
                addon,
            ],
            "register content profile",
        )?;
    }
    let bindings = case.join("bindings.json");
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"required","order":0},{"source_id":"RepoBeta","role":"server_only","order":1}]"#,
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
        "plan bindings",
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
            string(&plan["data"]["plan"]["fingerprint"], "binding fingerprint")?,
        ],
        "save bindings",
    )?;
    Ok(())
}

pub(super) fn logs(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !directory.is_dir() {
        return Ok(files);
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "rpt" || extension == "log")
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn text(files: &[PathBuf]) -> String {
    files
        .iter()
        .filter_map(|file| fs::read(file).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn wait_for_evidence(
    server_profiles: &Path,
    hc_roots: &[PathBuf; 2],
    identity: &str,
    timeout: Duration,
) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    // In a headless client, profileName reports the occupied slot label, so identity comes from
    // the client's own state root. Only the Windows report echoes the command line.
    let expectations = [
        ("\"-name=HC1\"", "|true|false"),
        ("\"-name=HC2\"", "|false|true"),
    ];
    let echoes_arguments = cfg!(windows);
    let local = format!("FOXY_HC|{identity}|local|");
    let loaded = |text: &str, patches: &str| {
        text.lines().any(|line| {
            line.contains(&local) && line.trim_end().trim_end_matches('"').ends_with(patches)
        })
    };
    let runtime_state = server_profiles
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Server profiles have no runtime-state root"))?;
    loop {
        let server = text(&[logs(server_profiles)?, logs(runtime_state)?].concat());
        let clients: Vec<String> = hc_roots
            .iter()
            .map(|root| {
                text(
                    &[
                        logs(&root.join("home/profiles")).unwrap_or_default(),
                        logs(root).unwrap_or_default(),
                    ]
                    .concat(),
                )
            })
            .collect();
        let server_ready = server.contains(&format!("FOXY_HC|{identity}|server|patches|true|true"))
            && server
                .matches(&format!(
                    "FOXY_HC|{identity}|server|connected|headlessclient"
                ))
                .count()
                >= 2
            && server.contains(&format!("FOXY_HC|{identity}|server|slots|2"));
        let clients_ready = expectations
            .iter()
            .zip(&clients)
            .all(|((name, patches), client)| {
                (!echoes_arguments || client.contains(name)) && loaded(client, patches)
            });
        if server_ready && clients_ready {
            for (index, client) in clients.iter().enumerate() {
                let (other_name, other_patches) = expectations[1 - index];
                if (echoes_arguments
                    && (client.contains(other_name) || !client.contains("\"-client\"")))
                    || loaded(client, other_patches)
                    || client.contains("-config=")
                    || client.contains("-serverMod")
                    || client.contains("-password")
                {
                    return Err(io::Error::other(
                        "Headless logs are not isolated or carry dedicated arguments",
                    ));
                }
            }
            if server.contains("\"-client\"") {
                return Err(io::Error::other(
                    "Server profile log contains a headless client run",
                ));
            }
            return Ok(format!(
                "server patches alpha+beta, 2 HC slots taken; HC1 alpha only; HC2 beta only; isolated logs {} and {}",
                hc_roots[0].display(),
                hc_roots[1].display()
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "Headless connection evidence missing: server_ready={server_ready}, clients_ready={clients_ready}"
            )));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
