use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn configuration_commit_respects_the_existing_owner_while_reviews_remain_write_free() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let directory = root.path().join("games/arma3/server");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("servers.json"),
        include_str!("../examples/json/appdata/games/arma3/server/servers.json"),
    )
    .unwrap();
    let lock = fs::File::create(root.path().join("games/arma3/database.lock")).unwrap();
    let before = inventory(root.path());
    lock.try_lock().unwrap();
    let id = "server_00000000000000000000000000000001";
    let plan = invoke(
        root.path(),
        "arma3",
        &["server", "configuration", "plan", id],
        0,
    );
    commit(root.path(), "arma3", id, &plan, true, 0);
    let review = invoke(
        root.path(),
        "arma3",
        &["server", "instance", "preflight", id],
        0,
    );
    assert_eq!(
        review["data"]["preflight"]["configuration"]["status"],
        "not_selected"
    );
    commit(root.path(), "arma3", id, &plan, false, 6);
    drop(lock);
    assert_eq!(inventory(root.path()), before);
    assert!(!root.path().join("games/arma3/database.db").exists());
}

fn invoke(root: &Path, game: &str, args: &[&str], code: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .args(["--game-space", game]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "args={args:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    if code == 0 {
        assert_eq!(value["ok"], true);
    } else if value["data"]["verification"].is_object() {
        assert_eq!(value["data"]["verification"]["valid"], false);
    } else {
        assert_eq!(value["ok"], false);
        assert!(value["data"].is_null());
    }
    value
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if root.exists() {
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            let name = PathBuf::from(path.file_name().unwrap());
            if path.is_dir() {
                for (child, bytes) in inventory(&path) {
                    files.insert(name.join(child), bytes);
                }
            } else {
                files.insert(name, fs::read(path).unwrap());
            }
        }
    }
    files
}

fn commit(root: &Path, game: &str, id: &str, plan: &Value, dry_run: bool, code: i32) -> Value {
    let generation = plan["data"]["expected_generation"].to_string();
    let revision = plan["data"]["preview"]["instance_revision"].to_string();
    let fingerprint = plan["data"]["preview"]["fingerprint"].as_str().unwrap();
    let mut args = vec![
        "server",
        "configuration",
        "commit",
        id,
        "--expected-revision",
        &revision,
        "--expected-generation",
        &generation,
        "--expected-fingerprint",
        fingerprint,
    ];
    if dry_run {
        args.push("--dry-run");
    }
    invoke(root, game, &args, code)
}

fn readiness(root: &Path, game: &str, id: &str, status: &str, matches: Option<bool>) {
    let before = inventory(root);
    let result = invoke(root, game, &["server", "instance", "preflight", id], 0);
    let report = &result["data"]["preflight"];
    assert_eq!(report["configuration"]["status"], status);
    assert_eq!(
        report["configuration"]["matches_current_draft"],
        serde_json::to_value(matches).unwrap()
    );
    assert_eq!(
        report["runtime_prerequisites"]["installation_leases"],
        "not_acquired"
    );
    assert_eq!(report["runtime_prerequisites"]["launch_available"], false);
    assert_eq!(
        report["runtime_prerequisites"]["crash_consumer_reconciliation"],
        "persistent_registration_barrier"
    );
    assert_eq!(report["writes_performed"], false);
    assert_eq!(report["deployment_ready"], false);
    let capabilities = invoke(root, game, &["server", "capabilities"], 0);
    assert_eq!(
        capabilities["data"]["runtime_prerequisites"],
        report["runtime_prerequisites"]
    );
    assert_eq!(
        capabilities["data"]["capabilities"]["lifecycle"],
        matches!(game, "arma3" | "reforger")
            && cfg!(all(
                target_arch = "x86_64",
                any(windows, target_os = "linux")
            ))
    );
    assert_eq!(inventory(root), before);
}

