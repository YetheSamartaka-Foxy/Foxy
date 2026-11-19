use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn isolated_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    root
}

fn run_cli(root: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command.arg("--config-dir").arg(root).args(args);
    command.env("FOXY_CONFIG_DIR", root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.output().unwrap()
}

#[test]
fn server_commands_reject_wipe_db_without_opening_or_wiping_database() {
    let root = isolated_root();
    let database = root.path().join("games/arma3/database.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let sentinel = b"database fixture for wipe refusal";
    fs::write(&database, sentinel).unwrap();
    let cases: &[&[&str]] = &[
        &["server", "instance", "list", "--game-space", "arma3"],
        &["server", "owner", "--game-space", "arma3"],
        &["server", "service", "export", "--game-space", "arma3"],
        &[
            "server",
            "backup",
            "list",
            "missing",
            "--game-space",
            "arma3",
        ],
        &[
            "server",
            "restore",
            "status",
            "missing",
            "--game-space",
            "arma3",
        ],
        &[
            "server",
            "session",
            "list",
            "missing",
            "--game-space",
            "arma3",
        ],
        &[
            "server",
            "reforger-addons",
            "list",
            "missing",
            "--game-space",
            "arma3",
        ],
        &[
            "server",
            "inspect-addons",
            "--address",
            "127.0.0.1",
            "--port",
            "2302",
        ],
        &[
            "server",
            "publish",
            "plan",
            "missing",
            "--output",
            "R:/Mods/Out",
            "--game-space",
            "arma3",
        ],
    ];

    for subcommand in cases {
        let mut args = vec!["--json", "--wipe-db", "--yes"];
        args.extend_from_slice(subcommand);
        let output = run_cli(root.path(), &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "expected VALIDATION_ERROR (2) for {subcommand:?}, got {:?}; stdout={}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|err| {
            panic!("invalid JSON for {subcommand:?}: {err}\nstdout={stdout}\nstderr={stderr}")
        });
        assert_eq!(json["ok"], Value::Bool(false));
        assert_eq!(json["action"], Value::String("server".to_string()));
        assert!(json["data"].is_null());
        let errors = json["errors"].as_array().expect("errors array");
        assert!(
            errors
                .iter()
                .filter_map(Value::as_str)
                .any(|line| line.contains("--wipe-db")),
            "expected --wipe-db refusal message in {json}"
        );
        assert_eq!(fs::read(&database).unwrap(), sentinel);
        assert!(!root.path().join("logs").exists());
    }
}

#[test]
fn server_metadata_reads_do_not_bootstrap_the_client() {
    let root = isolated_root();
    let output = run_cli(
        root.path(),
        &[
            "--json",
            "server",
            "instance",
            "list",
            "--game-space",
            "arma3",
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    let json: Value = serde_json::from_slice(&output.stdout).expect("server list JSON");
    assert_eq!(json["action"], "server.instance.list");
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["database_opened"], false);
    assert!(!root.path().join("games/arma3/database.db").exists());
    assert!(!root.path().join("logs").exists());
}

#[test]
fn version_json_envelope_and_help_contracts_are_consistent() {
    let root = isolated_root();

    let version = run_cli(root.path(), &["--json", "version"]);
    assert_eq!(version.status.code(), Some(0));
    let json: Value = serde_json::from_slice(&version.stdout).expect("version JSON");
    assert_eq!(json["ok"], Value::Bool(true));
    assert_eq!(json["action"], Value::String("version".to_string()));
    let reported_version = json["data"]["version"].as_str().expect("version string");
    assert!(!reported_version.is_empty());
    let flag_version = run_cli(root.path(), &["--version"]);
    assert_eq!(flag_version.status.code(), Some(0));
    let flag_text = String::from_utf8_lossy(&flag_version.stdout);
    assert!(
        flag_text.contains(reported_version),
        "expected `--version` ({flag_text:?}) to match `version --json` ({reported_version:?})"
    );
    assert_eq!(json["errors"], Value::Array(Vec::new()));

    for help_args in [
        &["--help"][..],
        &["server", "--help"][..],
        &["server", "backup", "--help"][..],
        &["server", "session", "--help"][..],
    ] {
        let help = run_cli(root.path(), help_args);
        assert_eq!(
            help.status.code(),
            Some(0),
            "help failed for {help_args:?}: {}",
            String::from_utf8_lossy(&help.stderr)
        );
        let text = String::from_utf8(help.stdout).expect("UTF-8 help output");
        assert!(!text.is_empty(), "empty help output for {help_args:?}");
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "em/en dash found in help output for {help_args:?}"
        );
    }
}
