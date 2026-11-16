use super::*;
use std::thread;
use std::time::Instant;

const KEY_PORT: u16 = 27152;

pub(super) fn run() -> io::Result<()> {
    run_with(false)
}

pub(super) fn run_reconciliation() -> io::Result<()> {
    run_with(true)
}

fn run_with(reconcile: bool) -> io::Result<()> {
    let source = inventory::required_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = inventory::required_directory("FOXY_ARMA_KEY_SCRATCH")?;
    let fixtures = inventory::required_directory("FOXY_ARMA_KEY_FIXTURES")?;
    if source.starts_with(&scratch)
        || scratch.starts_with(&source)
        || fixtures.starts_with(&scratch)
        || scratch.starts_with(&fixtures)
    {
        return Err(io::Error::other(
            "Native source, fixtures and scratch must be separate",
        ));
    }
    let alpha_key = fixtures.join("FoxyNativeAlpha.bikey");
    let beta_key = fixtures.join("FoxyNativeBeta.bikey");
    let signed_pbo = fixtures.join("client-content/addons/foxy_native_alpha.pbo");
    let signature =
        fixtures.join("client-content/addons/foxy_native_alpha.pbo.FoxyNativeAlpha.bisign");
    let alpha_key_hash = hash_ordinary(&alpha_key)?;
    let beta_key_hash = hash_ordinary(&beta_key)?;
    let pbo_hash = hash_ordinary(&signed_pbo)?;
    let signature_hash = hash_ordinary(&signature)?;

    let case = inventory::fresh_child(&scratch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&case, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    if case.to_string_lossy().len() > 60 {
        return Err(io::Error::other(
            "Key scratch path is too long for native Windows arguments",
        ));
    }
    require_port_range_free(KEY_PORT)?;
    let source_hash = inventory::tree_hash(&source)?;
    let adopted = case.join("adopted");
    inventory::copy_tree(&source, &adopted)?;
    let adopted_hash = inventory::tree_hash(&adopted)?;
    let base_key_hash = hash_ordinary(&adopted.join("keys/a3.bikey"))?;
    if source_hash != adopted_hash {
        return Err(io::Error::other("Adopted native copy differs from source"));
    }
    let config = case.join("config");
    fs::create_dir(&config)?;
    fs::write(
        config.join("games.json"),
        include_bytes!("../../examples/json/appdata/games.json"),
    )?;
    let nonce = format!("{:x}", nonce());
    let mission_source = case.join("FoxyKey.vr");
    write_key_mission(&mission_source, &nonce)?;
    let imported_mission = invoke(
        &config,
        &[
            "--yes",
            "server",
            "mission",
            "import",
            "--source",
            path_arg(&mission_source)?,
            "--apply",
        ],
        "import key-trust library mission",
    )?;
    let mission_entry = imported_mission["data"]["entry"].clone();
    let id = string(
        &invoke(
            &config,
            &[
                "--yes",
                "server",
                "instance",
                "add",
                "--name",
                "KeyTrustNative",
            ],
            "add instance",
        )?["data"]["instance"]["id"],
        "instance id",
    )?
    .to_owned();
    let mut instance = invoke(&config, &["server", "instance", "show", &id], "show draft")?["data"]
        ["instance"]
        .clone();
    let mut settings = generation_a(&format!("FoxyKey {nonce}"));
    settings["game_port"] = json!(KEY_PORT);
    settings["max_players"] = json!(1);
    settings["persistent"] = json!(true);
    settings["auto_init"] = json!(true);
    settings["battleye"] = json!(false);
    settings["verify_signatures"] = json!("v2");
    settings["mission_cycle"] = json!([{
        "enabled": true,
        "source": {"kind":"library", "mission_id":mission_entry["mission_id"], "revision_id":mission_entry["revision_id"]},
        "template": "FoxyKey.vr",
        "difficulty": "regular",
        "params": {}
    }]);
    instance["definition"]["settings"] = settings;
    replace(&config, &case, &instance, 1, "key-trust-initial.json")?;
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

    let profile_root = case.join("profiles");
    fs::create_dir(&profile_root)?;
    let required = add_profile(
        &config,
        &case,
        &profile_root,
        "Required",
        "required",
        "required",
        "@mod_01",
    )?;
    let optional = add_profile(
        &config,
        &case,
        &profile_root,
        "Optional",
        "optional",
        "optional",
        "@mod_02",
    )?;
    let server_only = add_profile(
        &config,
        &case,
        &profile_root,
        "ServerOnly",
        "serveronly",
        "serveronly",
        "@mod_03",
    )?;
    for addon in [&required, &optional, &server_only] {
        fs::create_dir_all(addon.join("addons"))?;
        fs::create_dir_all(addon.join("keys"))?;
    }
    fs::copy(&alpha_key, required.join("keys/FoxyNativeAlpha.bikey"))?;
    fs::copy(&beta_key, optional.join("keys/FoxyNativeBeta.bikey"))?;
    fs::copy(&signed_pbo, required.join("addons/foxy_native_alpha.pbo"))?;
    fs::copy(
        &signature,
        required.join("addons/foxy_native_alpha.pbo.FoxyNativeAlpha.bisign"),
    )?;
    let optional_missions = case.join("optional-missions");
    fs::create_dir(&optional_missions)?;
    let optional_content = case.join("optional-content");
    super::super::mission::write(
        &optional_missions,
        &optional_content,
        "optional",
        "OptionalKey",
    )?;
    fs::copy(
        optional_content.join("addons/foxy_configuration_optional.pbo"),
        optional.join("addons/foxy_configuration_optional.pbo"),
    )?;
    let server_missions = case.join("server-missions");
    fs::create_dir(&server_missions)?;
    let server_content = case.join("server-content");
    super::super::mission::write(
        &server_missions,
        &server_content,
        "serveronly",
        "ServerOnlyKey",
    )?;
    fs::copy(
        server_content.join("addons/foxy_configuration_serveronly.pbo"),
        server_only.join("addons/foxy_configuration_serveronly.pbo"),
    )?;
    save_profiles(
        &config,
        &case,
        &id,
        &["Required", "Optional", "ServerOnly"],
        false,
    )?;
    let stage_plan = invoke(
        &config,
        &["server", "content", "stage", "plan", &id],
        "plan key stage",
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
        "apply key stage",
    )?;
    let review = invoke(
        &config,
        &["server", "keys", "audit", &id, "--stage-id", &stage_id],
        "review staged keys",
    )?;
    let candidates = review["data"]["review"]["candidates"]
        .as_array()
        .ok_or_else(|| io::Error::other("Key review candidates are missing"))?;
    let alpha = candidate(candidates, "FoxyNativeAlpha.bikey")?;
    let beta = candidate(candidates, "FoxyNativeBeta.bikey")?;
    if alpha["trusted"] != false || beta["trusted"] != false {
        return Err(io::Error::other(
            "New public keys were trusted without an explicit operator selection",
        ));
    }
    let alpha_id = string(&alpha["id"], "Alpha candidate id")?.to_owned();
    let review_fingerprint = string(
        &review["data"]["review"]["fingerprint"],
        "key review fingerprint",
    )?
    .to_owned();
    invoke(
        &config,
        &[
            "--yes",
            "server",
            "keys",
            "audit",
            &id,
            "--stage-id",
            &stage_id,
            "--trust",
            &alpha_id,
            "--fingerprint",
            &review_fingerprint,
            "--apply",
        ],
        "save explicit Alpha key trust",
    )?;
    let current = invoke(
        &config,
        &["server", "instance", "show", &id],
        "show trusted draft",
    )?["data"]["instance"]
        .clone();
    let settings = &current["definition"]["settings"];
    if settings["game_port"] != KEY_PORT
        || settings["persistent"] != true
        || settings["auto_init"] != true
        || settings["battleye"] != false
        || settings["verify_signatures"] != "v2"
        || settings["mission_cycle"][0]["source"]["mission_id"] != mission_entry["mission_id"]
        || settings["mission_cycle"][0]["source"]["revision_id"] != mission_entry["revision_id"]
    {
        return Err(io::Error::other(
            "Saved server definition does not retain the reviewed mission pin and native options",
        ));
    }
    let trusted = settings["key_trust"]["keys"]
        .as_array()
        .ok_or_else(|| io::Error::other("Saved key trust policy is missing"))?;
    let trusted_names = trusted
        .iter()
        .filter_map(|key| key["name"].as_str())
        .collect::<Vec<_>>();
    if !trusted_names.contains(&"FoxyNativeAlpha.bikey")
        || trusted_names.contains(&"FoxyNativeBeta.bikey")
        || trusted_names
            .iter()
            .any(|name| !matches!(*name, "FoxyNativeAlpha.bikey" | "a3.bikey"))
    {
        return Err(io::Error::other(
            "Saved policy did not contain only Alpha and the implicit base key",
        ));
    }
    let revision = number(&current["revision"], "trusted draft revision")?;
    save_profiles(
        &config,
        &case,
        &id,
        &["Required", "Optional", "ServerOnly"],
        false,
    )?;
    let stage_id = stage_again(&config, &id)?;
    let preview = commit_configuration(&config, &id, revision)?;
    require_preview_files(&preview, "Managed.Arma3Profile")?;
    let prepared_a = prepare_from_stage(
        &config,
        &id,
        &stage_id,
        0,
        revision,
        1,
        format!("KeyTrustA_{nonce}"),
    )?;
    activate(&config, &id, &prepared_a.preparation, 1, 0)?;
    let plan_a = start_plan(&config, &id)?;
    let engine_a = PathBuf::from(string(&plan_a.launch["directory"], "runtime directory")?);
    let state = runtime_state(&config, &id);
    let owner = transport::start_owner(&config)?;
    run_generation(&config, &id, &state, &plan_a, &nonce, true)?;

    let args_a = arguments(&plan_a.launch)?;
    let initial = fs::read(configuration_path(&args_a)?)?;
    require_role_args(&args_a, "-mod=", &["@mod_01"])?;
    require_role_args(&args_a, "-serverMod=", &["@mod_03"])?;
    verify_keys(&engine_a, true, &alpha_key_hash, &base_key_hash)?;
    if contains_file_name(&engine_a, "foxy_configuration_optional.pbo")? {
        return Err(io::Error::other(
            "Optional source PBO was copied into the runtime",
        ));
    }
    let predecessor_hash = inventory::tree_hash(&engine_a)?;
    if !String::from_utf8_lossy(&initial).contains("verifySignatures = 2") {
        return Err(io::Error::other(
            "Committed server configuration did not retain signature verification v2",
        ));
    }

    save_profiles(
        &config,
        &case,
        &id,
        &["Required", "Optional", "ServerOnly"],
        true,
    )?;
    let stage_b = stage_again(&config, &id)?;
    let review_b = invoke(
        &config,
        &["server", "keys", "audit", &id, "--stage-id", &stage_b],
        "review key trust removal",
    )?;
    let review_b_fingerprint = string(
        &review_b["data"]["review"]["fingerprint"],
        "second key review fingerprint",
    )?
    .to_owned();
    invoke(
        &config,
        &[
            "--yes",
            "server",
            "keys",
            "audit",
            &id,
            "--stage-id",
            &stage_b,
            "--fingerprint",
            &review_b_fingerprint,
            "--apply",
        ],
        "save base-only key trust policy",
    )?;
    let updated = invoke(
        &config,
        &["server", "instance", "show", &id],
        "show optional-role draft",
    )?["data"]["instance"]
        .clone();
    let rev = number(&updated["revision"], "updated draft revision")?;
    save_profiles(
        &config,
        &case,
        &id,
        &["Required", "Optional", "ServerOnly"],
        true,
    )?;
    let stage_b = stage_again(&config, &id)?;
    let updated_keys = updated["definition"]["settings"]["key_trust"]["keys"]
        .as_array()
        .ok_or_else(|| io::Error::other("Updated trust policy is missing"))?;
    if updated_keys.len() != 1 || updated_keys[0]["name"] != "a3.bikey" {
        return Err(io::Error::other(
            "Key trust removal did not preserve only the implicit base key",
        ));
    }
    let preview_b = commit_configuration(&config, &id, rev)?;
    let prepared_b = prepare_from_stage(
        &config,
        &id,
        &stage_b,
        1,
        rev,
        2,
        format!("KeyTrustB_{nonce}"),
    )?;
    activate(&config, &id, &prepared_b.preparation, 2, 1)?;
    let plan_b = start_plan(&config, &id)?;
    let engine_b = PathBuf::from(string(
        &plan_b.launch["directory"],
        "second runtime directory",
    )?);
    run_generation(&config, &id, &state, &plan_b, &nonce, false)?;
    require_role_absent(
        &arguments(&plan_b.launch)?,
        "-mod=",
        &["@mod_01", "@mod_02"],
    )?;
    require_role_args(&arguments(&plan_b.launch)?, "-serverMod=", &["@mod_03"])?;
    verify_keys(&engine_b, false, &alpha_key_hash, &base_key_hash)?;
    if contains_file_name(&engine_b, "foxy_configuration_optional.pbo")? {
        return Err(io::Error::other(
            "Optional source PBO was copied into the second runtime",
        ));
    }

    let selection = invoke(
        &config,
        &[
            "server",
            "deploy",
            "select-plan",
            &id,
            &prepared_a.record,
            "--expected-deployment-generation",
            "2",
        ],
        "review predecessor record selection",
    )?;
    invoke(
        &config,
        &[
            "--yes",
            "server",
            "deploy",
            "select",
            &id,
            &prepared_a.record,
            "--expected-deployment-generation",
            "2",
            "--expected-plan-fingerprint",
            string(
                &selection["data"]["plan"]["fingerprint"],
                "record selection fingerprint",
            )?,
        ],
        "select predecessor deployment record",
    )?;
    let rollback = invoke(
        &config,
        &[
            "server",
            "deploy",
            "activation",
            "plan",
            &id,
            &prepared_a.preparation,
            "--expected-deployment-generation",
            "3",
            "--expected-activation-generation",
            "2",
        ],
        "plan prior activation rollback",
    )?;
    invoke(
        &config,
        &[
            "--yes",
            "server",
            "deploy",
            "activation",
            "apply",
            &id,
            &prepared_a.preparation,
            "--expected-deployment-generation",
            "3",
            "--expected-activation-generation",
            "2",
            "--expected-plan-fingerprint",
            string(
                &rollback["data"]["plan"]["fingerprint"],
                "rollback fingerprint",
            )?,
        ],
        "activate prior preparation",
    )?;
    let plan_rollback = start_plan(&config, &id)?;
    let engine_rollback = PathBuf::from(string(
        &plan_rollback.launch["directory"],
        "rollback runtime directory",
    )?);
    run_generation(&config, &id, &state, &plan_rollback, &nonce, true)?;
    require_role_args(&arguments(&plan_rollback.launch)?, "-mod=", &["@mod_01"])?;
    require_role_args(
        &arguments(&plan_rollback.launch)?,
        "-serverMod=",
        &["@mod_03"],
    )?;
    verify_keys(&engine_rollback, true, &alpha_key_hash, &base_key_hash)?;
    if contains_file_name(&engine_rollback, "foxy_configuration_optional.pbo")? {
        return Err(io::Error::other(
            "Optional source PBO was copied into the rolled-back runtime",
        ));
    }
    let _ = preview_b;
    let owner = if reconcile {
        reconcile_shared_content(
            &config,
            &case,
            &id,
            &state,
            &plan_rollback,
            &engine_b,
            &prepared_b.record,
            &nonce,
            owner,
        )?
    } else {
        owner
    };
    if inventory::tree_hash(&source)? != source_hash
        || inventory::tree_hash(&adopted)? != adopted_hash
        || inventory::tree_hash(&engine_a)? != predecessor_hash
        || hash_ordinary(&alpha_key)? != alpha_key_hash
        || hash_ordinary(&beta_key)? != beta_key_hash
        || hash_ordinary(&signed_pbo)? != pbo_hash
        || hash_ordinary(&signature)? != signature_hash
    {
        return Err(io::Error::other(
            "Source, adopted installation or selected public fixtures changed",
        ));
    }
    drop(owner);
    eprintln!(
        "Native key-trust evidence retained under private case {}",
        case.file_name().unwrap_or_default().to_string_lossy()
    );
    Ok(())
}

fn add_profile(
    config: &Path,
    case: &Path,
    root: &Path,
    name: &str,
    id: &str,
    folder: &str,
    addon: &str,
) -> io::Result<PathBuf> {
    let folder_path = root.join(folder);
    let local = case.join(format!("local-{id}"));
    fs::create_dir_all(folder_path.join(addon))?;
    fs::create_dir_all(folder_path.join("keys"))?;
    fs::create_dir_all(local.join(addon))?;
    fs::create_dir_all(local.join("keys"))?;
    invoke(
        config,
        &[
            "server",
            "content",
            "add-folder",
            "--id",
            folder,
            "--path",
            path_arg(&folder_path)?,
        ],
        "register content folder",
    )?;
    add_existing_profile(config, name, folder, &local, addon)?;
    Ok(folder_path.join(addon))
}

fn add_existing_profile(
    config: &Path,
    name: &str,
    folder: &str,
    local: &Path,
    addon: &str,
) -> io::Result<()> {
    invoke(
        config,
        &[
            "server",
            "content",
            "add-profile",
            "--name",
            name,
            "--folder",
            folder,
            "--url",
            "",
            "--local-path",
            path_arg(local)?,
            "--supply",
            "already-present",
            "--addon",
            addon,
        ],
        "register content profile",
    )?;
    Ok(())
}

fn save_profiles(
    config: &Path,
    case: &Path,
    id: &str,
    names: &[&str],
    required_is_optional: bool,
) -> io::Result<()> {
    let bindings = case.join("key-bindings.json");
    let values = names.iter().enumerate().map(|(order, name)| json!({"source_id":name,"role":match *name {"Required" if required_is_optional=>"optional","Required"=>"required","Optional"=>"optional","ServerOnly"=>"server_only",_=>"required"},"order":order})).collect::<Vec<_>>();
    fs::write(&bindings, serde_json::to_vec(&values)?)?;
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
        "plan content bindings",
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
        "save content bindings",
    )?;
    Ok(())
}

