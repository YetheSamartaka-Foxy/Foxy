use super::*;
use std::thread;
use std::time::Instant;

const PORT: u16 = 27132;

pub(super) fn run() -> io::Result<()> {
    let source = inventory::required_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = inventory::required_directory("FOXY_ARMA_MISSION_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and mission scratch must be separate",
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
            "Mission scratch path is too long for native Windows arguments",
        ));
    }
    require_port_range_free(PORT)?;
    let source_hash = inventory::tree_hash(&source)?;
    let adopted = case.join("adopted");
    inventory::copy_tree(&source, &adopted)?;
    let adopted_hash = inventory::tree_hash(&adopted)?;
    if adopted_hash != source_hash {
        return Err(io::Error::other("Adopted copy differs from native source"));
    }
    let config = case.join("config");
    fs::create_dir(&config)?;
    fs::write(
        config.join("games.json"),
        include_bytes!("../../examples/json/appdata/games.json"),
    )?;

    let nonce = format!("{:x}", nonce());
    let host = format!("FoxyLibrary {nonce}");
    let added = invoke(
        &config,
        &[
            "--yes",
            "server",
            "instance",
            "add",
            "--name",
            "MissionLibraryNative",
        ],
        "add instance",
    )?;
    let id = string(&added["data"]["instance"]["id"], "instance id")?.to_owned();
    let mut draft =
        invoke(&config, &["server", "instance", "show", &id], "show draft")?["data"]["instance"]
            .clone();
    draft["definition"]["settings"]["hostname"] = json!(host);
    draft["definition"]["settings"]["game_port"] = json!(PORT);
    draft["definition"]["settings"]["max_players"] = json!(1);
    draft["definition"]["settings"]["persistent"] = json!(true);
    draft["definition"]["settings"]["auto_init"] = json!(true);
    draft["definition"]["settings"]["battleye"] = json!(false);
    draft["definition"]["settings"]["verify_signatures"] = json!("v2");
    replace(&config, &case, &draft, 1, "mission-library-draft.json")?;
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

    let folders = case.join("mission-sources");
    fs::create_dir(&folders)?;
    let alpha = folders.join("alpha.vr");
    let beta = folders.join("beta.vr");
    write_mission(&alpha, &nonce, "alpha", 23)?;
    write_mission(&beta, &nonce, "beta", 17)?;
    let alpha_hash = inventory::tree_hash(&alpha)?;
    let beta_hash = inventory::tree_hash(&beta)?;
    let alpha_entry = import_mission(&config, &alpha)?;
    let beta_entry = import_mission(&config, &beta)?;
    let library = invoke(
        &config,
        &["server", "mission", "list"],
        "list mission library",
    )?;
    for entry in [&alpha_entry, &beta_entry] {
        let found = library["data"]["library"]["entries"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item["mission_id"] == entry["mission_id"]
                        && item["revision_id"] == entry["revision_id"]
                        && item["payload"]["payload_blake3"] == entry["payload"]["payload_blake3"]
                })
            });
        if !found {
            return Err(io::Error::other(
                "Imported immutable mission revision is absent from library listing",
            ));
        }
    }

    let mission_cycle = json!([
        {"enabled":true,"source":{"kind":"library","mission_id":beta_entry["mission_id"],"revision_id":beta_entry["revision_id"]},"template":"beta.vr","difficulty":"veteran","params":{"Probe":23}},
        {"enabled":false,"source":{"kind":"library","mission_id":"mission_00000000000000000000000000000000","revision_id":"revision_0000000000000000000000000000000000000000000000000000000000000000"},"template":"missing.vr","difficulty":"regular","params":{"Probe":99}},
        {"enabled":true,"source":{"kind":"library","mission_id":alpha_entry["mission_id"],"revision_id":alpha_entry["revision_id"]},"template":"alpha.vr","difficulty":"regular","params":{"Probe":17}}
    ]);
    let cycle_file = case.join("mission-cycle.json");
    fs::write(&cycle_file, serde_json::to_vec(&mission_cycle)?)?;
    invoke(
        &config,
        &[
            "--yes",
            "server",
            "mission",
            "cycle",
            &id,
            "--expected-revision",
            "3",
            "--file",
            path_arg(&cycle_file)?,
            "--apply",
        ],
        "bind mission revisions",
    )?;
    let mut instance = invoke(
        &config,
        &["server", "instance", "show", &id],
        "show bound instance",
    )?["data"]["instance"]
        .clone();
    let stored_cycle = instance["definition"]["settings"]["mission_cycle"]
        .as_array()
        .ok_or_else(|| io::Error::other("Stored mission cycle is missing"))?;
    if stored_cycle.len() != 3
        || stored_cycle[0]["template"] != "beta.vr"
        || stored_cycle[0]["difficulty"] != "veteran"
        || stored_cycle[0]["params"]["Probe"] != 23
        || stored_cycle[1]["enabled"] != false
        || stored_cycle[2]["template"] != "alpha.vr"
        || stored_cycle[2]["difficulty"] != "regular"
        || stored_cycle[2]["params"]["Probe"] != 17
    {
        return Err(io::Error::other(
            "Stored mission cycle did not preserve order, disabled entry, parameters and difficulty",
        ));
    }
    instance["definition"]["settings"]["server"]["auto_select_mission"] = json!(true);
    replace(
        &config,
        &case,
        &instance,
        4,
        "mission-library-auto-select.json",
    )?;
    commit_configuration(&config, &id, 5)?;
    setup_content(&config, &case, &id)?;
    let prepared = prepare(
        &config,
        &case,
        &id,
        "library",
        &format!("Library_{nonce}"),
        PreparationRevision {
            deployment: 0,
            draft: 5,
            configuration: 1,
        },
    )?;
    activate(&config, &id, &prepared.preparation, 1, 0)?;
    let plan = start_plan(&config, &id)?;
    let engine = PathBuf::from(string(
        &plan.launch["directory"],
        "runtime engine directory",
    )?);
    let runtime = engine
        .parent()
        .ok_or_else(|| io::Error::other("Runtime engine directory has no runtime root"))?
        .to_path_buf();
    for entry in [&beta_entry, &alpha_entry] {
        verify_runtime_mission(&engine, entry)?;
    }
    if engine
        .join("foxy-content/mission_00000000000000000000000000000000/mpmissions/missing.vr.pbo")
        .exists()
        || engine
            .join(native_mission_directory())
            .join("missing.vr.pbo")
            .exists()
    {
        return Err(io::Error::other(
            "Disabled missing library mission was staged",
        ));
    }
    let owner = transport::start_owner(&config)?;
    let state = runtime_state(&config, &id);
    let before = output_snapshot(&state)?;
    invoke(
        &config,
        &[
            "server",
            "instance",
            "start",
            &id,
            "--expected-activation-generation",
            &plan.generation,
            "--expected-plan-fingerprint",
            &plan.fingerprint,
            "--yes",
        ],
        "start native mission cycle",
    )?;
    let status = invoke(
        &config,
        &["server", "instance", "status", &id],
        "status native mission cycle",
    )?;
    let run = number(
        &status["data"]["status"]["run_generation"],
        "run generation",
    )?;
    wait_cycle(&state, &before, &nonce, Duration::from_secs(240))?;
    let stop = invoke(
        &config,
        &[
            "server",
            "instance",
            "stop",
            &id,
            "--expected-run-generation",
            &run.to_string(),
            "--timeout-ms",
            "120000",
            "--yes",
        ],
        "stop native mission cycle",
    )?;
    if stop["data"]["outcome"]["status"]["forced"] != false {
        return Err(io::Error::other(format!(
            "Graceful stop was forced: {stop}"
        )));
    }

    let old_runtime_hash = inventory::tree_hash(&runtime)?;
    let alpha_id = string(&alpha_entry["mission_id"], "Alpha id")?;
    if inventory::tree_hash(&alpha)? != alpha_hash {
        return Err(io::Error::other(
            "Alpha import source changed before refresh",
        ));
    }
    fs::write(alpha.join("description.ext"), b"onLoadName=\"Foxy refreshed library mission\";\nrespawn=3;\nclass Header { gameType=\"COOP\"; minPlayers=1; maxPlayers=1; };\nclass Params { class Probe { title=\"Probe\"; values[]={17,23}; texts[]={\"17\",\"23\"}; default=23; }; };\n")?;
    let refreshed_source_hash = inventory::tree_hash(&alpha)?;
    let refresh = invoke(
        &config,
        &["--yes", "server", "mission", "refresh", alpha_id, "--apply"],
        "refresh Alpha library revision",
    )?;
    let new_alpha = refresh["data"]["entry"].clone();
    if new_alpha["mission_id"] != alpha_entry["mission_id"]
        || new_alpha["revision_id"] == alpha_entry["revision_id"]
    {
        return Err(io::Error::other(
            "Refreshing local mission did not create a new immutable revision",
        ));
    }
    let after_list = invoke(
        &config,
        &["server", "mission", "list"],
        "list after refresh",
    )?;
    if after_list["data"]["library"]["entries"]
        .as_array()
        .is_none_or(|entries| {
            !entries.iter().any(|entry| {
                entry["mission_id"] == alpha_entry["mission_id"]
                    && entry["revision_id"] == new_alpha["revision_id"]
            })
        })
    {
        return Err(io::Error::other(
            "Mission refresh did not advance the library current revision",
        ));
    }
    if inventory::tree_hash(&runtime)? != old_runtime_hash {
        return Err(io::Error::other(
            "Library refresh mutated the committed runtime",
        ));
    }
    let before_rebind = invoke(
        &config,
        &["server", "instance", "show", &id],
        "show configuration after library refresh",
    )?;
    if before_rebind["data"]["instance"]["definition"]["settings"]["mission_cycle"][2]["source"]["revision_id"]
        != alpha_entry["revision_id"]
    {
        return Err(io::Error::other(
            "Refreshing the library changed the committed cycle's immutable revision pin",
        ));
    }
    let mut rebound_cycle = mission_cycle.clone();
    rebound_cycle[2]["source"]["revision_id"] = new_alpha["revision_id"].clone();
    let rebound_cycle_file = case.join("mission-cycle-rebound.json");
    fs::write(&rebound_cycle_file, serde_json::to_vec(&rebound_cycle)?)?;
    invoke(
        &config,
        &[
            "--yes",
            "server",
            "mission",
            "cycle",
            &id,
            "--expected-revision",
            "5",
            "--file",
            path_arg(&rebound_cycle_file)?,
            "--apply",
        ],
        "rebind refreshed Alpha revision",
    )?;
    let rebound = invoke(
        &config,
        &["server", "instance", "show", &id],
        "show rebound instance",
    )?["data"]["instance"]
        .clone();
    replace(&config, &case, &rebound, 6, "mission-library-rebound.json")?;
    commit_configuration(&config, &id, 7)?;
    let rebound_prepared = prepare(
        &config,
        &case,
        &id,
        "libraryrebound",
        &format!("LibraryRebound_{nonce}"),
        PreparationRevision {
            deployment: 1,
            draft: 7,
            configuration: 2,
        },
    )?;
    activate(&config, &id, &rebound_prepared.preparation, 2, 1)?;
    let rebound_plan = start_plan(&config, &id)?;
    if rebound_plan.runtime == plan.runtime || rebound_plan.fingerprint == plan.fingerprint {
        return Err(io::Error::other(
            "Explicit mission rebind did not create a new runtime generation",
        ));
    }
    let rebound_engine = PathBuf::from(string(
        &rebound_plan.launch["directory"],
        "rebound engine directory",
    )?);
    verify_runtime_mission(&rebound_engine, &new_alpha)?;
    let rebound_before = output_snapshot(&state)?;
    invoke(
        &config,
        &[
            "server",
            "instance",
            "start",
            &id,
            "--expected-activation-generation",
            &rebound_plan.generation,
            "--expected-plan-fingerprint",
            &rebound_plan.fingerprint,
            "--yes",
        ],
        "start rebound mission cycle",
    )?;
    let rebound_status = invoke(
        &config,
        &["server", "instance", "status", &id],
        "status rebound mission cycle",
    )?;
    let rebound_run = number(
        &rebound_status["data"]["status"]["run_generation"],
        "rebound run generation",
    )?;
    wait_cycle(&state, &rebound_before, &nonce, Duration::from_secs(240))?;
    let rebound_stop = invoke(
        &config,
        &[
            "server",
            "instance",
            "stop",
            &id,
            "--expected-run-generation",
            &rebound_run.to_string(),
            "--timeout-ms",
            "120000",
            "--yes",
        ],
        "stop rebound mission cycle",
    )?;
    if rebound_stop["data"]["outcome"]["status"]["forced"] != false {
        return Err(io::Error::other(format!(
            "Rebound graceful stop was forced: {rebound_stop}"
        )));
    }
    if inventory::tree_hash(&runtime)? != old_runtime_hash
        || inventory::tree_hash(&source)? != source_hash
        || inventory::tree_hash(&adopted)? != adopted_hash
        || inventory::tree_hash(&alpha)? != refreshed_source_hash
        || inventory::tree_hash(&beta)? != beta_hash
    {
        return Err(io::Error::other(
            "Native source, adopted copy or imported mission folders changed",
        ));
    }
    drop(owner);
    eprintln!(
        "Native mission library evidence: exact immutable and refreshed PBOs staged; Beta then Alpha observed in both generations; disabled missing entry absent; case {}",
        case.display()
    );
    Ok(())
}

