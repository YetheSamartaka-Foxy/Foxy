use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn invoke(root: &Path, args: &[&str], expected: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .args(["--game-space", "arma3"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], expected == 0);
    value
}

fn fixture(parent: &Path) -> PathBuf {
    let root = parent.join("server-keys");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("games.json"),
        include_bytes!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    root
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(base, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(base).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    // The space claim rewrites its owner PID on every claiming command; it is not instance data.
    result.retain(|path, _| path.file_name().is_none_or(|name| name != "database.owner"));
    result
}

fn public_bikey(authority: &[u8]) -> Vec<u8> {
    let mut blob = vec![
        6, 2, 0, 0, 0x00, 0x24, 0x00, 0x00, b'R', b'S', b'A', b'1', 0x00, 0x04, 0x00, 0x00, 0x01,
        0x00, 0x01, 0x00,
    ];
    blob.extend(std::iter::repeat_n(0x5a, 128));
    let mut bytes = authority.to_vec();
    bytes.push(0);
    bytes.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    bytes.extend(blob);
    bytes
}

fn add_instance(root: &Path) -> String {
    invoke(
        root,
        &["server", "instance", "add", "--name", "MainServer"],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn operator_key_review_is_write_free_and_apply_requires_fresh_explicit_selection() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path());
    let id = add_instance(&root);
    let key_path = scratch.path().join("FixtureAuthority.bikey");
    let key_bytes = public_bikey(b"FixtureAuthority");
    fs::write(&key_path, &key_bytes).unwrap();
    let key_path_arg = key_path.to_str().unwrap();
    let before = inventory(&root);

    let reviewed = invoke(
        &root,
        &[
            "server",
            "keys",
            "audit",
            &id,
            "--operator-key",
            key_path_arg,
        ],
        0,
    );
    let review = &reviewed["data"]["review"];
    assert_eq!(reviewed["data"]["writes_performed"], false);
    assert_eq!(reviewed["data"]["database_opened"], false);
    assert_eq!(reviewed["data"]["ownership_claimed"], false);
    assert_eq!(review["cryptographic_verification"], false);
    assert_eq!(review["candidates"][0]["key"]["identity"]["bits"], 1024);
    assert_eq!(
        review["candidates"][0]["key"]["identity"]["authority"],
        "FixtureAuthority"
    );
    assert_eq!(
        review["candidates"][0]["key"]["origins"][0]["kind"],
        "operator"
    );
    assert_eq!(inventory(&root), before);

    let candidate = review["candidates"][0]["id"].as_str().unwrap();
    let fingerprint = review["fingerprint"].as_str().unwrap();
    let expected_revision = review["instance_revision"].as_u64().unwrap().to_string();
    let stale = invoke(
        &root,
        &[
            "--yes",
            "server",
            "keys",
            "audit",
            &id,
            "--operator-key",
            key_path_arg,
            "--apply",
            "--fingerprint",
            "stale-fingerprint",
            "--trust",
            candidate,
        ],
        2,
    );
    assert_eq!(stale["data"], Value::Null);
    assert_eq!(inventory(&root), before);

    invoke(
        &root,
        &[
            "--yes",
            "server",
            "keys",
            "audit",
            &id,
            "--operator-key",
            key_path_arg,
            "--apply",
            "--fingerprint",
            fingerprint,
            "--expected-revision",
            "0",
            "--trust",
            candidate,
        ],
        2,
    );
    assert_eq!(inventory(&root), before);

    invoke(
        &root,
        &[
            "--yes",
            "server",
            "keys",
            "audit",
            &id,
            "--operator-key",
            key_path_arg,
            "--apply",
            "--fingerprint",
            fingerprint,
            "--trust",
            "unknown:candidate",
        ],
        2,
    );
    assert_eq!(inventory(&root), before);

    let applied = invoke(
        &root,
        &[
            "--yes",
            "server",
            "keys",
            "audit",
            &id,
            "--operator-key",
            key_path_arg,
            "--apply",
            "--fingerprint",
            fingerprint,
            "--expected-revision",
            &expected_revision,
            "--trust",
            candidate,
        ],
        0,
    );
    assert_eq!(applied["data"]["writes_performed"], true);
    assert_eq!(applied["data"]["cryptographic_verification"], false);
    assert_eq!(applied["data"]["changes_client_enablement"], false);
    assert_eq!(fs::read(&key_path).unwrap(), key_bytes);
}

#[test]
fn dry_run_never_saves_even_when_apply_is_requested() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path());
    let id = add_instance(&root);
    let key_path = scratch.path().join("FixtureAuthority.bikey");
    fs::write(&key_path, public_bikey(b"FixtureAuthority")).unwrap();
    let key_path_arg = key_path.to_str().unwrap();
    let review = invoke(
        &root,
        &[
            "server",
            "keys",
            "audit",
            &id,
            "--operator-key",
            key_path_arg,
        ],
        0,
    );
    let fingerprint = review["data"]["review"]["fingerprint"].as_str().unwrap();
    let before = inventory(&root);
    let preview = invoke(
        &root,
        &[
            "--dry-run",
            "--yes",
            "server",
            "keys",
            "audit",
            &id,
            "--operator-key",
            key_path_arg,
            "--apply",
            "--fingerprint",
            fingerprint,
            "--trust",
            "candidate",
        ],
        0,
    );
    assert_eq!(preview["data"]["dry_run"], true);
    assert_eq!(preview["data"]["writes_performed"], false);
    assert_eq!(preview["data"]["ownership_claimed"], false);
    assert_eq!(preview["data"]["cryptographic_verification"], false);
    assert_eq!(inventory(&root), before);
    assert!(!root.join("games/arma3/database.db").exists());
}
