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
    fn visit(base: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
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

fn setup(root: &Path, game: &str, supply: &str) -> (String, PathBuf) {
    let id = invoke(
        root,
        game,
        &["server", "instance", "add", "--name", "MainServer"],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let folder = root.join(format!("source-{game}"));
    fs::create_dir_all(folder.join("@mod_01/nested/empty")).unwrap();
    fs::write(folder.join("@mod_01/nested/payload.bin"), b"stage payload").unwrap();
    fs::write(folder.join("@mod_01/root.txt"), b"root data").unwrap();
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
            if supply == "download" {
                "https://repo.example.invalid/"
            } else {
                ""
            },
            "--local-path",
            local.to_str().unwrap(),
            "--supply",
            supply,
            "--addon",
            "@mod_01",
        ],
        0,
    );
    let bindings = root.join(format!("bindings-{game}.json"));
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"required","order":4}]"#,
    )
    .unwrap();
    let reviewed = invoke(
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
    let fp = reviewed["data"]["plan"]["fingerprint"].as_str().unwrap();
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
            fp,
        ],
        0,
    );
    (id, folder)
}

fn plan(root: &Path, game: &str, id: &str, code: i32) -> Value {
    invoke(
        root,
        game,
        &["server", "content", "stage", "plan", id],
        code,
    )
}
fn apply(root: &Path, game: &str, id: &str, fp: &str, code: i32) -> Value {
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
            fp,
        ],
        code,
    )
}
fn stage_path(root: &Path, game: &str, id: &str, stage: &str) -> PathBuf {
    root.join(format!(
        "games/{game}/server/instances/{id}/content-stages/{stage}"
    ))
}

#[test]
fn plan_apply_verify_and_reuse_are_exact_for_both_game_spaces() {
    let root = fixture();
    for game in ["arma3", "reforger"] {
        let (id, _folder) = setup(root.path(), game, "already-present");
        let before = inventory(root.path());
        let planned = plan(root.path(), game, &id, 0);
        let data = &planned["data"]["plan"];
        let stage_id = data["stage_id"].as_str().unwrap();
        let fingerprint = data["fingerprint"].as_str().unwrap();
        assert!(
            data["manifest"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["path"] == "addons/@mod_01/nested/empty")
        );
        assert_eq!(
            data["manifest"]["bindings"]["bindings"][0]["binding"]["role"],
            "required"
        );
        assert_eq!(planned["data"]["writes_performed"], false);
        assert_eq!(planned["data"]["downloads_started"], false);
        assert_eq!(inventory(root.path()), before);
        assert!(
            !root
                .path()
                .join(format!("games/{game}/database.db"))
                .exists()
        );
        let dry = invoke(
            root.path(),
            game,
            &[
                "--dry-run",
                "server",
                "content",
                "stage",
                "apply",
                &id,
                "--expected-plan-fingerprint",
                fingerprint,
            ],
            0,
        );
        assert_eq!(dry["data"]["writes_performed"], false);
        assert_eq!(inventory(root.path()), before);
        let applied = apply(root.path(), game, &id, fingerprint, 0);
        let report = &applied["data"]["stage"];
        assert_eq!(report["stage_id"], stage_id);
        assert_eq!(report["verified"], true);
        assert_eq!(report["published"], true);
        assert_eq!(report["deployment_ready"], false);
        assert_eq!(report["active_deployment_changed"], false);
        assert_eq!(report["downloads_started"], false);
        let directory = stage_path(root.path(), game, &id, stage_id);
        assert_eq!(
            fs::read(directory.join("addons/@mod_01/nested/payload.bin")).unwrap(),
            b"stage payload"
        );
        assert!(directory.join("addons/@mod_01/nested/empty").is_dir());
        assert!(directory.join("manifest.json").is_file());
        let listed = invoke(
            root.path(),
            game,
            &["server", "content", "stage", "list", &id],
            0,
        );
        assert_eq!(listed["data"]["stages"]["stage_ids"][0], stage_id);
        assert_eq!(listed["data"]["stages"]["integrity_verified"], false);
        let verified = invoke(
            root.path(),
            game,
            &["server", "content", "stage", "verify", &id, stage_id],
            0,
        );
        assert_eq!(verified["data"]["stage"]["verified"], true);
        assert_eq!(verified["data"]["writes_performed"], false);
        let reused = apply(root.path(), game, &id, fingerprint, 0);
        assert_eq!(reused["data"]["stage"]["published"], false);
        assert_eq!(reused["data"]["stage"]["reused_existing"], true);
    }
}

