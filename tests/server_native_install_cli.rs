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

fn errors(value: &Value) -> String {
    value["errors"].to_string()
}

#[test]
fn native_install_refuses_unsafe_requests_before_contacting_steam() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("config");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let external = temp.path().join("servers");
    fs::create_dir(&external).unwrap();
    let (code, added) = invoke(&root, &["server", "instance", "add", "--name", "Alpha"]);
    assert_eq!(code, 0);
    let id = added["data"]["instance"]["id"].as_str().unwrap().to_owned();
    let fresh = external.join("ServerAlpha");
    let fresh = fresh.to_str().unwrap();
    let before = tree(temp.path());

    for command in ["check", "validate"] {
        let (code, value) = invoke(&root, &["server", "installation", command, &id]);
        assert_ne!(code, 0, "{value}");
        assert!(errors(&value).contains("installation"), "{value}");
    }
    for command in ["update", "repair"] {
        let (code, value) = invoke(
            &root,
            &[
                "server",
                "installation",
                command,
                "plan",
                &id,
                "--directory",
                fresh,
            ],
        );
        assert_ne!(code, 0, "{value}");
        assert!(errors(&value).contains("installation"), "{value}");
        let (code, value) = invoke(
            &root,
            &[
                "server",
                "installation",
                command,
                "apply",
                &id,
                "--directory",
                fresh,
                "--expected-plan-fingerprint",
                &"0".repeat(64),
            ],
        );
        assert_eq!(code, 2, "{value}");
        assert!(errors(&value).contains("--yes"), "{value}");
    }

    let (code, value) = invoke(
        &root,
        &[
            "server",
            "installation",
            "install",
            "apply",
            &id,
            "--directory",
            fresh,
            "--expected-plan-fingerprint",
            &"0".repeat(64),
        ],
    );
    assert_eq!(code, 2);
    assert!(errors(&value).contains("--yes"), "{value}");

    let (code, value) = invoke(
        &root,
        &[
            "server",
            "installation",
            "install",
            "plan",
            &id,
            "--directory",
            fresh,
        ],
    );
    assert_ne!(code, 0);
    assert!(
        errors(&value).contains("Prepare a native Steam session with attended login"),
        "{value}"
    );

    fs::create_dir(external.join("Existing")).unwrap();
    fs::write(external.join("Existing/keep.txt"), b"keep").unwrap();
    let existing = external.join("Existing");
    let inside = root.join("games/arma3/managed");
    for (directory, expected) in [
        (
            existing.to_str().unwrap(),
            "existing folders are never overwritten",
        ),
        ("relative/ServerAlpha", "absolute path"),
        (
            inside.to_str().unwrap(),
            "separate from Foxy's data directory",
        ),
    ] {
        let (code, value) = invoke(
            &root,
            &[
                "server",
                "installation",
                "install",
                "plan",
                &id,
                "--directory",
                directory,
                "--anonymous",
            ],
        );
        assert_eq!(code, 2, "{directory}: {value}");
        assert!(errors(&value).contains(expected), "{directory}: {value}");
    }
    for args in [
        vec!["--branch", "../public", "--anonymous"],
        vec!["--account", "../alias"],
    ] {
        let mut command = vec![
            "server",
            "installation",
            "install",
            "plan",
            id.as_str(),
            "--directory",
            fresh,
        ];
        command.extend(args);
        let (code, value) = invoke(&root, &command);
        assert_eq!(code, 2, "{value}");
    }
    let (code, _) = invoke(
        &root,
        &[
            "server",
            "installation",
            "install",
            "plan",
            "server_00000000000000000000000000000009",
            "--directory",
            fresh,
            "--anonymous",
        ],
    );
    assert_ne!(code, 0);
    let (code, value) = invoke(
        &root,
        &[
            "server",
            "installation",
            "--game-space",
            "reforger",
            "install",
            "plan",
            &id,
            "--directory",
            fresh,
            "--anonymous",
        ],
    );
    assert_ne!(code, 0, "{value}");

    let mut after = tree(temp.path());
    after.remove(Path::new("servers/Existing/keep.txt"));
    assert_eq!(after, before);
    assert_eq!(
        fs::read(external.join("Existing/keep.txt")).unwrap(),
        b"keep"
    );
    assert!(!external.join("ServerAlpha").exists());
    assert!(!root.join("games/arma3/database.db").exists());
}

#[path = "server_native_install_cli/gates.rs"]
mod gates;
#[path = "server_native_install_cli/native.rs"]
mod native;
#[path = "server_native_install_cli/reforger.rs"]
mod reforger;
