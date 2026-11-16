use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn invoke(root: &Path, args: &[&str]) -> (i32, Value) {
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
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{args:?} emitted no JSON: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_none_or(|name| name != "database.owner") {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    result
}

#[test]
fn workshop_commands_refuse_invalid_requests_before_contacting_steam() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("config");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let before = tree(temp.path());

    let (code, list) = invoke(&root, &["server", "workshop", "list"]);
    assert_eq!(code, 0, "{list}");
    assert_eq!(list["data"]["workshop"]["generation"], 0);
    assert!(
        list["data"]["workshop"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let (code, verify) = invoke(&root, &["server", "workshop", "verify", "123"]);
    assert_eq!(code, 3, "{verify}");
    assert!(verify["errors"].to_string().contains("not tracked"));

    let (code, missing_yes) = invoke(
        &root,
        &[
            "server",
            "workshop",
            "acquire",
            "123",
            "--anonymous",
            "--expected-plan-fingerprint",
            &"0".repeat(64),
        ],
    );
    assert_eq!(code, 2);
    assert!(missing_yes["errors"].to_string().contains("--yes"));

    for (args, expected) in [
        (
            vec!["server", "workshop", "plan", "../1", "--anonymous"],
            "Invalid Workshop item id",
        ),
        (
            vec!["server", "workshop", "plan", "0", "--anonymous"],
            "Invalid Workshop item id",
        ),
        (
            vec!["server", "workshop", "plan", "123", "--account", "../x"],
            "Invalid Steam session alias",
        ),
        (
            vec![
                "server",
                "workshop",
                "plan",
                "123",
                "--anonymous",
                "--repair",
            ],
            "Only tracked Workshop items can be repaired",
        ),
        (
            vec!["server", "workshop", "plan", "123"],
            "Prepare a native Steam session with attended login",
        ),
    ] {
        let (code, value) = invoke(&root, &args);
        assert_eq!(code, 2, "{args:?}: {value}");
        assert!(
            value["errors"].to_string().contains(expected),
            "{args:?}: {value}"
        );
    }
    let (code, value) = invoke(
        &root,
        &[
            "server",
            "workshop",
            "--game-space",
            "reforger",
            "plan",
            "123",
            "--anonymous",
        ],
    );
    assert_ne!(code, 0, "{value}");

    assert_eq!(tree(temp.path()), before);
    assert!(!root.join("games/arma3/server/workshop").exists());
    assert!(!root.join("games/arma3/database.db").exists());
}
