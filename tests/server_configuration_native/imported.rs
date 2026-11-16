use super::*;

pub(super) fn run() -> io::Result<()> {
    let source = inventory::required_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = inventory::required_directory("FOXY_ARMA_IMPORT_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and import scratch must be separate",
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
            "Import scratch path is too long for the native engine",
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
    let host = format!("FoxyImported \"{nonce}\"");
    let identity = format!("Imported_{nonce}");
    let added = invoke(
        &config,
        &["server", "instance", "add", "--name", "ImportedNative"],
        "add instance",
    )?;
    let id = string(&added["data"]["instance"]["id"], "instance id")?.to_owned();
    let mut draft =
        invoke(&config, &["server", "instance", "show", &id], "show draft")?["data"]["instance"]
            .clone();
    draft["definition"]["settings"] = generation_a(&host);
    replace(&config, &case, &draft, 1, "draft.json")?;
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
        "adopt",
    )?;
    let preview = invoke(
        &config,
        &["server", "instance", "render", &id],
        "render source",
    )?["data"]["configuration_preview"]
        .clone();
    let mut inputs = Vec::new();
    let fixture_secret = "FixtureNativePrivate349";
    for (name, role) in [
        (SERVER_CONFIG, "--server"),
        (BASIC_CONFIG, "--basic"),
        ("Managed.Arma3Profile", "--profile"),
    ] {
        let file = preview["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["relative_path"] == name)
            .ok_or_else(|| io::Error::other("Source preview file missing"))?;
        let mut text = string(&file["contents"], "source contents")?.to_owned();
        if name == SERVER_CONFIG {
            text.push_str(&format!("\n// Operator comment retained exactly\npasswordAdmin = \"{fixture_secret}\";\nclass ExtraPolicy {{ path=\"R:/Mods/Unresolved Folder\"; markers[]={{{{1,2}},true}}; }};\n"));
        }
        let path = case.join(name);
        fs::write(&path, text)?;
        inputs.push((role, path));
    }
    let mut args = vec![
        "--yes",
        "server",
        "configuration",
        "import",
        &id,
        "--expected-revision",
        "3",
        "--review-paths",
        "--apply",
    ];
    for (role, path) in &inputs {
        args.extend([*role, path_arg(path)?]);
    }
    let imported = invoke(&config, &args, "operator import")?;
    if serde_json::to_string(&imported)?.contains(fixture_secret) {
        return Err(io::Error::other("Secret leaked into import review"));
    }
    let committed = commit_configuration(&config, &id, 4)?;
    if serde_json::to_string(&committed)?.contains(fixture_secret) {
        return Err(io::Error::other(
            "Secret leaked into configuration snapshot",
        ));
    }
    setup_content(&config, &case, &id)?;
    let prepared = prepare(
        &config,
        &case,
        &id,
        "alpha",
        &identity,
        PreparationRevision {
            deployment: 0,
            draft: 4,
            configuration: 1,
        },
    )?;
    activate(&config, &id, &prepared.preparation, 1, 0)?;
    let plan = start_plan(&config, &id)?;
    let seeds = plan.launch["configuration_seeds"]
        .as_array()
        .ok_or_else(|| io::Error::other("Protected configuration seeds missing"))?;
    if seeds.len() != 1 {
        return Err(io::Error::other(
            "Expected exactly the protected server configuration seed",
        ));
    }
    let destination = PathBuf::from(string(&seeds[0]["destination"], "protected destination")?);
    if destination.exists() {
        return Err(io::Error::other(
            "Read-only launch planning materialized a protected file",
        ));
    }
    let source_cfg = PathBuf::from(string(&seeds[0]["source"], "configuration template")?);
    let template = fs::read_to_string(&source_cfg)?;
    if template.contains(fixture_secret)
        || !template.contains("foxy-secret:secret_")
        || !template.contains("// Operator comment retained exactly")
    {
        return Err(io::Error::other(
            "Archived operator bytes or redaction are incorrect",
        ));
    }
    let owner = transport::start_owner(&config)?;
    run_once(
        &config,
        &id,
        &runtime_state(&config, &id),
        &plan,
        &Expected {
            identity: &identity,
            hostname: &host,
            crosshair: 1,
            third_person: 0,
            tactical_ping: 3,
            parameter: 17,
            profile: "missing",
            mission: "missing",
        },
    )?;
    let private = fs::read_to_string(&destination)?;
    if !private.contains(fixture_secret)
        || private.contains("foxy-secret:")
        || !private.contains("class ExtraPolicy")
    {
        return Err(io::Error::other(
            "Protected runtime did not reconstruct the reviewed operator configuration",
        ));
    }
    let launch = launch_paths(&plan.launch)?;
    let arguments = plan.launch["arguments"].as_array().unwrap();
    if arguments
        .iter()
        .filter_map(Value::as_str)
        .any(|arg| arg.contains(fixture_secret))
    {
        return Err(io::Error::other("Credential leaked into process arguments"));
    }
    let engine_hash = inventory::tree_hash(&launch.engine)?;
    let before_private = fs::read(&destination)?;
    run_once(
        &config,
        &id,
        &runtime_state(&config, &id),
        &plan,
        &Expected {
            identity: &identity,
            hostname: &host,
            crosshair: 1,
            third_person: 0,
            tactical_ping: 3,
            parameter: 17,
            profile: "foxy-managed-profile-retained",
            mission: "foxy-managed-mission-retained",
        },
    )?;
    if fs::read(&destination)? != before_private
        || fs::read_to_string(&source_cfg)? != template
        || inventory::tree_hash(&source)? != source_hash
        || inventory::tree_hash(&adopted)? != adopted_hash
        || inventory::tree_hash(&launch.engine)? != engine_hash
    {
        return Err(io::Error::other(
            "Source, adopted bytes or reviewed generations changed",
        ));
    }
    drop(owner);
    eprintln!(
        "Native operator import and protected configuration accepted; fresh mission and retained namespaces observed twice; case {}",
        case.display()
    );
    Ok(())
}
