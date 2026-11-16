use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn invoke(root: &Path, args: &[&str], expected_code: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args);
    command.env("FOXY_CONFIG_DIR", root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "args={args:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], expected_code == 0);
    assert_eq!(result.as_object().unwrap().len(), 4);
    if expected_code != 0 {
        assert!(result["data"].is_null());
    }
    result
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

fn inventory(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();
    if !root.exists() {
        return result;
    }
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.insert(PathBuf::from(path.file_name().unwrap()), None);
            for (nested, bytes) in inventory(&path) {
                result.insert(PathBuf::from(path.file_name().unwrap()).join(nested), bytes);
            }
        } else {
            result.insert(
                PathBuf::from(path.file_name().unwrap()),
                Some(fs::read(path).unwrap()),
            );
        }
    }
    result
}

#[test]
fn server_draft_inspection_and_dry_run_leave_every_file_unchanged() {
    let root = fixture();
    let before = inventory(root.path());
    for args in [
        vec!["server", "capabilities", "--game-space", "reforger"],
        vec!["server", "instance", "list", "--game-space", "reforger"],
        vec![
            "--dry-run",
            "server",
            "instance",
            "add",
            "--game-space",
            "reforger",
            "--name",
            "MainServer",
        ],
    ] {
        invoke(root.path(), &args, 0);
        assert_eq!(inventory(root.path()), before);
    }
    invoke(
        root.path(),
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            "generic",
            "--name",
            "MainServer",
        ],
        2,
    );
    invoke(
        root.path(),
        &["--wipe-db", "--yes", "server", "instance", "list"],
        2,
    );
    invoke(root.path(), &["server", "content", "list"], 0);
    for args in [
        vec!["--wipe-db", "--yes", "server", "context"],
        vec!["--wipe-db", "--yes", "server", "content", "list"],
    ] {
        invoke(root.path(), &args, 2);
    }
    invoke(
        root.path(),
        &["server", "instance", "add", "--name", "\u{1b}Server"],
        2,
    );
    assert_eq!(inventory(root.path()), before);
    let missing = root.path().join("missing");
    invoke(
        &missing,
        &[
            "--dry-run",
            "server",
            "instance",
            "add",
            "--name",
            "MainServer",
        ],
        2,
    );
    assert!(!missing.exists());
}

