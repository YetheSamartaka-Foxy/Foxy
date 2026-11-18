use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn invoke(root: &Path, args: &[&str], expected: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], expected == 0, "{value}");
    value
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_bytes!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    root
}

#[test]
fn export_preview_and_dry_run_never_start_an_owner_or_write_a_definition() {
    let root = fixture();
    let before = fs::read(root.path().join("games.json")).unwrap();
    let preview = invoke(
        root.path(),
        &["server", "service", "export", "--game-space", "arma3"],
        0,
    );
    let plan = &preview["data"]["plan"];
    assert_eq!(preview["data"]["writes_performed"], false);
    assert_eq!(preview["data"]["registration_performed"], false);
    assert_eq!(preview["data"]["database_opened"], false);
    let arguments: Vec<&str> = plan["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let expected = if cfg!(windows) {
        vec![
            "server",
            "supervise",
            "--game-space",
            "arma3",
            "--background",
        ]
    } else {
        vec!["server", "supervise", "--game-space", "arma3"]
    };
    assert_eq!(&arguments[2..], expected);
    assert!(!plan["definition"].as_str().unwrap().contains("--detach"));
    #[cfg(windows)]
    assert!(
        plan["definition"]
            .as_str()
            .unwrap()
            .contains("<RunLevel>LeastPrivilege</RunLevel>")
    );
    #[cfg(target_os = "linux")]
    assert!(plan["definition"].as_str().unwrap().contains("Type=exec\n"));
    let destination = root.path().join("new-definition.txt");
    let dry = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "service",
            "export",
            "--game-space",
            "arma3",
            "--output",
            destination.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(dry["data"]["plan"]["fingerprint"], plan["fingerprint"]);
    assert_eq!(dry["data"]["writes_performed"], false);
    assert!(!destination.exists());
    assert!(!root.path().join("games/arma3/database.db").exists());
    let owner = invoke(
        root.path(),
        &["server", "owner", "--game-space", "arma3"],
        4,
    );
    assert_eq!(owner["data"], Value::Null);
    assert_eq!(fs::read(root.path().join("games.json")).unwrap(), before);
}

#[test]
fn explicit_export_writes_the_reviewed_bytes_once_and_refuses_a_foreign_host_format() {
    let root = fixture();
    let destination = root.path().join("owner-definition.txt");
    let args = [
        "server",
        "service",
        "export",
        "--game-space",
        "reforger",
        "--output",
        destination.to_str().unwrap(),
    ];
    let saved = invoke(root.path(), &args, 0);
    assert_eq!(saved["data"]["writes_performed"], true);
    let bytes = fs::read(&destination).unwrap();
    assert_eq!(
        bytes,
        saved["data"]["plan"]["definition"]
            .as_str()
            .unwrap()
            .as_bytes()
    );
    let refused = invoke(root.path(), &args, 2);
    assert!(
        refused["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error.as_str().unwrap().contains("already exists"))
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    let foreign = if cfg!(windows) {
        "systemd-user"
    } else {
        "windows-user-task"
    };
    invoke(
        root.path(),
        &[
            "server",
            "service",
            "export",
            "--game-space",
            "arma3",
            "--format",
            foreign,
        ],
        2,
    );
    assert!(!root.path().join("games/reforger/database.db").exists());
}