fn stage_again(config: &Path, id: &str) -> io::Result<String> {
    let plan = invoke(
        config,
        &["server", "content", "stage", "plan", id],
        "plan replacement stage",
    )?;
    let stage = string(&plan["data"]["plan"]["stage_id"], "stage id")?.to_owned();
    invoke(
        config,
        &[
            "server",
            "content",
            "stage",
            "apply",
            id,
            "--expected-plan-fingerprint",
            string(&plan["data"]["plan"]["fingerprint"], "stage fingerprint")?,
        ],
        "apply replacement stage",
    )?;
    Ok(stage)
}

fn prepare_from_stage(
    config: &Path,
    id: &str,
    stage: &str,
    deployment: u64,
    draft: u64,
    configuration: u64,
    identity: String,
) -> io::Result<preparation::Prepared> {
    preparation::prepare_stage(
        config,
        id,
        stage,
        PreparationRevision {
            deployment,
            draft,
            configuration,
        },
        identity,
    )
}

fn candidate<'a>(candidates: &'a [Value], name: &str) -> io::Result<&'a Value> {
    candidates
        .iter()
        .find(|candidate| candidate["key"]["name"] == name)
        .ok_or_else(|| io::Error::other(format!("Key review did not contain {name}")))
}

fn arguments(launch: &Value) -> io::Result<Vec<String>> {
    launch["arguments"]
        .as_array()
        .ok_or_else(|| io::Error::other("Launch arguments are missing"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| io::Error::other("Launch argument is not text"))
        })
        .collect()
}