#[test]
fn both_games_render_commit_verify_and_rollback_without_engine_or_installation_writes() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let registry = fs::read(root.path().join("games.json")).unwrap();
    for game in ["arma3", "reforger"] {
        let added = invoke(
            root.path(),
            game,
            &["server", "instance", "add", "--name", "MainServer"],
            0,
        );
        let id = added["data"]["instance"]["id"].as_str().unwrap();
        let catalog_path = root
            .path()
            .join("games")
            .join(game)
            .join("server/servers.json");
        let catalog = fs::read(&catalog_path).unwrap();
        let before = inventory(root.path());
        readiness(root.path(), game, id, "not_selected", None);
        let rendered = invoke(root.path(), game, &["server", "instance", "render", id], 0);
        assert_eq!(
            rendered["data"]["configuration_preview"]["launch_ready"],
            false
        );
        let empty = invoke(
            root.path(),
            game,
            &["server", "configuration", "list", id],
            0,
        );
        assert_eq!(empty["data"]["state"]["generation"], 0);
        let first = invoke(
            root.path(),
            game,
            &["server", "configuration", "plan", id],
            0,
        );
        commit(root.path(), game, id, &first, true, 0);
        assert_eq!(inventory(root.path()), before);
        let saved = commit(root.path(), game, id, &first, false, 0);
        assert_eq!(saved["data"]["state"]["generation"], 1);
        readiness(
            root.path(),
            game,
            id,
            "verified_configuration_only",
            Some(true),
        );
        assert_eq!(fs::read(&catalog_path).unwrap(), catalog);
        let first_id = first["data"]["revision_id"].as_str().unwrap();
        let directory = root
            .path()
            .join("games")
            .join(game)
            .join("server/instances")
            .join(id)
            .join("configuration");
        let first_directory = directory.join("revisions").join(first_id);
        let first_bytes = inventory(&first_directory);
        assert!(first_bytes.contains_key(Path::new("manifest.json")));
        let valid = invoke(
            root.path(),
            game,
            &[
                "server",
                "configuration",
                "verify",
                id,
                "--revision",
                first_id,
            ],
            0,
        );
        assert_eq!(valid["data"]["verification"]["valid"], true);
        let mut instance = added["data"]["instance"].clone();
        let field = if game == "arma3" { "hostname" } else { "name" };
        instance["definition"]["settings"][field] = "SecondServer".into();
        let input = root.path().join("instance.json");
        fs::write(&input, serde_json::to_vec(&instance).unwrap()).unwrap();
        invoke(
            root.path(),
            game,
            &[
                "server",
                "instance",
                "replace",
                "--file",
                input.to_str().unwrap(),
                "--expected-revision",
                "1",
            ],
            0,
        );
        let second = invoke(
            root.path(),
            game,
            &["server", "configuration", "plan", id],
            0,
        );
        readiness(
            root.path(),
            game,
            id,
            "verified_configuration_only",
            Some(false),
        );
        assert_ne!(second["data"]["revision_id"], first["data"]["revision_id"]);
        commit(root.path(), game, id, &first, false, 2);
        commit(root.path(), game, id, &second, false, 0);
        assert_eq!(inventory(&first_directory), first_bytes);
        let before_rollback = inventory(root.path());
        let rollback = [
            "server",
            "configuration",
            "rollback",
            id,
            "--revision",
            first_id,
            "--expected-generation",
            "2",
        ];
        invoke(root.path(), game, &rollback, 2);
        let mut preview = rollback.to_vec();
        preview.push("--dry-run");
        invoke(root.path(), game, &preview, 0);
        assert_eq!(inventory(root.path()), before_rollback);
        let mut apply = rollback.to_vec();
        apply.push("--yes");
        let rolled_back = invoke(root.path(), game, &apply, 0);
        assert_eq!(rolled_back["data"]["state"]["active"], first_id);
        assert_eq!(rolled_back["data"]["state"]["generation"], 3);
        readiness(
            root.path(),
            game,
            id,
            "verified_configuration_only",
            Some(false),
        );
        let current = invoke(root.path(), game, &["server", "instance", "show", id], 0);
        assert_eq!(current["data"]["instance"]["revision"], 2);
        let pointer_before = fs::read(directory.join("state.json")).unwrap();
        let unexpected = first_directory.join("unowned.cfg");
        fs::write(&unexpected, b"unowned").unwrap();
        readiness(root.path(), game, id, "invalid", None);
        invoke(
            root.path(),
            game,
            &[
                "server",
                "configuration",
                "verify",
                id,
                "--revision",
                first_id,
            ],
            4,
        );
        invoke(
            root.path(),
            game,
            &[
                "server",
                "configuration",
                "rollback",
                id,
                "--revision",
                first_id,
                "--expected-generation",
                "3",
                "--yes",
            ],
            4,
        );
        assert_eq!(
            fs::read(directory.join("state.json")).unwrap(),
            pointer_before
        );
        assert_eq!(fs::read(&unexpected).unwrap(), b"unowned");
        fs::remove_file(unexpected).unwrap();
        let config = first_directory.join(if game == "arma3" {
            "server.cfg"
        } else {
            "server.json"
        });
        fs::write(config, b"tampered").unwrap();
        invoke(
            root.path(),
            game,
            &[
                "server",
                "configuration",
                "verify",
                id,
                "--revision",
                first_id,
            ],
            4,
        );
        assert_eq!(
            fs::read(directory.join("state.json")).unwrap(),
            pointer_before
        );
        fs::write(directory.join("state.json"), b"invalid history").unwrap();
        readiness(root.path(), game, id, "unreadable", None);
        fs::write(directory.join("state.json"), &pointer_before).unwrap();
        assert!(
            !root
                .path()
                .join("games")
                .join(game)
                .join("database.db")
                .exists()
        );
    }
    assert_eq!(fs::read(root.path().join("games.json")).unwrap(), registry);
    assert!(!root.path().join("logs").exists());
}