#[test]
fn server_draft_cli_shares_revision_checked_edit_flows_across_both_games() {
    let root = fixture();
    let registry = fs::read(root.path().join("games.json")).unwrap();
    for space in ["reforger", "arma3"] {
        let created = invoke(
            root.path(),
            &[
                "server",
                "instance",
                "add",
                "--game-space",
                space,
                "--name",
                "MainServer",
            ],
            0,
        );
        let id = created["data"]["instance"]["id"].as_str().unwrap();
        assert_eq!(created["data"]["instance"]["definition"]["game"], space);
        assert!(
            !root
                .path()
                .join(format!("games/{space}/database.db"))
                .exists()
        );
        if space == "reforger" {
            assert!(!root.path().join("games/arma3").exists());
        }
        let shown = invoke(
            root.path(),
            &["server", "instance", "show", id, "--game-space", space],
            0,
        );
        let mut instance = shown["data"]["instance"].clone();
        instance["display_name"] = "EditedServer".into();
        let input = root.path().join("draft.json");
        fs::write(&input, serde_json::to_vec_pretty(&instance).unwrap()).unwrap();
        let before_validate = inventory(root.path());
        invoke(
            root.path(),
            &[
                "server",
                "instance",
                "validate",
                "--game-space",
                space,
                "--file",
                input.to_str().unwrap(),
            ],
            0,
        );
        assert_eq!(inventory(root.path()), before_validate);
        invoke(
            root.path(),
            &[
                "server",
                "instance",
                "replace",
                "--game-space",
                space,
                "--file",
                input.to_str().unwrap(),
                "--expected-revision",
                "1",
            ],
            0,
        );
        let before_stale = inventory(root.path());
        invoke(
            root.path(),
            &[
                "server",
                "instance",
                "replace",
                "--game-space",
                space,
                "--file",
                input.to_str().unwrap(),
                "--expected-revision",
                "1",
            ],
            2,
        );
        assert_eq!(inventory(root.path()), before_stale);
        invoke(
            root.path(),
            &[
                "server",
                "instance",
                "rename",
                id,
                "--game-space",
                space,
                "--name",
                "TrainingServer",
                "--expected-revision",
                "2",
            ],
            0,
        );
        let copied = invoke(
            root.path(),
            &[
                "server",
                "instance",
                "clone",
                id,
                "--game-space",
                space,
                "--name",
                "SecondServer",
                "--expected-revision",
                "3",
            ],
            0,
        );
        let copied_id = copied["data"]["instance"]["id"].as_str().unwrap();
        assert_ne!(id, copied_id);
        let before_remove = inventory(root.path());
        invoke(
            root.path(),
            &[
                "--dry-run",
                "server",
                "instance",
                "remove",
                copied_id,
                "--game-space",
                space,
                "--expected-revision",
                "1",
            ],
            0,
        );
        invoke(
            root.path(),
            &[
                "server",
                "instance",
                "remove",
                copied_id,
                "--game-space",
                space,
                "--expected-revision",
                "1",
            ],
            2,
        );
        assert_eq!(inventory(root.path()), before_remove);
        invoke(
            root.path(),
            &[
                "--yes",
                "server",
                "instance",
                "remove",
                copied_id,
                "--game-space",
                space,
                "--expected-revision",
                "1",
            ],
            0,
        );
        invoke(
            root.path(),
            &[
                "server",
                "instance",
                "show",
                copied_id,
                "--game-space",
                space,
            ],
            3,
        );
        let listed = invoke(
            root.path(),
            &["server", "instance", "list", "--game-space", space],
            0,
        );
        assert_eq!(
            listed["data"]["catalog"]["instances"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(listed["data"]["catalog"]["instances"][0]["revision"], 3);
    }
    assert_eq!(fs::read(root.path().join("games.json")).unwrap(), registry);
    assert!(!root.path().join("logs").exists());
    assert!(!root.path().join("app_settings.json").exists());
}

#[test]
fn server_draft_cli_refuses_foreign_ownership_and_preserves_corrupt_or_future_stores() {
    let root = fixture();
    let space_dir = root.path().join("games/reforger");
    fs::create_dir_all(&space_dir).unwrap();
    let lock = fs::File::create(space_dir.join("database.lock")).unwrap();
    lock.try_lock().unwrap();
    invoke(
        root.path(),
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            "reforger",
            "--name",
            "MainServer",
        ],
        6,
    );
    invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "instance",
            "add",
            "--game-space",
            "reforger",
            "--name",
            "MainServer",
        ],
        0,
    );
    invoke(
        root.path(),
        &["server", "instance", "list", "--game-space", "reforger"],
        0,
    );
    assert!(!space_dir.join("server").exists());
    drop(lock);
    invoke(
        root.path(),
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            "reforger",
            "--name",
            "MainServer",
        ],
        0,
    );
    let path = space_dir.join("server/servers.json");
    for (bytes, code) in [
        (b"{".as_slice(), 4),
        (b"{\"schema_version\":999}".as_slice(), 2),
    ] {
        fs::write(&path, bytes).unwrap();
        let before = inventory(root.path());
        invoke(
            root.path(),
            &[
                "server",
                "instance",
                "add",
                "--game-space",
                "reforger",
                "--name",
                "MainServer",
            ],
            code,
        );
        let capabilities = invoke(
            root.path(),
            &["server", "capabilities", "--game-space", "reforger"],
            0,
        );
        assert_eq!(
            capabilities["data"]["availability"]["edit_drafts"],
            "blocked_by_store"
        );
        assert_eq!(inventory(root.path()), before);
    }
    let invalid = root.path().join("invalid.json");
    fs::write(
        &invalid,
        serde_json::to_vec(&json!({"schema_version": 1, "password": "private"})).unwrap(),
    )
    .unwrap();
    let error = invoke(
        root.path(),
        &[
            "server",
            "instance",
            "validate",
            "--game-space",
            "reforger",
            "--file",
            invalid.to_str().unwrap(),
        ],
        2,
    );
    assert!(!error.to_string().contains("private"));
}