fn configuration_path(args: &[String]) -> io::Result<PathBuf> {
    let path = args
        .iter()
        .find_map(|argument| argument.strip_prefix("-config="))
        .ok_or_else(|| io::Error::other("Start plan lacks its configuration argument"))?;
    let path = PathBuf::from(path);
    if path.file_name().and_then(|name| name.to_str()) != Some(SERVER_CONFIG) || !path.is_file() {
        return Err(io::Error::other(
            "Start plan configuration argument does not resolve to the committed server config",
        ));
    }
    Ok(path)
}

fn require_role_args(args: &[String], flag: &str, expected: &[&str]) -> io::Result<()> {
    let value = args
        .iter()
        .find_map(|arg| arg.strip_prefix(flag))
        .ok_or_else(|| io::Error::other(format!("Missing {flag} arguments")))?;
    let actual = value
        .split(';')
        .filter(|value| !value.is_empty())
        .filter_map(|value| Path::new(value).file_name().and_then(|name| name.to_str()))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(io::Error::other(format!(
            "{flag} role vector differs: expected {expected:?}, got {actual:?}"
        )));
    }
    Ok(())
}

fn require_role_absent(args: &[String], flag: &str, forbidden: &[&str]) -> io::Result<()> {
    let Some(value) = args.iter().find_map(|arg| arg.strip_prefix(flag)) else {
        return Ok(());
    };
    for item in value.split(';').filter(|item| !item.is_empty()) {
        let name = Path::new(item)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if forbidden.contains(&name) {
            return Err(io::Error::other(format!(
                "{flag} unexpectedly contains {name}"
            )));
        }
    }
    Ok(())
}

