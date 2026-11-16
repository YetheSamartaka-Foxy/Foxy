#![cfg(any(windows, target_os = "linux"))]

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

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
        "{args:?}\n{}\n{}",
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
    fn visit(base: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for item in fs::read_dir(path).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                out.insert(path.strip_prefix(base).unwrap().to_path_buf(), Vec::new());
                visit(base, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

struct Prepared {
    id: String,
    stage_id: String,
    stage_dir: PathBuf,
    config_dir: PathBuf,
}

fn prepare(root: &Path, game: &str, with_config: bool, with_stage: bool) -> Prepared {
    let created = invoke(
        root,
        game,
        &["server", "instance", "add", "--name", "Fixture"],
        0,
    );
    let mut instance = created["data"]["instance"].clone();
    let id = instance["id"].as_str().unwrap().to_owned();
    if game == "arma3" {
        instance["definition"]["settings"]["mission_cycle"] = serde_json::json!([
            {"template":"Mission.Altis","difficulty":"regular"}
        ]);
    }
    let draft = root.join(format!("draft-{game}.json"));
    fs::write(&draft, serde_json::to_vec(&instance).unwrap()).unwrap();
    invoke(
        root,
        game,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            draft.to_str().unwrap(),
            "--expected-revision",
            "1",
        ],
        0,
    );
    let folder = root.join(format!("source-{game}"));
    fs::create_dir_all(folder.join("@mod_01/mpmissions")).unwrap();
    fs::write(
        folder.join("@mod_01/mpmissions/Mission.Altis.pbo"),
        b"mission bytes",
    )
    .unwrap();
    fs::create_dir_all(folder.join("@mod_01/keys")).unwrap();
    fs::write(folder.join("@mod_01/keys/server.bikey"), b"key bytes").unwrap();
    invoke(
        root,
        game,
        &[
            "server",
            "content",
            "add-folder",
            "--id",
            "RepoAlpha",
            "--path",
            folder.to_str().unwrap(),
        ],
        0,
    );
    let local = root.join(format!("client-{game}"));
    invoke(
        root,
        game,
        &[
            "server",
            "content",
            "add-profile",
            "--name",
            "RepoAlpha",
            "--folder",
            "RepoAlpha",
            "--url",
            "",
            "--local-path",
            local.to_str().unwrap(),
            "--supply",
            "already-present",
            "--addon",
            "@mod_01",
        ],
        0,
    );
    let binding_file = root.join(format!("bindings-{game}.json"));
    fs::write(
        &binding_file,
        br#"[{"source_id":"RepoAlpha","role":"required","order":0}]"#,
    )
    .unwrap();
    let bindings = invoke(
        root,
        game,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            &id,
            "--bindings",
            binding_file.to_str().unwrap(),
        ],
        0,
    );
    invoke(
        root,
        game,
        &[
            "server",
            "content",
            "bindings",
            "save",
            &id,
            "--bindings",
            binding_file.to_str().unwrap(),
            "--expected-plan-fingerprint",
            bindings["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );

    let mut config_dir = PathBuf::new();
    if with_config {
        let plan = invoke(root, game, &["server", "configuration", "plan", &id], 0);
        let data = &plan["data"];
        invoke(
            root,
            game,
            &[
                "server",
                "configuration",
                "commit",
                &id,
                "--expected-revision",
                data["preview"]["instance_revision"]
                    .as_u64()
                    .unwrap()
                    .to_string()
                    .as_str(),
                "--expected-generation",
                data["expected_generation"]
                    .as_u64()
                    .unwrap()
                    .to_string()
                    .as_str(),
                "--expected-fingerprint",
                data["preview"]["fingerprint"].as_str().unwrap(),
            ],
            0,
        );
        config_dir = root.join(format!(
            "games/{game}/server/instances/{id}/configuration/revisions/{}",
            data["revision_id"].as_str().unwrap()
        ));
    }
    let mut stage_id = String::new();
    let mut stage_dir = PathBuf::new();
    if with_stage {
        let plan = invoke(root, game, &["server", "content", "stage", "plan", &id], 0);
        let data = &plan["data"]["plan"];
        stage_id = data["stage_id"].as_str().unwrap().to_owned();
        stage_dir = root.join(format!(
            "games/{game}/server/instances/{id}/content-stages/{stage_id}"
        ));
        invoke(
            root,
            game,
            &[
                "server",
                "content",
                "stage",
                "apply",
                &id,
                "--expected-plan-fingerprint",
                data["fingerprint"].as_str().unwrap(),
            ],
            0,
        );
    }
    Prepared {
        id,
        stage_id,
        stage_dir,
        config_dir,
    }
}

fn deploy(
    root: &Path,
    game: &str,
    id: &str,
    stage: Option<&str>,
    expected: (Option<&str>, Option<&str>),
    dry: bool,
    code: i32,
) -> Value {
    let mut args = Vec::new();
    if dry {
        args.push("--dry-run");
    }
    args.extend(["server", "deploy", "plan", id]);
    if let Some(stage) = stage {
        args.extend(["--stage", stage]);
    }
    if let Some(revision) = expected.0 {
        args.extend(["--expected-revision", revision]);
    }
    if let Some(generation) = expected.1 {
        args.extend(["--expected-configuration-generation", generation]);
    }
    invoke(root, game, &args, code)
}

#[test]
fn review_is_stable_read_only_and_blocked_for_both_games() {
    let root = fixture();
    for game in ["arma3", "reforger"] {
        let prepared = prepare(root.path(), game, true, true);
        let before = inventory(root.path());
        let first = deploy(
            root.path(),
            game,
            &prepared.id,
            Some(&prepared.stage_id),
            (Some("2"), Some("1")),
            false,
            0,
        );
        let plan = &first["data"]["plan"];
        assert_eq!(first["action"], "server.deploy.plan");
        assert_eq!(first["data"]["database_opened"], false);
        assert_eq!(first["data"]["writes_performed"], false);
        assert!(!plan["deployment_ready"].as_bool().unwrap());
        assert!(!plan["active_deployment_changed"].as_bool().unwrap());
        assert!(!plan["downloads_started"].as_bool().unwrap());
        assert!(!plan["installation_leases_acquired"].as_bool().unwrap());
        assert!(!plan["process_started"].as_bool().unwrap());
        assert!(!plan["blockers"].as_array().unwrap().is_empty());
        if game == "arma3" {
            assert_eq!(plan["artifacts"]["key_files"], 1);
            assert_eq!(plan["artifacts"]["mission_files"], 1);
        } else {
            assert!(plan["artifacts"]["keys"].as_array().unwrap().is_empty());
            assert!(plan["artifacts"]["missions"].as_array().unwrap().is_empty());
        }
        let repeated = deploy(
            root.path(),
            game,
            &prepared.id,
            Some(&prepared.stage_id),
            (Some("2"), Some("1")),
            false,
            0,
        );
        let dry = deploy(
            root.path(),
            game,
            &prepared.id,
            Some(&prepared.stage_id),
            (Some("2"), Some("1")),
            true,
            0,
        );
        assert_eq!(repeated["data"]["plan"]["fingerprint"], plan["fingerprint"]);
        assert_eq!(dry["data"]["plan"]["fingerprint"], plan["fingerprint"]);
        assert_eq!(inventory(root.path()), before);
        assert!(
            !root
                .path()
                .join(format!("games/{game}/database.db"))
                .exists()
        );
    }
}

#[test]
fn missing_and_corrupt_retained_inputs_are_completed_blocked_reviews() {
    let root = fixture();
    let prepared = prepare(root.path(), "arma3", false, false);
    let before = inventory(root.path());
    let missing = deploy(
        root.path(),
        "arma3",
        &prepared.id,
        None,
        (None, None),
        false,
        0,
    );
    assert!(
        missing["data"]["plan"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "configuration_missing")
    );
    assert!(
        missing["data"]["plan"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "content_missing")
    );
    assert_eq!(inventory(root.path()), before);

    let prepared = prepare(root.path(), "reforger", true, true);
    fs::write(
        prepared.config_dir.join("server.json"),
        b"damaged configuration",
    )
    .unwrap();
    fs::write(
        prepared.stage_dir.join("addons/@mod_01/keys/server.bikey"),
        b"damaged stage",
    )
    .unwrap();
    let before = inventory(root.path());
    let report = deploy(
        root.path(),
        "reforger",
        &prepared.id,
        Some(&prepared.stage_id),
        (Some("2"), Some("1")),
        false,
        0,
    );
    assert!(
        !report["data"]["plan"]["configuration"]["integrity_verified"]
            .as_bool()
            .unwrap()
    );
    assert!(
        !report["data"]["plan"]["content"]["integrity_verified"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(inventory(root.path()), before);
    assert_eq!(
        deploy(
            root.path(),
            "reforger",
            &prepared.id,
            Some("stage_bad"),
            (None, None),
            false,
            2
        )["ok"],
        false
    );
    let unchanged = inventory(root.path());
    assert_eq!(
        deploy(
            root.path(),
            "reforger",
            &prepared.id,
            Some(&prepared.stage_id),
            (Some("99"), None),
            false,
            2
        )["ok"],
        false
    );
    assert_eq!(inventory(root.path()), unchanged);
    assert_eq!(
        deploy(
            root.path(),
            "reforger",
            &prepared.id,
            Some(&prepared.stage_id),
            (None, Some("99")),
            false,
            2
        )["ok"],
        false
    );
    assert_eq!(inventory(root.path()), unchanged);
}

#[test]
fn busy_claim_allows_read_only_review_and_dry_run_but_wipe_is_rejected() {
    let root = fixture();
    let prepared = prepare(root.path(), "arma3", true, true);
    let before = inventory(root.path());
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("games/arma3/database.lock"))
        .unwrap();
    lock.lock().unwrap();
    let plan = deploy(
        root.path(),
        "arma3",
        &prepared.id,
        Some(&prepared.stage_id),
        (Some("2"), Some("1")),
        false,
        0,
    );
    let dry = deploy(
        root.path(),
        "arma3",
        &prepared.id,
        Some(&prepared.stage_id),
        (None, None),
        true,
        0,
    );
    assert_eq!(
        dry["data"]["plan"]["fingerprint"],
        plan["data"]["plan"]["fingerprint"]
    );
    invoke(
        root.path(),
        "arma3",
        &[
            "--wipe-db",
            "--yes",
            "server",
            "deploy",
            "plan",
            &prepared.id,
        ],
        2,
    );
    drop(lock);
    assert_eq!(inventory(root.path()), before);
}