#[test]
fn managed_arma_configuration_round_trips_through_json_replace_and_commit() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let added = invoke(
        root.path(),
        "arma3",
        &["server", "instance", "add", "--name", "MainServer"],
        0,
    );
    let id = added["data"]["instance"]["id"].as_str().unwrap().to_owned();
    let example: Value = serde_json::from_str(include_str!(
        "../examples/json/server_backend/arma3_managed_configuration.json"
    ))
    .unwrap();
    let mut instance = added["data"]["instance"].clone();
    instance["definition"] = example["definition"].clone();
    let input = root.path().join("managed.json");
    let replace = |instance: &Value, revision: &str, code: i32| {
        fs::write(&input, serde_json::to_vec(instance).unwrap()).unwrap();
        invoke(
            root.path(),
            "arma3",
            &[
                "server",
                "instance",
                "replace",
                "--file",
                input.to_str().unwrap(),
                "--expected-revision",
                revision,
            ],
            code,
        )
    };
    replace(&instance, "1", 0);
    let shown = invoke(
        root.path(),
        "arma3",
        &["server", "instance", "show", &id],
        0,
    );
    assert_eq!(
        shown["data"]["instance"]["definition"],
        example["definition"]
    );

    let plan = invoke(
        root.path(),
        "arma3",
        &["server", "configuration", "plan", &id],
        0,
    );
    let files = plan["data"]["preview"]["files"].as_array().unwrap();
    let names = files
        .iter()
        .map(|file| file["relative_path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "dedicated.cfg",
            "network.cfg",
            "Managed.Arma3Profile",
            "launch-intent.json"
        ]
    );
    for (file, expected) in files.iter().zip([
        include_str!("../examples/arma3/dedicated.cfg"),
        include_str!("../examples/arma3/network.cfg"),
        include_str!("../examples/arma3/Managed.Arma3Profile"),
    ]) {
        assert_eq!(file["contents"], expected.replace("\r\n", "\n"));
    }
    let intent: Value = serde_json::from_str(files[3]["contents"].as_str().unwrap()).unwrap();
    assert_eq!(
        intent,
        serde_json::json!({
            "game_port": 2302,
            "auto_init": true,
            "server_config": "dedicated.cfg",
            "basic_config": "network.cfg",
            "profile_name": "Managed"
        })
    );
    commit(root.path(), "arma3", &id, &plan, false, 0);
    let revision = plan["data"]["revision_id"].as_str().unwrap();
    let committed = root
        .path()
        .join("games/arma3/server/instances")
        .join(&id)
        .join("configuration/revisions")
        .join(revision);
    let stored = inventory(&committed);
    for file in files {
        let name = file["relative_path"].as_str().unwrap();
        let bytes = stored
            .iter()
            .find(|(path, _)| path.file_name().and_then(|value| value.to_str()) == Some(name))
            .map(|(_, bytes)| bytes.clone())
            .unwrap_or_else(|| panic!("committed revision lacks {name}"));
        assert_eq!(bytes, file["contents"].as_str().unwrap().as_bytes());
    }

    let catalog = root.path().join("games/arma3/server/servers.json");
    let before = fs::read(&catalog).unwrap();
    let mut invalid = shown["data"]["instance"].clone();
    for (path, value) in [
        ("idle_fps_limit", serde_json::json!(61)),
        ("disable_channels", serde_json::json!([[16, false, false]])),
        ("on_different_data", serde_json::json!("")),
        ("motd_interval", serde_json::json!(-1)),
    ] {
        invalid["definition"]["settings"]["server"] =
            example["definition"]["settings"]["server"].clone();
        invalid["definition"]["settings"]["server"][path] = value;
        replace(&invalid, "2", 2);
    }
    invalid["definition"]["settings"]["server"] =
        example["definition"]["settings"]["server"].clone();
    invalid["definition"]["settings"]["configuration"]["basic"] = "dedicated.cfg".into();
    replace(&invalid, "2", 2);
    assert_eq!(fs::read(&catalog).unwrap(), before);

    let mut legacy = shown["data"]["instance"].clone();
    for section in ["server", "difficulty", "configuration"] {
        legacy["definition"]["settings"]
            .as_object_mut()
            .unwrap()
            .remove(section);
    }
    legacy["definition"]["settings"]["basic"] = serde_json::json!({});
    legacy["definition"]["settings"]["mission_cycle"] = serde_json::json!([]);
    replace(&legacy, "2", 0);
    let legacy_plan = invoke(
        root.path(),
        "arma3",
        &["server", "configuration", "plan", &id],
        0,
    );
    let legacy_files = legacy_plan["data"]["preview"]["files"].as_array().unwrap();
    assert_eq!(legacy_files[0]["relative_path"], "server.cfg");
    assert_eq!(legacy_files[1]["relative_path"], "basic.cfg");
    assert_eq!(legacy_files[1]["contents"], "");
    assert_eq!(legacy_files.len(), 3);
    let stored: Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    let settings = &stored["instances"][0]["definition"]["settings"];
    assert!(settings.get("server").is_none());
    assert!(settings.get("difficulty").is_none());
    assert!(settings.get("configuration").is_none());
}