fn verify_keys(engine: &Path, alpha: bool, alpha_hash: &str, base_hash: &str) -> io::Result<()> {
    let private = engine.join("foxy-keys");
    let alpha_path = private.join("FoxyNativeAlpha.bikey");
    if alpha_path.exists() != alpha {
        return Err(io::Error::other(
            "Private runtime key directory does not match the reviewed trust policy",
        ));
    }
    let mut private_names = if private.exists() {
        fs::read_dir(&private)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    private_names.sort();
    let expected_private = if alpha {
        vec!["FoxyNativeAlpha.bikey".to_owned()]
    } else {
        Vec::new()
    };
    if private_names != expected_private {
        return Err(io::Error::other(format!(
            "Private runtime key directory contains an unexpected key set: {private_names:?}"
        )));
    }
    let base_path = engine.join("keys/a3.bikey");
    if hash_ordinary(&base_path)? != base_hash {
        return Err(io::Error::other(
            "Copied baseline key is missing or differs from the adopted installation",
        ));
    }
    if alpha && hash_ordinary(&alpha_path)? != alpha_hash {
        return Err(io::Error::other(
            "Runtime Alpha public key differs from the reviewed fixture bytes",
        ));
    }
    Ok(())
}

fn contains_file_name(root: &Path, name: &str) -> io::Result<bool> {
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut count = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 16 {
            return Err(io::Error::other("Runtime tree depth exceeded bound"));
        }
        for entry in fs::read_dir(directory)? {
            count += 1;
            if count > 100_000 {
                return Err(io::Error::other("Runtime file inventory exceeded bound"));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Runtime contains a link"));
            }
            if metadata.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if metadata.is_file() && entry.file_name() == std::ffi::OsStr::new(name) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn run_generation(
    config: &Path,
    id: &str,
    state: &Path,
    plan: &StartPlan,
    nonce: &str,
    required: bool,
) -> io::Result<()> {
    let before = report_snapshot(state)?;
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
        "start key trust mission",
    )?;
    let status = invoke(
        config,
        &["server", "instance", "status", id],
        "status key trust mission",
    )?;
    let run = number(
        &status["data"]["status"]["run_generation"],
        "run generation",
    )?;
    let observed = wait_key_marker(state, &before, nonce, required, Duration::from_secs(240));
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
        "stop key trust mission",
    );
    observed?;
    let stopped = stop?;
    if stopped["data"]["outcome"]["status"]["forced"] != false {
        return Err(io::Error::other("Native key trust stop was forced"));
    }
    Ok(())
}

fn wait_key_marker(
    state: &Path,
    before: &std::collections::HashMap<std::path::PathBuf, u64>,
    nonce: &str,
    required: bool,
    timeout: Duration,
) -> io::Result<()> {
    let marker = format!("FOXY_KEY_TRUST|{nonce}|");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for path in key_report_paths(state)? {
            let bytes = fs::read(&path)?;
            let start = before
                .get(&path)
                .copied()
                .unwrap_or(0)
                .min(bytes.len() as u64) as usize;
            if let Some((_, record)) = String::from_utf8_lossy(&bytes[start..]).rsplit_once(&marker)
            {
                let record = record
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .trim_end_matches('"');

                let wanted = format!("{}|true|false|end", if required { "true" } else { "false" });
                if record == wanted {
                    return Ok(());
                }
                return Err(io::Error::other(format!(
                    "Fresh key-trust mission observation differed: {record}"
                )));
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Fresh engine output omitted the key-trust mission observation",
    ))
}

fn report_snapshot(state: &Path) -> io::Result<std::collections::HashMap<std::path::PathBuf, u64>> {
    let mut result = std::collections::HashMap::new();
    for path in key_report_paths(state)? {
        result.insert(path.clone(), fs::metadata(path)?.len());
    }
    Ok(result)
}

fn key_report_paths(state: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut pending = vec![(state.to_path_buf(), 0usize)];
    let mut output = Vec::new();
    let mut count = 0usize;
    while let Some((directory, depth)) = pending.pop() {
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
                pending.push((entry.path(), depth + 1));
            } else if metadata.is_file()
                && entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("rpt") || extension.eq_ignore_ascii_case("log")
                })
            {
                if metadata.len() > 16 * 1024 * 1024 {
                    return Err(io::Error::other("Engine report exceeded size bound"));
                }
                output.push(entry.path());
            }
        }
    }
    Ok(output)
}