#[test]
fn stale_sources_and_corrupt_retained_stage_are_preserved() {
    let root = fixture();
    let game = "arma3";
    let (id, folder) = setup(root.path(), game, "already-present");
    let initial = plan(root.path(), game, &id, 0);
    let manifest = &initial["data"]["plan"];
    let old_id = manifest["stage_id"].as_str().unwrap().to_owned();
    let old_fp = manifest["fingerprint"].as_str().unwrap().to_owned();
    apply(root.path(), game, &id, &old_fp, 0);
    let payload = folder.join("@mod_01/nested/payload.bin");
    fs::write(&payload, b"changed source bytes").unwrap();
    let changed = inventory(root.path());
    apply(root.path(), game, &id, &old_fp, 2);
    assert_eq!(inventory(root.path()), changed);
    let fresh = plan(root.path(), game, &id, 0);
    let fresh_id = fresh["data"]["plan"]["stage_id"].as_str().unwrap();
    assert_ne!(fresh_id, old_id);
    let old_path = stage_path(root.path(), game, &id, &old_id);
    assert!(old_path.is_dir());
    apply(
        root.path(),
        game,
        &id,
        fresh["data"]["plan"]["fingerprint"].as_str().unwrap(),
        0,
    );
    assert!(old_path.is_dir());
    let damaged = old_path.join("addons/@mod_01/root.txt");
    let bytes = fs::read(&damaged).unwrap();
    fs::write(&damaged, b"tampered").unwrap();
    let corrupt = inventory(root.path());
    invoke(
        root.path(),
        game,
        &["server", "content", "stage", "verify", &id, &old_id],
        4,
    );
    assert_eq!(inventory(root.path()), corrupt);
    let current = plan(root.path(), game, &id, 0);
    let current_fp = current["data"]["plan"]["fingerprint"].as_str().unwrap();
    apply(root.path(), game, &id, current_fp, 0);
    assert_eq!(fs::read(&damaged).unwrap(), b"tampered");
    fs::write(damaged, bytes).unwrap();
    let _ = inventory(root.path());
}

#[test]
fn downloads_and_busy_claim_block_only_actual_staging() {
    let root = fixture();
    let game = "reforger";
    let (id, _folder) = setup(root.path(), game, "download");
    let before = inventory(root.path());
    invoke(
        root.path(),
        game,
        &["server", "content", "stage", "plan", &id],
        2,
    );
    assert_eq!(inventory(root.path()), before);

    let (id, _folder) = setup(root.path(), "arma3", "already-present");
    let planned = plan(root.path(), "arma3", &id, 0);
    let fp = planned["data"]["plan"]["fingerprint"].as_str().unwrap();
    apply(root.path(), "arma3", &id, fp, 0);
    let listed = invoke(
        root.path(),
        "arma3",
        &["server", "content", "stage", "list", &id],
        0,
    );
    let stage_id = listed["data"]["stages"]["stage_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let verify_args = ["server", "content", "stage", "verify", &id, &stage_id];
    let inventory_before = inventory(root.path());
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("games/arma3/database.lock"))
        .unwrap();
    lock.lock().unwrap();
    plan(root.path(), "arma3", &id, 0);
    invoke(
        root.path(),
        "arma3",
        &["server", "content", "stage", "list", &id],
        0,
    );
    invoke(root.path(), "arma3", &verify_args, 0);
    let dry = invoke(
        root.path(),
        "arma3",
        &[
            "--dry-run",
            "server",
            "content",
            "stage",
            "apply",
            &id,
            "--expected-plan-fingerprint",
            fp,
        ],
        0,
    );
    assert_eq!(dry["data"]["writes_performed"], false);
    apply(root.path(), "arma3", &id, fp, 6);
    drop(lock);
    assert_eq!(inventory(root.path()), inventory_before);
}
