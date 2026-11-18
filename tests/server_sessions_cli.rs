use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

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
    assert!(result["data"].get("database_opened") != Some(&Value::Bool(true)));
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

const SPACE: [&str; 2] = ["--game-space", "arma3"];

#[test]
fn server_session_help_lists_inventory_intents_and_fingerprint_gate() {
    let output = Command::new(env!("CARGO_BIN_EXE_Foxy"))
        .args(["server", "session", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for item in ["list", "plan", "apply"] {
        assert!(help.contains(item), "missing {item} in {help}");
    }
}

#[test]
fn apply_requires_yes_and_session_intents_reject_unsupported_game_without_writes() {
    let root = fixture();
    let instance = invoke(
        root.path(),
        &[
            "server", "instance", "add", "--name", "Main", SPACE[0], SPACE[1],
        ],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let refused = invoke(
        root.path(),
        &[
            "server",
            "session",
            "apply",
            &instance,
            "--mode",
            "configured",
            "--expected-generation",
            "0",
            "--expected-plan-fingerprint",
            "reviewed-fingerprint",
            SPACE[0],
            SPACE[1],
        ],
        2,
    );
    assert!(refused["errors"].as_array().unwrap().iter().any(|error| {
        error
            .as_str()
            .is_some_and(|error| error.contains("requires --yes"))
    }));
    assert!(!root.path().join("games/arma3/database.db").exists());

    let unsupported = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "session",
            "apply",
            &instance,
            "--mode",
            "configured",
            "--expected-generation",
            "0",
            "--expected-plan-fingerprint",
            "reviewed-fingerprint",
            SPACE[0],
            SPACE[1],
        ],
        2,
    );
    assert_eq!(unsupported["ok"], false);
    assert!(!root.path().join("games/arma3/database.db").exists());
    assert!(
        !root
            .path()
            .join("games/arma3/server/instances")
            .join(&instance)
            .join("session-intent.json")
            .exists()
    );
}
