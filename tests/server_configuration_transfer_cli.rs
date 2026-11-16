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

fn fixture(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
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
    result
}

#[test]
fn operator_import_redacts_before_review_and_pack_round_trip_has_no_installation_or_secret_authority()
 {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path(), "original");
    let added = invoke(
        &root,
        &["server", "instance", "add", "--name", "MainServer"],
        0,
    );
    let id = added["data"]["instance"]["id"].as_str().unwrap();
    let source = scratch.path().join("operator.cfg");
    let original = "// exact operator comment\r\nhostname=\"OperatorHost\";\r\npasswordAdmin=\"FixtureOnlyPrivate349\";\r\nclass ExtraPolicy { path=\"R:/Mods/Shared missions\"; enabled=1; };\r\nclass Missions { class One { template=\"Example.World\"; difficulty=\"Regular\"; }; };\r\n";
    fs::write(&source, original).unwrap();
    let source_arg = source.to_str().unwrap();
    let before = inventory(&root);
    let review = invoke(
        &root,
        &[
            "server",
            "configuration",
            "import",
            id,
            "--expected-revision",
            "1",
            "--server",
            source_arg,
        ],
        0,
    );
    assert_eq!(review["data"]["writes_performed"], false);
    assert_eq!(
        review["data"]["report"]["secret_references"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        !serde_json::to_string(&review)
            .unwrap()
            .contains("FixtureOnlyPrivate349")
    );
    invoke(
        &root,
        &[
            "server",
            "configuration",
            "import",
            id,
            "--expected-revision",
            "1",
            "--server",
            source_arg,
            "--apply",
        ],
        2,
    );
    invoke(
        &root,
        &[
            "--dry-run",
            "--yes",
            "server",
            "configuration",
            "import",
            id,
            "--expected-revision",
            "1",
            "--server",
            source_arg,
            "--apply",
        ],
        0,
    );
    assert_eq!(inventory(&root), before);
    let saved = invoke(
        &root,
        &[
            "--yes",
            "server",
            "configuration",
            "import",
            id,
            "--expected-revision",
            "1",
            "--server",
            source_arg,
            "--review-paths",
            "--apply",
        ],
        0,
    );
    assert_eq!(saved["data"]["instance"]["revision"], 2);
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    let rendered = invoke(&root, &["server", "instance", "render", id], 0);
    let files = rendered["data"]["configuration_preview"]["files"]
        .as_array()
        .unwrap();
    assert!(
        files[0]["contents"]
            .as_str()
            .unwrap()
            .contains("class ExtraPolicy { path=\"R:/Mods/Shared missions\"; enabled=1; };\r\n")
    );
    assert!(
        !serde_json::to_string(&rendered)
            .unwrap()
            .contains("FixtureOnlyPrivate349")
    );
    let pack_path = scratch.path().join("instance.foxyserver");
    invoke(
        &root,
        &[
            "server",
            "configuration",
            "export-pack",
            id,
            "--file",
            pack_path.to_str().unwrap(),
        ],
        0,
    );
    let pack_bytes = fs::read(&pack_path).unwrap();
    assert!(!String::from_utf8_lossy(&pack_bytes).contains("FixtureOnlyPrivate349"));
    invoke(
        &root,
        &[
            "server",
            "configuration",
            "export-pack",
            id,
            "--file",
            pack_path.to_str().unwrap(),
        ],
        2,
    );
    assert_eq!(fs::read(&pack_path).unwrap(), pack_bytes);
    let imported_root = fixture(scratch.path(), "imported");
    let result = invoke(
        &imported_root,
        &[
            "--yes",
            "server",
            "configuration",
            "import-pack",
            "--file",
            pack_path.to_str().unwrap(),
            "--name",
            "ImportedServer",
            "--apply",
        ],
        0,
    );
    let new_id = result["data"]["instance"]["id"].as_str().unwrap();
    assert_ne!(new_id, id);
    assert!(result["data"]["instance"]["installation_id"].is_null());
    assert_eq!(result["data"]["instance"]["revision"], 1);
    assert_eq!(
        result["data"]["report"]["secret_references"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result["data"]["instance"]["configuration_import"]["paths_reviewed"],
        false
    );
    assert!(
        !imported_root
            .join("games/arma3/server/credentials")
            .exists()
    );
    let roundtrip_path = scratch.path().join("roundtrip.foxyserver");
    invoke(
        &imported_root,
        &[
            "server",
            "configuration",
            "export-pack",
            new_id,
            "--file",
            roundtrip_path.to_str().unwrap(),
        ],
        0,
    );
    let roundtrip: Value = serde_json::from_slice(&fs::read(roundtrip_path).unwrap()).unwrap();
    let original_pack: Value = serde_json::from_slice(&pack_bytes).unwrap();
    assert_eq!(
        roundtrip["configuration_import"]["files"],
        original_pack["configuration_import"]["files"]
    );
    assert_eq!(roundtrip["definition"], original_pack["definition"]);
    let sanitized_path = scratch.path().join("sanitized.foxyserver");
    invoke(
        &root,
        &[
            "server",
            "configuration",
            "export-pack",
            id,
            "--file",
            sanitized_path.to_str().unwrap(),
            "--sanitized",
        ],
        0,
    );
    let sanitized: Value = serde_json::from_slice(&fs::read(sanitized_path).unwrap()).unwrap();
    assert!(sanitized["configuration_import"].is_null());
    assert!(!sanitized["omitted_fields"].as_array().unwrap().is_empty());
    assert_eq!(sanitized["display_name"], "SanitizedServer");
}

#[test]
fn managed_conversion_and_unsupported_inputs_preserve_current_data() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path(), "config");
    let added = invoke(
        &root,
        &["server", "instance", "add", "--name", "MainServer"],
        0,
    );
    let id = added["data"]["instance"]["id"].as_str().unwrap();
    let source = scratch.path().join("basic.cfg");
    fs::write(&source, "MinBandwidth=131072; MaxBandwidth=10000000000; MinErrorToSend=0.125; class sockets { maxPacketSize=1400; }; unknown=17;").unwrap();
    let args = [
        "--yes",
        "server",
        "configuration",
        "import",
        id,
        "--expected-revision",
        "1",
        "--basic",
        source.to_str().unwrap(),
        "--managed",
        "--apply",
    ];
    let applied = invoke(&root, &args, 0);
    assert_eq!(
        applied["data"]["instance"]["definition"]["settings"]["basic"]["max_bandwidth"],
        10000000000u64
    );
    assert!(
        applied["data"]["report"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("unknown"))
    );
    let before = inventory(&root);
    invoke(&root, &args, 2);
    fs::write(
        &source,
        "#include \"uninspected.cfg\"\npassword=\"FixtureOnlyPrivate349\";",
    )
    .unwrap();
    let denied = invoke(
        &root,
        &[
            "--yes",
            "server",
            "configuration",
            "import",
            id,
            "--expected-revision",
            "2",
            "--server",
            source.to_str().unwrap(),
            "--apply",
        ],
        2,
    );
    assert!(
        !serde_json::to_string(&denied)
            .unwrap()
            .contains("FixtureOnlyPrivate349")
    );
    assert_eq!(inventory(&root), before);
}
