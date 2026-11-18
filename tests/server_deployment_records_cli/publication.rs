use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{invoke, prepare_record};

pub(super) fn add_player_inputs(root: &Path, folder: &Path, rows: &mut Vec<Value>) {
    fs::create_dir(folder.join("@mod_01/addons")).unwrap();
    fs::write(folder.join("@mod_01/addons/radio.pbo"), b"player radio").unwrap();
    fs::write(
        folder.join("@mod_01/addons/radio.pbo.alpha.bisign"),
        b"public signature",
    )
    .unwrap();
    fs::write(
        folder.join("@mod_01/keys/repo.biprivatekey"),
        b"private signing material",
    )
    .unwrap();
    fs::write(
        folder.join("@mod_01/server.cfg"),
        b"passwordAdmin = PRIVATE_SENTINEL;",
    )
    .unwrap();
    fs::create_dir(folder.join("@mod_01/userconfig")).unwrap();
    fs::write(
        folder.join("@mod_01/userconfig/settings.pbo"),
        b"operator settings",
    )
    .unwrap();

    for (profile, role, name, order) in [
        ("RepoBeta", "optional", "@mod_02", 1),
        ("RepoGamma", "server_only", "@mod_03", 2),
    ] {
        fs::create_dir_all(folder.join(name).join("addons")).unwrap();
        fs::write(folder.join(name).join("addons/payload.pbo"), profile).unwrap();
        invoke(
            root,
            "arma3",
            &[
                "server",
                "content",
                "add-profile",
                "--name",
                profile,
                "--folder",
                "RepoAlpha",
                "--url",
                "",
                "--local-path",
                root.join(profile).to_str().unwrap(),
                "--supply",
                "already-present",
                "--addon",
                name,
            ],
            0,
        );
        rows.push(json!({"source_id":profile,"role":role,"order":order}));
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn arma_publish_respects_reviewed_selection_and_checks_exact_output() {
    use super::{fixture, inventory};
    use std::path::PathBuf;

    let root = fixture();
    let parent = tempfile::tempdir().unwrap();
    let output = parent.path().join("player-output");
    let target = output.to_str().unwrap();
    let deployment = prepare_record(root.path(), "arma3", true);
    let id = deployment.id.as_str();
    let before = inventory(root.path());
    let plan = invoke(
        root.path(),
        "arma3",
        &["server", "publish", "plan", id, "--output", target],
        0,
    );
    assert_eq!(plan["data"]["plan"]["record_id"], deployment.record);
    assert_eq!(plan["data"]["plan"]["files"], 2);
    assert_eq!(plan["data"]["plan"]["bytes"], 28);
    assert_eq!(plan["data"]["database_opened"], false);
    assert!(inventory(parent.path()).is_empty());
    assert_eq!(inventory(root.path()), before);

    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "publish",
            "plan",
            id,
            "--output",
            target,
            "--recommend",
            "RepoGamma",
        ],
        2,
    );
    let inputs = [
        id,
        "--output",
        target,
        "--recommend",
        "RepoBeta",
        "--include-public-keys",
    ];
    let run = |verb, more: &[&str], code| {
        let args = [["server", "publish", verb].as_slice(), &inputs, more].concat();
        invoke(root.path(), "arma3", &args, code)
    };
    let plan = run("plan", &[], 0);
    let fingerprint = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    let review = ["--expected-plan-fingerprint", fingerprint];
    assert_eq!(plan["data"]["plan"]["files"], 4);
    assert_eq!(plan["data"]["plan"]["arma_addons"][2]["selected"], false);
    run("apply", &review, 2);
    run(
        "apply",
        &["--yes", "--expected-plan-fingerprint", "stale"],
        2,
    );
    let preview = run("apply", &["--dry-run", review[0], review[1]], 0);
    assert_eq!(preview["data"]["plan"]["fingerprint"], fingerprint);
    assert!(inventory(parent.path()).is_empty());
    let result = run("apply", &["--yes", review[0], review[1]], 0);
    assert_eq!(result["data"]["outcome"]["verified"], true);
    assert_eq!(result["data"]["writes_performed"], true);

    let expected: Vec<PathBuf> = [
        "@mod_01",
        "@mod_01/addons",
        "@mod_01/addons/radio.pbo",
        "@mod_01/addons/radio.pbo.alpha.bisign",
        "@mod_01/keys",
        "@mod_01/keys/server.bikey",
        "@mod_01/foxy_addon.json",
        "@mod_02",
        "@mod_02/addons",
        "@mod_02/addons/payload.pbo",
        "@mod_02/foxy_addon.json",
        "repo.json",
        "foxy_addons.json",
        "server_mod_line.txt",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    let published = inventory(&output);
    assert_eq!(published.len(), expected.len());
    for path in expected {
        assert!(published.contains_key(&path), "{path:?}");
    }
    assert_eq!(
        &published[Path::new("@mod_01/addons/radio.pbo")],
        b"player radio"
    );
    let listing: Value = serde_json::from_slice(&published[Path::new("foxy_addons.json")]).unwrap();
    assert_eq!(listing["requiredMods"][0]["modName"], "@mod_01");
    assert_eq!(listing["requiredMods"][0]["enabled"], true);
    assert!(listing["requiredMods"][0].get("clientSide").is_none());
    assert_eq!(listing["optionalMods"][0]["modName"], "@mod_02");
    assert_eq!(listing["optionalMods"][0]["enabled"], false);
    assert_eq!(
        &published[Path::new("server_mod_line.txt")],
        b"-mod=@mod_01;\n"
    );
    run("verify", &review, 0);
    run("apply", &["--yes", review[0], review[1]], 2);
    assert_eq!(inventory(&output), published);
    assert_eq!(inventory(root.path()), before);

    fs::write(output.join("manifest.json"), b"unexpected file").unwrap();
    run("verify", &[], 4);
    fs::remove_file(output.join("manifest.json")).unwrap();
    fs::write(output.join("@mod_01/addons/radio.pbo"), b"wrong bytes!").unwrap();
    run("verify", &[], 4);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn publish_reads_committed_record_across_busy_space_and_draft_edits() {
    use super::{File, OpenOptions, fixture, inventory};

    let root = fixture();
    let parent = tempfile::tempdir().unwrap();
    let existing = parent.path().join("existing-output");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("operator.txt"), b"keep").unwrap();
    let deployment = prepare_record(root.path(), "arma3", true);
    let id = deployment.id.as_str();

    let mut instance = invoke(root.path(), "arma3", &["server", "instance", "show", id], 0)["data"]
        ["instance"]
        .clone();
    instance["definition"]["settings"]["hostname"] = "Changed Draft Name".into();
    let draft_file = root.path().join("changed-draft.json");
    fs::write(&draft_file, serde_json::to_vec(&instance).unwrap()).unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "instance",
            "replace",
            "--file",
            draft_file.to_str().unwrap(),
            "--expected-revision",
            "2",
        ],
        0,
    );

    let fresh_output = parent.path().join("busy-plan-output");
    let before_root = inventory(root.path());
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("games/arma3/database.lock"))
        .unwrap();
    lock.lock().unwrap();
    let plan = invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "publish",
            "plan",
            id,
            "--output",
            fresh_output.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(plan["data"]["plan"]["record_id"], deployment.record);
    assert_eq!(plan["data"]["plan"]["destination_exists"], false);
    assert_eq!(plan["data"]["database_opened"], false);
    let fp = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "--dry-run",
            "server",
            "publish",
            "apply",
            id,
            "--output",
            fresh_output.to_str().unwrap(),
            "--expected-plan-fingerprint",
            fp,
        ],
        0,
    );
    drop(lock);

    let existing_plan = invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "publish",
            "plan",
            id,
            "--output",
            existing.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(existing_plan["data"]["plan"]["destination_exists"], true);
    let existing_fp = existing_plan["data"]["plan"]["fingerprint"]
        .as_str()
        .unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "publish",
            "apply",
            id,
            "--output",
            existing.to_str().unwrap(),
            "--yes",
            "--expected-plan-fingerprint",
            existing_fp,
        ],
        2,
    );
    assert_eq!(fs::read(existing.join("operator.txt")).unwrap(), b"keep");
    assert_eq!(fs::read_dir(&existing).unwrap().count(), 1);
    invoke(
        root.path(),
        "arma3",
        &[
            "--wipe-db",
            "--yes",
            "server",
            "publish",
            "plan",
            id,
            "--output",
            fresh_output.to_str().unwrap(),
        ],
        2,
    );
    assert_eq!(inventory(root.path()), before_root);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn reforger_publishes_the_fragment_without_server_files() {
    use super::{fixture, inventory};

    let root = fixture();
    let parent = tempfile::tempdir().unwrap();
    let output = parent.path().join("player-output");
    let target = output.to_str().unwrap();
    let deployment = prepare_record(root.path(), "reforger", false);
    let id = deployment.id.as_str();
    let before = inventory(root.path());
    let plan = invoke(
        root.path(),
        "reforger",
        &["server", "publish", "plan", id, "--output", target],
        0,
    );
    assert_eq!(
        plan["data"]["plan"]["reforger"]["scenario_id"],
        "{0123456789ABCDEF}Missions/Example.conf"
    );
    assert_eq!(plan["data"]["plan"]["reforger"]["addons"], json!([]));
    assert!(inventory(parent.path()).is_empty());
    invoke(
        root.path(),
        "reforger",
        &[
            "server",
            "publish",
            "apply",
            id,
            "--output",
            target,
            "--yes",
            "--expected-plan-fingerprint",
            plan["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    assert_eq!(inventory(&output).len(), 1);
    let fragment = fs::read(output.join("reforger_addons.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fragment).unwrap(),
        json!({"game":{"mods":[]}})
    );
    invoke(
        root.path(),
        "reforger",
        &["server", "publish", "verify", id, "--output", target],
        0,
    );
    assert_eq!(inventory(root.path()), before);
}

#[cfg(windows)]
#[test]
#[ignore = "Prepares a disposable committed fixture for the real desktop publication check"]
fn prepare_player_publication_desktop_fixture() {
    let root = std::env::var_os("FOXY_D34_GUI_SCRATCH").expect("explicit new fixture directory");
    let root = Path::new(&root);
    assert!(root.is_absolute());
    fs::create_dir(root).unwrap();
    fs::write(
        root.join("games.json"),
        include_str!("../../examples/json/appdata/games.json"),
    )
    .unwrap();
    let deployment = prepare_record(root, "arma3", true);
    fs::write(root.join("publication-fixture-id.txt"), deployment.id).unwrap();
}
