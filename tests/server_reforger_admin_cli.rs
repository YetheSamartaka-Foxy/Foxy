use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

const IDENTITY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE=";

fn run(root: &Path, args: &[&str]) -> (i32, Value, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .arg("--quiet")
        .args(args)
        .env("FOXY_CONFIG_DIR", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("args={args:?}\nstdout={stdout}\nstderr={stderr}"));
    (output.status.code().unwrap_or(-1), value, stdout + &stderr)
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

fn add(root: &Path, game: &str, name: &str) -> String {
    let (code, value, text) = run(
        root,
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            game,
            "--name",
            name,
        ],
    );
    assert_eq!(code, 0, "{text}");
    value["data"]["instance"]["id"].as_str().unwrap().to_owned()
}

fn dry_run(root: &Path, game: &str, command: &[&str]) -> (i32, Value, String) {
    let mut args = vec!["--dry-run", "server", "reforger-admin"];
    args.extend_from_slice(command);
    args.extend(["--game-space", game]);
    run(root, &args)
}

fn inventory(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn visit(root: &Path, directory: &Path, result: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative == "logs" || relative.starts_with("logs/") {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                result.insert(relative, None);
                visit(root, &path, result);
            } else {
                result.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }

    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn typed_dry_runs_are_stable_offline_and_default_bans_to_page_one() {
    let root = fixture();
    let id = add(root.path(), "reforger", "AdminReforger");
    let before = inventory(root.path());

    let (code, players, text) = dry_run(root.path(), "reforger", &["players", &id]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(players["action"], "server.reforger-admin.players");
    assert_eq!(players["data"]["wire_text"], "#players");
    assert_eq!(players["data"]["owner_contacted"], false);
    assert_eq!(players["data"]["database_opened"], false);
    assert_eq!(players["data"]["writes_performed"], false);

    let (code, bans, text) = dry_run(root.path(), "reforger", &["bans", &id]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(bans["data"]["wire_text"], "#ban list 1");
    assert_eq!(bans["data"]["command"]["page"], 1);

    let (code, kick, text) = dry_run(
        root.path(),
        "reforger",
        &[
            "kick",
            &id,
            "--player-id",
            "7",
            "--expected-identity",
            IDENTITY,
            "--expected-run-generation",
            "5",
        ],
    );
    assert_eq!(code, 0, "{text}");
    assert_eq!(kick["data"]["wire_text"], "#kick 7");

    let after = inventory(root.path());
    assert_eq!(
        after, before,
        "typed dry runs must not write metadata or DB files"
    );
    assert!(!after.keys().any(|path| path.ends_with("database.db")));
}

#[test]
fn game_capability_and_typed_input_validation_happen_before_owner_access() {
    let root = fixture();
    let reforger_id = add(root.path(), "reforger", "AdminReforger");
    let arma_id = add(root.path(), "arma3", "AdminArma");

    let (code, _, text) = dry_run(root.path(), "arma3", &["players", &arma_id]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("unavailable"), "{text}");

    for command in [
        vec![
            "ban",
            &reforger_id,
            "--identity",
            "123456789",
            "--duration-seconds",
            "10",
            "--reason",
            "test",
            "--expected-run-generation",
            "5",
        ],
        vec![
            "ban",
            &reforger_id,
            "--identity",
            IDENTITY,
            "--duration-seconds",
            "10",
            "--reason",
            "bad\n#restart",
            "--expected-run-generation",
            "5",
        ],
        vec![
            "kick",
            &reforger_id,
            "--player-id",
            "7",
            "--expected-identity",
            "invalid_identity",
            "--expected-run-generation",
            "5",
        ],
    ] {
        let (code, _, text) = dry_run(root.path(), "reforger", &command);
        assert_eq!(
            code, 2,
            "invalid typed command must fail before owner attach: {text}"
        );
    }
}

#[test]
fn side_effecting_commands_require_yes_and_an_explicit_nonzero_run() {
    let root = fixture();
    let id = add(root.path(), "reforger", "AdminReforger");

    for (args, expected) in [
        (
            vec![
                "server",
                "reforger-admin",
                "reload-scenario",
                &id,
                "--expected-run-generation",
                "9",
                "--game-space",
                "reforger",
            ],
            "requires --yes",
        ),
        (
            vec![
                "--yes",
                "server",
                "reforger-admin",
                "reload-scenario",
                &id,
                "--game-space",
                "reforger",
            ],
            "--expected-run-generation",
        ),
        (
            vec![
                "--yes",
                "server",
                "reforger-admin",
                "reload-scenario",
                &id,
                "--expected-run-generation",
                "0",
                "--game-space",
                "reforger",
            ],
            "greater than zero",
        ),
    ] {
        let (code, _, text) = run(root.path(), &args);
        assert_eq!(code, 2, "{text}");
        assert!(text.contains(expected), "{text}");
    }
}
