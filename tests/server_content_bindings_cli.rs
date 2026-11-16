use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn invoke(root: &Path, game: &str, args: &[&str], code: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .args(["--game-space", game]);
    command.env("FOXY_CONFIG_DIR", root);
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
    assert_eq!(value["ok"], code == 0);
    assert_eq!(value.as_object().unwrap().len(), 4);
    value
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    root
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.insert(path.strip_prefix(base).unwrap().to_path_buf(), Vec::new());
                visit(base, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn add_instance(root: &Path, space: &str) -> String {
    invoke(
        root,
        space,
        &["server", "instance", "add", "--name", "MainServer"],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn register(root: &Path, space: &str, name: &str, addon: &str, url: &str) {
    let folder = root.join(format!("sources-{space}-{name}"));
    fs::create_dir_all(folder.join(addon)).unwrap();
    fs::write(folder.join(addon).join("fixture.txt"), b"unchanged content").unwrap();
    invoke(
        root,
        space,
        &[
            "server",
            "content",
            "add-folder",
            "--id",
            name,
            "--path",
            folder.to_str().unwrap(),
        ],
        0,
    );
    invoke(
        root,
        space,
        &[
            "server",
            "content",
            "add-profile",
            "--name",
            name,
            "--folder",
            name,
            "--url",
            url,
            "--local-path",
            root.join(format!("client-{space}-{name}"))
                .to_str()
                .unwrap(),
            "--supply",
            "already-present",
            "--addon",
            addon,
        ],
        0,
    );
}

fn write_bindings(root: &Path, value: &Value) -> PathBuf {
    let path = root.join("reviewed-bindings.json");
    fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

fn plan(root: &Path, game: &str, id: &str, inputs: &Path, dry_run: bool) -> Value {
    let mut args = Vec::new();
    if dry_run {
        args.push("--dry-run");
    }
    args.extend([
        "server",
        "content",
        "bindings",
        "plan",
        id,
        "--bindings",
        inputs.to_str().unwrap(),
    ]);
    invoke(root, game, &args, 0)
}

fn save(root: &Path, game: &str, id: &str, inputs: &Path, fingerprint: &str, code: i32) -> Value {
    invoke(
        root,
        game,
        &[
            "server",
            "content",
            "bindings",
            "save",
            id,
            "--bindings",
            inputs.to_str().unwrap(),
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        code,
    )
}

fn binding_path(root: &Path, game: &str, id: &str) -> PathBuf {
    root.join("games")
        .join(game)
        .join("server/instances")
        .join(id)
        .join("content-bindings.json")
}

#[test]
fn bindings_review_and_roundtrip_are_read_only_until_explicit_save_for_both_games() {
    let root = fixture();
    for game in ["arma3", "reforger"] {
        let id = add_instance(root.path(), game);
        register(root.path(), game, "RepoLocal", "@mod_01", "");
        register(
            root.path(),
            game,
            "RepoRemote",
            "@mod_02",
            "https://repo.example.invalid/shared",
        );
        let inputs = write_bindings(
            root.path(),
            &json!([
                {"source_id":"RepoRemote", "role":"optional", "order":8},
                {"source_id":"RepoLocal", "role":"server_only", "order":2}
            ]),
        );
        let owned_path = binding_path(root.path(), game, &id);
        let before = inventory(root.path());
        let shown = invoke(
            root.path(),
            game,
            &["server", "content", "bindings", "show", &id],
            0,
        );
        assert_eq!(shown["data"]["state"]["document"], Value::Null);
        assert_eq!(shown["data"]["state"]["fingerprint"], Value::Null);
        assert_eq!(shown["data"]["state"]["review_current"], false);
        assert_eq!(shown["data"]["writes_performed"], false);
        invoke(
            root.path(),
            game,
            &[
                "--wipe-db",
                "--yes",
                "server",
                "content",
                "bindings",
                "plan",
                &id,
                "--bindings",
                inputs.to_str().unwrap(),
            ],
            2,
        );
        assert_eq!(inventory(root.path()), before);
        let reviewed = plan(root.path(), game, &id, &inputs, false);
        let reviewed_plan = &reviewed["data"]["plan"];
        assert_eq!(reviewed_plan["document"]["game_space_id"], game);
        assert_eq!(reviewed_plan["document"]["instance_id"], id);
        assert_eq!(reviewed_plan["document"]["generation"], 1);
        assert_eq!(reviewed_plan["expected_generation"], 0);
        assert_eq!(
            reviewed_plan["document"]["bindings"][0]["binding"]["source_id"],
            "RepoLocal"
        );
        assert_eq!(
            reviewed_plan["document"]["bindings"][0]["binding"]["role"],
            "server_only"
        );
        assert_eq!(
            reviewed_plan["document"]["bindings"][1]["binding"]["source_id"],
            "RepoRemote"
        );
        assert_eq!(
            reviewed_plan["document"]["bindings"][1]["binding"]["order"],
            8
        );
        assert_eq!(reviewed_plan["preview"]["deployment_ready"], false);
        assert_eq!(reviewed_plan["preview"]["downloads_started"], false);
        assert_eq!(reviewed_plan["preview"]["writes_performed"], false);
        assert_eq!(reviewed["data"]["writes_performed"], false);
        let dry_run = plan(root.path(), game, &id, &inputs, true);
        assert_eq!(
            dry_run["data"]["plan"]["fingerprint"],
            reviewed_plan["fingerprint"]
        );
        assert_eq!(inventory(root.path()), before);
        assert!(!owned_path.exists());
        assert!(
            !root
                .path()
                .join(format!("games/{game}/database.db"))
                .exists()
        );

        let fingerprint = reviewed_plan["fingerprint"].as_str().unwrap();
        let saved = save(root.path(), game, &id, &inputs, fingerprint, 0);
        assert_eq!(saved["data"]["writes_performed"], true);
        assert_eq!(saved["data"]["downloads_started"], false);
        assert_eq!(saved["data"]["deployment_ready"], false);
        assert_eq!(saved["data"]["state"]["document"]["generation"], 1);
        assert_eq!(saved["data"]["state"]["review_current"], true);
        let after = inventory(root.path());
        let owner_path = PathBuf::from(format!("games/{game}/database.owner"));
        for (path, bytes) in &before {
            if path != &owner_path {
                assert_eq!(after.get(path), Some(bytes), "changed file {path:?}");
            }
        }
        assert!(after.contains_key(&owner_path));
        let mut added = after
            .keys()
            .filter(|path| !before.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        let mut expected_added = vec![
            PathBuf::from(format!("games/{game}/server/instances")),
            PathBuf::from(format!("games/{game}/server/instances/{id}")),
            PathBuf::from(format!(
                "games/{game}/server/instances/{id}/content-bindings.json"
            )),
        ];
        for path in [
            PathBuf::from(format!("games/{game}/database.lock")),
            owner_path.clone(),
        ] {
            if !before.contains_key(&path) {
                expected_added.push(path);
            }
        }
        added.sort();
        expected_added.sort();
        assert_eq!(added, expected_added);
        assert!(
            !root
                .path()
                .join(format!("games/{game}/database.db"))
                .exists()
        );
        let reloaded = invoke(
            root.path(),
            game,
            &["server", "content", "bindings", "show", &id],
            0,
        );
        assert_eq!(
            reloaded["data"]["state"]["document"],
            saved["data"]["state"]["document"]
        );
        assert_eq!(reloaded["data"]["state"]["review_current"], true);

        let empty_inputs = write_bindings(root.path(), &json!([]));
        let clear_plan = plan(root.path(), game, &id, &empty_inputs, false);
        assert_eq!(clear_plan["data"]["plan"]["expected_generation"], 1);
        assert_eq!(clear_plan["data"]["plan"]["document"]["generation"], 2);
        save(
            root.path(),
            game,
            &id,
            &empty_inputs,
            clear_plan["data"]["plan"]["fingerprint"].as_str().unwrap(),
            0,
        );
        let cleared = invoke(
            root.path(),
            game,
            &["server", "content", "bindings", "show", &id],
            0,
        );
        assert_eq!(cleared["data"]["state"]["document"]["generation"], 2);
        assert_eq!(cleared["data"]["state"]["document"]["bindings"], json!([]));
        assert!(
            inventory(root.path())
                .contains_key(&owned_path.strip_prefix(root.path()).unwrap().to_path_buf())
        );
    }
}

#[test]
fn stale_review_tokens_preserve_raw_owned_files_and_catalog_or_source_edits() {
    let root = fixture();
    let game = "arma3";
    let id = add_instance(root.path(), game);
    register(root.path(), game, "RepoAlpha", "@mod_01", "");
    let inputs = write_bindings(
        root.path(),
        &json!([{"source_id":"RepoAlpha", "role":"required", "order":1}]),
    );
    let initial = plan(root.path(), game, &id, &inputs, false);
    save(
        root.path(),
        game,
        &id,
        &inputs,
        initial["data"]["plan"]["fingerprint"].as_str().unwrap(),
        0,
    );
    let owned_path = binding_path(root.path(), game, &id);

    let mut changed_inputs = json!([{"source_id":"RepoAlpha", "role":"optional", "order":1}]);
    fs::write(&inputs, serde_json::to_vec(&changed_inputs).unwrap()).unwrap();
    let old_plan = initial["data"]["plan"]["fingerprint"].as_str().unwrap();
    let owned_before = fs::read(&owned_path).unwrap();
    let inventory_before = inventory(root.path());
    save(root.path(), game, &id, &inputs, old_plan, 2);
    assert_eq!(fs::read(&owned_path).unwrap(), owned_before);
    assert_eq!(inventory(root.path()), inventory_before);

    changed_inputs[0]["role"] = "required".into();
    fs::write(&inputs, serde_json::to_vec(&changed_inputs).unwrap()).unwrap();
    let source_path = root
        .path()
        .join(format!("games/{game}/server/content-sources.json"));
    let reviewed = plan(root.path(), game, &id, &inputs, false);
    let source_fingerprint = reviewed["data"]["plan"]["document"]["content_sources_fingerprint"]
        .as_str()
        .unwrap();
    let mut source_bytes = fs::read(&source_path).unwrap();
    source_bytes.push(b'\n');
    fs::write(&source_path, &source_bytes).unwrap();
    let inventory_before = inventory(root.path());
    save(
        root.path(),
        game,
        &id,
        &inputs,
        reviewed["data"]["plan"]["fingerprint"].as_str().unwrap(),
        2,
    );
    assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
    assert_eq!(fs::read(&owned_path).unwrap(), owned_before);
    assert_eq!(inventory(root.path()), inventory_before);
    assert!(!source_fingerprint.is_empty());

    let reviewed = plan(root.path(), game, &id, &inputs, false);
    let catalog_path = root
        .path()
        .join(format!("games/{game}/server/servers.json"));
    let mut catalog_bytes = fs::read(&catalog_path).unwrap();
    catalog_bytes.push(b'\n');
    fs::write(&catalog_path, &catalog_bytes).unwrap();
    let inventory_before = inventory(root.path());
    save(
        root.path(),
        game,
        &id,
        &inputs,
        reviewed["data"]["plan"]["fingerprint"].as_str().unwrap(),
        2,
    );
    assert_eq!(fs::read(&catalog_path).unwrap(), catalog_bytes);
    assert_eq!(fs::read(&owned_path).unwrap(), owned_before);
    assert_eq!(inventory(root.path()), inventory_before);

    let reviewed = plan(root.path(), game, &id, &inputs, false);
    let mut binding_bytes = fs::read(&owned_path).unwrap();
    binding_bytes.push(b'\n');
    fs::write(&owned_path, &binding_bytes).unwrap();
    let inventory_before = inventory(root.path());
    save(
        root.path(),
        game,
        &id,
        &inputs,
        reviewed["data"]["plan"]["fingerprint"].as_str().unwrap(),
        2,
    );
    assert_eq!(fs::read(&owned_path).unwrap(), binding_bytes);
    assert_eq!(inventory(root.path()), inventory_before);
}

#[test]
fn corrupt_documents_are_preserved_and_busy_owner_blocks_only_actual_save() {
    let root = fixture();
    let game = "arma3";
    let id = add_instance(root.path(), game);
    register(root.path(), game, "RepoAlpha", "@mod_01", "");
    let inputs = write_bindings(
        root.path(),
        &json!([{"source_id":"RepoAlpha", "role":"required", "order":1}]),
    );
    let reviewed = plan(root.path(), game, &id, &inputs, false);
    let fingerprint = reviewed["data"]["plan"]["fingerprint"].as_str().unwrap();
    let owned_path = binding_path(root.path(), game, &id);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.path().join(format!("games/{game}/database.lock")))
        .unwrap();
    let before = inventory(root.path());
    lock.lock().unwrap();
    let shown = invoke(
        root.path(),
        game,
        &["server", "content", "bindings", "show", &id],
        0,
    );
    assert_eq!(shown["data"]["state"]["document"], Value::Null);
    let preview = plan(root.path(), game, &id, &inputs, false);
    assert_eq!(preview["data"]["plan"]["fingerprint"], fingerprint);
    let dry = invoke(
        root.path(),
        game,
        &[
            "--dry-run",
            "server",
            "content",
            "bindings",
            "save",
            &id,
            "--bindings",
            inputs.to_str().unwrap(),
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        0,
    );
    assert_eq!(dry["data"]["writes_performed"], false);
    save(root.path(), game, &id, &inputs, fingerprint, 6);
    assert!(!owned_path.exists());
    drop(lock);
    assert_eq!(inventory(root.path()), before);

    let claim = root
        .path()
        .join(format!("games/{game}/server/instances/{id}"));
    fs::create_dir_all(&claim).unwrap();
    let malformed = [
        b"{".to_vec(),
        br#"{"schema_version":999}"#.to_vec(),
        br#"{"schema_version":1,"unexpected":true}"#.to_vec(),
    ];
    for bytes in malformed {
        fs::write(&owned_path, &bytes).unwrap();
        let before = inventory(root.path());
        invoke(
            root.path(),
            game,
            &["server", "content", "bindings", "show", &id],
            4,
        );
        assert_eq!(fs::read(&owned_path).unwrap(), bytes);
        assert_eq!(inventory(root.path()), before);
    }
}