fn hash_ordinary(path: &Path) -> io::Result<String> {
    let bytes = read_ordinary(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn write_key_mission(directory: &Path, nonce: &str) -> io::Result<()> {
    fs::create_dir(directory)?;
    let mission = b"version=12;\nclass Mission {\naddOns[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\naddOnsAuto[]={\"A3_Characters_F_BLUFOR\",\"A3_Map_VR\"};\nrandomSeed=1;\nclass Intel { year=2035; month=6; day=24; hour=12; minute=0; };\nclass Groups { items=1; class Item0 { side=\"WEST\"; class Vehicles { items=1; class Item0 { position[]={500,0,500}; id=0; side=\"WEST\"; vehicle=\"B_Soldier_F\"; player=\"PLAYER COMMANDER\"; leader=1; skill=0.5; }; }; }; };\n};\n";
    let description = b"onLoadName=\"Foxy key trust\";\nrespawn=3;\nclass Header { gameType=\"COOP\"; minPlayers=1; maxPlayers=1; };\n";
    let script = format!(
        "diag_log format [\"FOXY_KEY_TRUST|{nonce}|%1|%2|%3|end\", isClass (configFile >> \"CfgPatches\" >> \"FoxyNativeAlpha\"), isClass (configFile >> \"CfgPatches\" >> \"FoxyConfiguration_serveronly\"), isClass (configFile >> \"CfgPatches\" >> \"FoxyConfiguration_optional\")];\n[] spawn {{ sleep 8; endMission \"END1\"; }};\n"
    );
    fs::write(directory.join("mission.sqm"), mission)?;
    fs::write(directory.join("description.ext"), description)?;
    fs::write(directory.join("initServer.sqf"), script)
}

#[allow(clippy::too_many_arguments)]
fn reconcile_shared_content(
    config: &Path,
    case: &Path,
    id: &str,
    state: &Path,
    rollback: &StartPlan,
    engine_b: &Path,
    record_b: &str,
    nonce: &str,
    owner: transport::Owner,
) -> io::Result<transport::Owner> {
    let peer = string(
        &invoke(
            config,
            &["server", "instance", "add", "--name", "KeyTrustPeer"],
            "add peer instance",
        )?["data"]["instance"]["id"],
        "peer instance id",
    )?
    .to_owned();
    let bindings = case.join("peer-bindings.json");
    fs::write(
        &bindings,
        br#"[{"source_id":"Required","role":"required","order":0}]"#,
    )?;
    let reviewed = invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            &peer,
            "--bindings",
            path_arg(&bindings)?,
        ],
        "plan peer bindings",
    )?;
    invoke(
        config,
        &[
            "server",
            "content",
            "bindings",
            "save",
            &peer,
            "--bindings",
            path_arg(&bindings)?,
            "--expected-plan-fingerprint",
            string(
                &reviewed["data"]["plan"]["fingerprint"],
                "peer binding fingerprint",
            )?,
        ],
        "save peer bindings",
    )?;
    let peer_stage = stage_again(config, &peer)?;
    let peer_stage_dir = config
        .join("games")
        .join(GAME)
        .join("server/instances")
        .join(&peer)
        .join("content-stages")
        .join(&peer_stage);
    let peer_hash = inventory::tree_hash(&peer_stage_dir)?;

    let report = invoke(
        config,
        &["server", "content", "consumers"],
        "content consumers",
    )?;
    let required = report["data"]["report"]["sources"]
        .as_array()
        .and_then(|sources| {
            sources
                .iter()
                .find(|source| source["profile"] == "Required")
        })
        .ok_or_else(|| io::Error::other("Consumer report omitted the Required source"))?;
    let users = required["instances"]
        .as_array()
        .map(|users| {
            users
                .iter()
                .filter_map(|user| user["instance_id"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !users.contains(&id) || !users.contains(&peer.as_str()) {
        return Err(io::Error::other(format!(
            "Shared Required source did not list both consumers: {users:?}"
        )));
    }

    let engine_b_name = engine_b
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Runtime B has no directory name"))?
        .to_owned();
    let ids = |plan: &Value, field: &str| -> Vec<String> {
        plan["data"]["plan"][field]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    invoke(
        config,
        &[
            "--yes",
            "server",
            "deploy",
            "pin",
            id,
            record_b,
            "--expected-pin-generation",
            "0",
        ],
        "pin generation B record",
    )?;
    let pinned = invoke(
        config,
        &["server", "deploy", "reconcile", "plan", id],
        "plan with pin",
    )?;
    if ids(&pinned, "candidates").contains(&engine_b_name)
        || !ids(&pinned, "retained").contains(&engine_b_name)
    {
        return Err(io::Error::other(
            "A pinned record's runtime was offered for removal",
        ));
    }
    invoke(
        config,
        &[
            "--yes",
            "server",
            "deploy",
            "unpin",
            id,
            record_b,
            "--expected-pin-generation",
            "1",
        ],
        "unpin generation B record",
    )?;
    let plan = invoke(
        config,
        &["server", "deploy", "reconcile", "plan", id],
        "plan reconciliation",
    )?;
    let active_name = rollback.launch["directory"]
        .as_str()
        .and_then(|path| Path::new(path).parent())
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Active runtime directory is unknown"))?
        .to_owned();
    if !ids(&plan, "candidates").contains(&engine_b_name)
        || ids(&plan, "candidates").contains(&active_name)
    {
        return Err(io::Error::other(format!(
            "Reconciliation candidates were wrong: {:?}",
            ids(&plan, "candidates")
        )));
    }
    let fingerprint = string(
        &plan["data"]["plan"]["fingerprint"],
        "reconciliation fingerprint",
    )?
    .to_owned();
    invoke(
        config,
        &[
            "server",
            "instance",
            "start",
            id,
            "--expected-activation-generation",
            &rollback.generation,
            "--expected-plan-fingerprint",
            &rollback.fingerprint,
            "--yes",
        ],
        "start instance before refused reclamation",
    )?;
    let running = invoke(
        config,
        &["server", "instance", "status", id],
        "running status",
    )?;
    let run = number(
        &running["data"]["status"]["run_generation"],
        "run generation",
    )?;
    let (code, refused) = transport::invoke_raw(
        config,
        &[
            "--yes",
            "server",
            "deploy",
            "reconcile",
            "apply",
            id,
            "--expected-plan-fingerprint",
            &fingerprint,
        ],
    )?;
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
        "stop after refused reclamation",
    )?;
    if code != 2
        || !refused
            .to_string()
            .contains("running or unresolved engine consumer")
        || !engine_b.exists()
        || stop["data"]["outcome"]["status"]["forced"] != false
    {
        return Err(io::Error::other(format!(
            "Reclamation was not refused while the instance engine ran: {code} {refused}"
        )));
    }
    let applied = invoke(
        config,
        &[
            "--yes",
            "server",
            "deploy",
            "reconcile",
            "apply",
            id,
            "--expected-plan-fingerprint",
            &fingerprint,
        ],
        "apply reconciliation",
    )?;
    if engine_b.exists() || applied["data"]["outcome"]["sources_changed"] != false {
        return Err(io::Error::other(
            "Reconciliation did not remove only the reviewed runtime",
        ));
    }
    if inventory::tree_hash(&peer_stage_dir)? != peer_hash {
        return Err(io::Error::other(
            "Reconciling one instance changed the peer's stage",
        ));
    }
    eprintln!(
        "Reconciled {} artifact(s), {} byte(s)",
        applied["data"]["outcome"]["removed"]
            .as_array()
            .map_or(0, Vec::len),
        applied["data"]["outcome"]["reclaimed_bytes"]
    );
    let again = start_plan(config, id)?;
    if again.runtime != rollback.runtime || again.fingerprint != rollback.fingerprint {
        return Err(io::Error::other(
            "Reconciliation changed the committed start plan",
        ));
    }
    run_generation(config, id, state, &again, nonce, true)?;
    Ok(owner)
}
