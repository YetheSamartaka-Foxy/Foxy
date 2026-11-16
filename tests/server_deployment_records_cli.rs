use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
#[path = "server_deployment_records_cli/preparation.rs"]
mod preparation;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn invoke(root: &Path, game: &str, args: &[&str], code: i32) -> Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    cmd.arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .args(["--game-space", game]);
    cmd.env("FOXY_CONFIG_DIR", root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let output = cmd.output().unwrap();
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
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for item in fs::read_dir(dir).unwrap() {
            let p = item.unwrap().path();
            if p.is_dir() {
                out.insert(p.strip_prefix(base).unwrap().into(), Vec::new());
                visit(base, &p, out);
            } else {
                out.insert(p.strip_prefix(base).unwrap().into(), fs::read(p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

struct Prepared {
    id: String,
    stage: String,
    record: String,
}
fn prepare(root: &Path, game: &str) -> Prepared {
    let id = invoke(
        root,
        game,
        &["server", "instance", "add", "--name", "Fixture"],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let shown = invoke(root, game, &["server", "instance", "show", &id], 0);
    let mut instance = shown["data"]["instance"].clone();
    if game == "arma3" {
        instance["definition"]["settings"]["mission_cycle"] = serde_json::json!([
            {"template":"Mission.Altis","difficulty":"regular"}
        ]);
    } else {
        instance["definition"]["settings"]["scenario_id"] =
            "{0123456789ABCDEF}Missions/Example.conf".into();
    }
    let instance_file = root.join(format!("instance-{game}.json"));
    fs::write(&instance_file, serde_json::to_vec(&instance).unwrap()).unwrap();
    invoke(
        root,
        game,
        &[
            "server",
            "instance",
            "replace",
            "--file",
            instance_file.to_str().unwrap(),
            "--expected-revision",
            "1",
        ],
        0,
    );
    let folder = root.join(format!("source-{game}"));
    fs::create_dir_all(folder.join("@mod_01/mpmissions")).unwrap();
    fs::write(
        folder.join("@mod_01/mpmissions/Mission.Altis.pbo"),
        b"mission",
    )
    .unwrap();
    fs::create_dir_all(folder.join("@mod_01/keys")).unwrap();
    fs::write(folder.join("@mod_01/keys/server.bikey"), b"key").unwrap();
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
    let bindings = root.join(format!("bindings-{game}.json"));
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"required","order":0}]"#,
    )
    .unwrap();
    let plan = invoke(
        root,
        game,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            &id,
            "--bindings",
            bindings.to_str().unwrap(),
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
            bindings.to_str().unwrap(),
            "--expected-plan-fingerprint",
            plan["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    let plan = invoke(root, game, &["server", "content", "stage", "plan", &id], 0);
    let stage = plan["data"]["plan"]["stage_id"]
        .as_str()
        .unwrap()
        .to_owned();
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
            plan["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    let config = invoke(root, game, &["server", "configuration", "plan", &id], 0);
    let data = &config["data"];
    let rev = data["preview"]["instance_revision"]
        .as_u64()
        .unwrap()
        .to_string();
    let generation = data["expected_generation"].as_u64().unwrap().to_string();
    invoke(
        root,
        game,
        &[
            "server",
            "configuration",
            "commit",
            &id,
            "--expected-revision",
            &rev,
            "--expected-generation",
            &generation,
            "--expected-fingerprint",
            data["preview"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    let prepared = invoke(
        root,
        game,
        &[
            "server",
            "deploy",
            "prepare",
            &id,
            "--stage",
            &stage,
            "--expected-revision",
            "2",
            "--expected-configuration-generation",
            "1",
            "--expected-deployment-generation",
            "0",
        ],
        0,
    );
    let record = prepared["data"]["plan"]["record_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fp = prepared["data"]["plan"]["fingerprint"].as_str().unwrap();
    invoke(
        root,
        game,
        &[
            "server",
            "deploy",
            "commit",
            &id,
            "--stage",
            &stage,
            "--expected-revision",
            "2",
            "--expected-configuration-generation",
            "1",
            "--expected-deployment-generation",
            "0",
            "--expected-plan-fingerprint",
            fp,
        ],
        0,
    );
    Prepared { id, stage, record }
}

fn add_second_record(root: &Path, game: &str, id: &str) -> String {
    let payload = root.join(format!(
        "source-{game}/@mod_01/mpmissions/Mission.Altis.pbo"
    ));
    fs::write(payload, b"second immutable mission bytes").unwrap();
    let stage_plan = invoke(root, game, &["server", "content", "stage", "plan", id], 0);
    let stage = stage_plan["data"]["plan"]["stage_id"]
        .as_str()
        .unwrap()
        .to_owned();
    invoke(
        root,
        game,
        &[
            "server",
            "content",
            "stage",
            "apply",
            id,
            "--expected-plan-fingerprint",
            stage_plan["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    let prepared = invoke(
        root,
        game,
        &[
            "server",
            "deploy",
            "prepare",
            id,
            "--stage",
            &stage,
            "--expected-revision",
            "2",
            "--expected-configuration-generation",
            "1",
            "--expected-deployment-generation",
            "1",
        ],
        0,
    );
    let record = prepared["data"]["plan"]["record_id"]
        .as_str()
        .unwrap()
        .to_owned();
    invoke(
        root,
        game,
        &[
            "server",
            "deploy",
            "commit",
            id,
            "--stage",
            &stage,
            "--expected-revision",
            "2",
            "--expected-configuration-generation",
            "1",
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            prepared["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    record
}

fn selection_plan(
    root: &Path,
    game: &str,
    id: &str,
    target: &str,
    generation: &str,
    code: i32,
) -> Value {
    invoke(
        root,
        game,
        &[
            "server",
            "deploy",
            "select-plan",
            id,
            target,
            "--expected-deployment-generation",
            generation,
        ],
        code,
    )
}

fn select(
    root: &Path,
    game: &str,
    id: &str,
    target: &str,
    guard: (&str, &str),
    flags: &[&str],
    code: i32,
) -> Value {
    let mut args = flags.to_vec();
    args.extend([
        "server",
        "deploy",
        "select",
        id,
        target,
        "--expected-deployment-generation",
        guard.0,
        "--expected-plan-fingerprint",
        guard.1,
    ]);
    invoke(root, game, &args, code)
}

#[test]
fn records_prepare_commit_list_verify_and_dry_run_are_read_only_or_additive() {
    let root = fixture();
    for game in ["arma3", "reforger"] {
        let prepared = prepare(root.path(), game);
        let before = inventory(root.path());
        let listed = invoke(
            root.path(),
            game,
            &["server", "deploy", "list", &prepared.id],
            0,
        );
        assert_eq!(listed["data"]["state"]["selected"], prepared.record);
        assert_eq!(listed["data"]["state"]["generation"], 1);
        let verified = invoke(
            root.path(),
            game,
            &["server", "deploy", "verify", &prepared.id, &prepared.record],
            0,
        );
        assert_eq!(verified["data"]["verification"]["integrity_verified"], true);
        assert_eq!(verified["data"]["verification"]["deployment_ready"], false);
        let plan = invoke(
            root.path(),
            game,
            &[
                "server",
                "deploy",
                "prepare",
                &prepared.id,
                "--stage",
                &prepared.stage,
                "--expected-deployment-generation",
                "1",
            ],
            0,
        );
        assert_eq!(plan["data"]["plan"]["record_id"], prepared.record);
        let fp = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
        let dry = invoke(
            root.path(),
            game,
            &[
                "--dry-run",
                "server",
                "deploy",
                "commit",
                &prepared.id,
                "--stage",
                &prepared.stage,
                "--expected-deployment-generation",
                "1",
                "--expected-plan-fingerprint",
                fp,
            ],
            0,
        );
        assert_eq!(dry["data"]["writes_performed"], false);
        assert_eq!(inventory(root.path()), before);
        let committed = invoke(
            root.path(),
            game,
            &[
                "server",
                "deploy",
                "commit",
                &prepared.id,
                "--stage",
                &prepared.stage,
                "--expected-deployment-generation",
                "1",
                "--expected-plan-fingerprint",
                fp,
            ],
            0,
        );
        assert_eq!(committed["data"]["commit"]["state"]["generation"], 1);
        assert_eq!(committed["data"]["commit"]["selection_changed"], false);
        assert_eq!(
            committed["data"]["commit"]["active_deployment_changed"],
            false
        );
        assert!(inventory(root.path()).len() >= before.len());
        assert!(
            !root
                .path()
                .join(format!("games/{game}/database.db"))
                .exists()
        );
    }
}

#[test]
fn stale_and_corrupt_record_requests_preserve_selection_and_bytes() {
    let root = fixture();
    let prepared = prepare(root.path(), "arma3");
    let before = inventory(root.path());
    for args in [
        vec![
            "server",
            "deploy",
            "prepare",
            &prepared.id,
            "--stage",
            &prepared.stage,
            "--expected-deployment-generation",
            "0",
        ],
        vec![
            "server",
            "deploy",
            "commit",
            &prepared.id,
            "--stage",
            &prepared.stage,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            "stale",
        ],
    ] {
        invoke(root.path(), "arma3", &args, 2);
        assert_eq!(inventory(root.path()), before);
    }
    let record_dir = root.path().join(format!(
        "games/arma3/server/instances/{}/deployments/records/{}",
        prepared.id, prepared.record
    ));
    fs::write(record_dir.join("operator-data"), b"preserve").unwrap();
    let altered = inventory(root.path());
    invoke(
        root.path(),
        "arma3",
        &["server", "deploy", "verify", &prepared.id, &prepared.record],
        4,
    );
    assert_eq!(inventory(root.path()), altered);
}

#[test]
fn busy_claim_allows_list_verify_and_dry_run_but_blocks_commit() {
    let root = fixture();
    let prepared = prepare(root.path(), "arma3");
    let before = inventory(root.path());
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("games/arma3/database.lock"))
        .unwrap();
    lock.lock().unwrap();
    invoke(
        root.path(),
        "arma3",
        &["server", "deploy", "list", &prepared.id],
        0,
    );
    invoke(
        root.path(),
        "arma3",
        &["server", "deploy", "verify", &prepared.id, &prepared.record],
        0,
    );
    let plan = invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "prepare",
            &prepared.id,
            "--stage",
            &prepared.stage,
            "--expected-deployment-generation",
            "1",
        ],
        0,
    );
    let fp = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "--dry-run",
            "server",
            "deploy",
            "commit",
            &prepared.id,
            "--stage",
            &prepared.stage,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            fp,
        ],
        0,
    );
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "commit",
            &prepared.id,
            "--stage",
            &prepared.stage,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            fp,
        ],
        6,
    );
    drop(lock);
    assert_eq!(inventory(root.path()), before);
}

#[test]
fn selection_plan_dry_run_and_confirmed_select_change_only_index_for_both_games() {
    let root = fixture();
    for game in ["arma3", "reforger"] {
        let prepared = prepare(root.path(), game);
        let older = prepared.record.clone();
        let newer = add_second_record(root.path(), game, &prepared.id);
        let before = inventory(root.path());
        let plan = selection_plan(root.path(), game, &prepared.id, &older, "2", 0);
        assert_eq!(plan["data"]["plan"]["previous_record_id"], newer);
        assert_eq!(plan["data"]["plan"]["target"]["record_id"], older);
        assert_eq!(plan["data"]["writes_performed"], false);
        assert_eq!(inventory(root.path()), before);
        let fingerprint = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
        let dry = select(
            root.path(),
            game,
            &prepared.id,
            &older,
            ("2", fingerprint),
            &["--dry-run"],
            0,
        );
        assert_eq!(dry["data"]["writes_performed"], false);
        assert_eq!(inventory(root.path()), before);
        select(
            root.path(),
            game,
            &prepared.id,
            &older,
            ("2", fingerprint),
            &["--yes"],
            0,
        );
        let after = inventory(root.path());
        assert_eq!(after.len(), before.len());
        let state_path = PathBuf::from(format!(
            "games/{game}/server/instances/{}/deployments/state.json",
            prepared.id
        ));
        let owner_path = PathBuf::from(format!("games/{game}/database.owner"));
        for (path, bytes) in &before {
            if path == &state_path {
                assert_ne!(after.get(path), Some(bytes));
            } else if path == &owner_path {
                let owner = std::str::from_utf8(after.get(path).unwrap()).unwrap();
                assert!(owner.parse::<u32>().unwrap() > 0);
            } else {
                assert_eq!(
                    after.get(path),
                    Some(bytes),
                    "unexpected file change {path:?}"
                );
            }
        }
        let listed = invoke(
            root.path(),
            game,
            &["server", "deploy", "list", &prepared.id],
            0,
        );
        assert_eq!(listed["data"]["state"]["selected"], older);
        assert_eq!(listed["data"]["state"]["generation"], 3);
        assert_eq!(
            invoke(
                root.path(),
                game,
                &["server", "deploy", "verify", &prepared.id, &older],
                0
            )["data"]["verification"]["integrity_verified"],
            true
        );
        let noop = selection_plan(root.path(), game, &prepared.id, &older, "3", 0);
        let mut bytes = inventory(root.path());
        bytes.remove(&owner_path);
        let noop = select(
            root.path(),
            game,
            &prepared.id,
            &older,
            ("3", noop["data"]["plan"]["fingerprint"].as_str().unwrap()),
            &["--yes"],
            0,
        );
        assert_eq!(noop["data"]["writes_performed"], false);
        let mut after_noop = inventory(root.path());
        after_noop.remove(&owner_path);
        assert_eq!(after_noop, bytes);
    }
}

#[test]
fn selection_enforces_generation_fingerprint_yes_and_busy_claim() {
    let root = fixture();
    let prepared = prepare(root.path(), "arma3");
    let old = prepared.record.clone();
    add_second_record(root.path(), "arma3", &prepared.id);
    let before = inventory(root.path());
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "select-plan",
            &prepared.id,
            &old,
            "--expected-deployment-generation",
            "1",
        ],
        2,
    );
    assert_eq!(inventory(root.path()), before);
    let plan = selection_plan(root.path(), "arma3", &prepared.id, &old, "2", 0);
    let fp = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    select(root.path(), "arma3", &prepared.id, &old, ("2", fp), &[], 2);
    assert_eq!(inventory(root.path()), before);
    select(
        root.path(),
        "arma3",
        &prepared.id,
        &old,
        ("2", "stale"),
        &["--yes"],
        2,
    );
    assert_eq!(inventory(root.path()), before);

    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("games/arma3/database.lock"))
        .unwrap();
    lock.lock().unwrap();
    selection_plan(root.path(), "arma3", &prepared.id, &old, "2", 0);
    let dry = select(
        root.path(),
        "arma3",
        &prepared.id,
        &old,
        ("2", fp),
        &["--dry-run"],
        0,
    );
    assert_eq!(dry["data"]["writes_performed"], false);
    select(
        root.path(),
        "arma3",
        &prepared.id,
        &old,
        ("2", fp),
        &["--yes"],
        6,
    );
    drop(lock);
    assert_eq!(inventory(root.path()), before);

    let record_dir = root.path().join(format!(
        "games/arma3/server/instances/{}/deployments/records/{old}",
        prepared.id
    ));
    fs::write(record_dir.join("extra"), b"keep").unwrap();
    let corrupted = inventory(root.path());
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "select-plan",
            &prepared.id,
            &old,
            "--expected-deployment-generation",
            "2",
        ],
        4,
    );
    assert_eq!(inventory(root.path()), corrupted);
}