fn import_mission(config: &Path, source: &Path) -> io::Result<Value> {
    let value = invoke(
        config,
        &[
            "--yes",
            "server",
            "mission",
            "import",
            "--source",
            path_arg(source)?,
            "--apply",
        ],
        "import local mission",
    )?;
    let entry = value["data"]["entry"].clone();
    if string(&entry["payload"]["template"], "mission template")?
        != source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
        || entry["payload"]["source_kind"] != "folder"
    {
        return Err(io::Error::other(
            "Mission import did not preserve the unpacked template identity",
        ));
    }
    Ok(entry)
}

fn verify_runtime_mission(engine: &Path, entry: &Value) -> io::Result<()> {
    let mission_id = string(&entry["mission_id"], "mission id")?;
    let template = string(&entry["payload"]["template"], "mission template")?;
    let expected_bytes = number(&entry["payload"]["payload_bytes"], "payload bytes")?;
    let expected_hash = string(&entry["payload"]["payload_blake3"], "payload hash")?;
    let content_copy = engine
        .join("foxy-content")
        .join(mission_id)
        .join("mpmissions")
        .join(format!("{template}.pbo"));
    let selected_mission = engine
        .join(native_mission_directory())
        .join(format!("{template}.pbo"));
    for path in [&content_copy, &selected_mission] {
        let bytes = read_ordinary(path)?;
        if bytes.len() as u64 != expected_bytes
            || blake3::hash(&bytes).to_hex().as_str() != expected_hash
        {
            return Err(io::Error::other(format!(
                "Native mission copy differs from its immutable library revision: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn native_mission_directory() -> &'static str {
    if cfg!(windows) {
        "MPMissions"
    } else {
        "mpmissions"
    }
}

fn write_mission(directory: &Path, nonce: &str, marker: &str, parameter: u8) -> io::Result<()> {
    fs::create_dir(directory)?;
    fs::write(directory.join("mission.sqm"), b"version=12;\nclass Mission {\naddOns[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\naddOnsAuto[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\nrandomSeed=1;\nclass Intel { year=2035; month=6; day=24; hour=12; minute=0; };\nclass Groups { items=1; class Item0 { side=\"WEST\"; class Vehicles { items=1; class Item0 { position[]={500,0,500}; id=0; side=\"WEST\"; vehicle=\"B_Soldier_F\"; player=\"PLAYER COMMANDER\"; leader=1; skill=0.5; }; }; }; };\n};\n")?;
    fs::write(
        directory.join("description.ext"),
        format!(
            "onLoadName=\"Foxy mission library\";\nrespawn=3;\nclass Header {{ gameType=\"COOP\"; minPlayers=1; maxPlayers=1; }};\nclass Params {{ class Probe {{ title=\"Probe\"; values[]={{17,23}}; texts[]={{\"17\",\"23\"}}; default={parameter}; }}; }};\n"
        ),
    )?;
    fs::write(
        directory.join("initServer.sqf"),
        format!(
            "diag_log format [\"FOXY_MISSION_LIBRARY|{nonce}|{marker}|%1|%2|end\", toLower worldName, paramsArray select 0];\n[] spawn {{ sleep 8; endMission \"END1\"; }};\n"
        ),
    )?;
    Ok(())
}

fn wait_cycle(
    state: &Path,
    before: &std::collections::HashMap<PathBuf, u64>,
    nonce: &str,
    timeout: Duration,
) -> io::Result<()> {
    let marker = format!("FOXY_MISSION_LIBRARY|{nonce}|");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for path in report_paths(state)? {
            if fs::metadata(&path)?.len() > 16 * 1024 * 1024 {
                return Err(io::Error::other("Engine report exceeded size bound"));
            }
            let bytes = fs::read(&path)?;
            let start = before
                .get(&path)
                .copied()
                .filter(|size| *size <= bytes.len() as u64)
                .unwrap_or(0) as usize;
            let text = String::from_utf8_lossy(&bytes[start..]);
            let records = text
                .lines()
                .filter_map(|line| {
                    line.split_once(&marker)
                        .map(|(_, record)| record.trim().trim_end_matches('"'))
                })
                .filter(|record| record.ends_with("|end"))
                .collect::<Vec<_>>();
            if records.len() >= 2 {
                let expected_beta = "beta|vr|23|end";
                let expected_alpha = "alpha|vr|17|end";
                let beta = records.iter().position(|record| record == &expected_beta);
                let alpha = records.iter().position(|record| record == &expected_alpha);
                if beta.is_some_and(|index| alpha.is_some_and(|later| index < later)) {
                    return Ok(());
                }
                return Err(io::Error::other(format!(
                    "Native mission cycle observations did not prove Beta then Alpha: {records:?}"
                )));
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(io::Error::other(
        "Fresh native output did not observe Beta then Alpha mission-cycle advancement",
    ))
}

fn report_paths(state: &Path) -> io::Result<Vec<PathBuf>> {
    let mut stack = vec![(state.to_path_buf(), 0usize)];
    let mut files = Vec::new();
    let mut count = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if depth > 16 {
            return Err(io::Error::other("Runtime output depth exceeded bound"));
        }
        for entry in fs::read_dir(directory)? {
            count += 1;
            if count > 8192 {
                return Err(io::Error::other("Runtime output inventory exceeded bound"));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Runtime output contains a link"));
            }
            if metadata.is_dir() {
                stack.push((entry.path(), depth + 1));
            } else if metadata.is_file()
                && entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("rpt") || extension.eq_ignore_ascii_case("log")
                })
            {
                files.push(entry.path());
            }
        }
    }
    Ok(files)
}

fn output_snapshot(state: &Path) -> io::Result<std::collections::HashMap<PathBuf, u64>> {
    report_paths(state)?
        .into_iter()
        .map(|path| Ok((path.clone(), fs::metadata(path)?.len())))
        .collect()
}
