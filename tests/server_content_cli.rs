use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn invoke(root: &Path, args: &[&str], code: i32) -> Value {
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
        Some(code),
        "{args:?}: {} {}",
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
    fn visit(base: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.insert(path.strip_prefix(base).unwrap().to_path_buf(), Vec::new());
                visit(base, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn add_instance(root: &Path, space: &str) -> String {
    invoke(
        root,
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
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn register(root: &Path, space: &str, name: &str, addon: &str, supply: &str, url: &str) {
    let folder = root.join(format!("sources-{space}-{name}"));
    fs::create_dir_all(folder.join(addon)).unwrap();
    fs::write(folder.join(addon).join("fixture.txt"), b"unchanged content").unwrap();
    invoke(
        root,
        &[
            "server",
            "content",
            "add-folder",
            "--game-space",
            space,
            "--id",
            name,
            "--path",
            folder.to_str().unwrap(),
        ],
        0,
    );
    let local = root.join(format!("client-{name}"));
    invoke(
        root,
        &[
            "server",
            "content",
            "add-profile",
            "--game-space",
            space,
            "--name",
            name,
            "--folder",
            name,
            "--url",
            url,
            "--local-path",
            local.to_str().unwrap(),
            "--supply",
            supply,
            "--addon",
            addon,
        ],
        0,
    );
}

#[test]
fn content_plan_is_read_only_scoped_and_honest_for_both_games() {
    let root = fixture();
    for space in ["arma3", "reforger"] {
        let id = add_instance(root.path(), space);
        register(
            root.path(),
            space,
            "RepoAlpha",
            "@mod_01",
            "already-present",
            "",
        );
        register(
            root.path(),
            space,
            "RepoBeta",
            "@mod_02",
            "download",
            "https://repo.example.invalid/shared",
        );
        let bindings = root.path().join("bindings.json");
        fs::write(
            &bindings,
            serde_json::to_vec(&json!([
                {"source_id":"RepoAlpha", "role":"server_only", "order":20},
                {"source_id":"RepoBeta", "role":"optional", "order":10}
            ]))
            .unwrap(),
        )
        .unwrap();
        let selected = invoke(
            root.path(),
            &["server", "content", "list", "--game-space", space],
            0,
        );
        let fingerprint = selected["data"]["fingerprint"].as_str().unwrap();
        let before = inventory(root.path());
        let value = invoke(
            root.path(),
            &[
                "server",
                "content",
                "plan",
                &id,
                "--game-space",
                space,
                "--bindings",
                bindings.to_str().unwrap(),
                "--expected-revision",
                "1",
                "--expected-sources-fingerprint",
                fingerprint,
            ],
            0,
        );
        let plan = &value["data"]["plan"];
        assert_eq!(plan["game_space_id"], space);
        assert_eq!(plan["instance_id"], id);
        assert_eq!(plan["instance_revision"], 1);
        assert_eq!(plan["content_sources_fingerprint"], fingerprint);
        assert_eq!(plan["deployment_ready"], false);
        assert_eq!(plan["downloads_started"], false);
        assert_eq!(plan["writes_performed"], false);
        assert_eq!(value["data"]["database_opened"], false);
        assert_eq!(plan["bindings"][0]["profile_name"], "RepoBeta");
        assert_eq!(
            plan["bindings"][0]["addon_paths"][0]["state"],
            "acquisition_required"
        );
        assert_eq!(plan["bindings"][1]["repository_url"], "");
        assert_eq!(
            plan["bindings"][1]["addon_paths"][0]["state"],
            "present_unverified"
        );
        assert!(
            plan["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|blocker| blocker["code"] == "runtime_deployment_unavailable")
        );
        assert_eq!(inventory(root.path()), before);
        let repeated = invoke(
            root.path(),
            &[
                "--dry-run",
                "server",
                "content",
                "plan",
                &id,
                "--game-space",
                space,
                "--bindings",
                bindings.to_str().unwrap(),
            ],
            0,
        );
        assert_eq!(repeated["data"]["plan"]["fingerprint"], plan["fingerprint"]);
        assert_eq!(inventory(root.path()), before);
        assert!(
            !root
                .path()
                .join(format!("games/{space}/database.db"))
                .exists()
        );
        assert!(
            !root
                .path()
                .join(format!("games/{space}/server/instances"))
                .exists()
        );
    }
}

#[test]
fn invalid_or_stale_content_requests_preserve_every_file() {
    let root = fixture();
    let id = add_instance(root.path(), "arma3");
    register(
        root.path(),
        "arma3",
        "RepoAlpha",
        "@mod_01",
        "already-present",
        "https://repo.example.invalid/shared",
    );
    let bindings = root.path().join("bindings.json");
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"required","order":1}]"#,
    )
    .unwrap();
    let before = inventory(root.path());
    for args in [
        vec![
            "server",
            "content",
            "plan",
            &id,
            "--bindings",
            bindings.to_str().unwrap(),
            "--expected-revision",
            "999",
        ],
        vec![
            "server",
            "content",
            "plan",
            &id,
            "--bindings",
            bindings.to_str().unwrap(),
            "--expected-sources-fingerprint",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ],
        vec![
            "--wipe-db",
            "--yes",
            "server",
            "content",
            "plan",
            &id,
            "--bindings",
            bindings.to_str().unwrap(),
        ],
    ] {
        invoke(root.path(), &args, 2);
        assert_eq!(inventory(root.path()), before);
    }
    for document in [
        br#"[{"source_id":"Missing","role":"required","order":1}]"#.as_slice(),
        br#"[{"source_id":"RepoAlpha","role":"required","order":1},{"source_id":"RepoAlpha","role":"optional","order":2}]"#,
        br#"[{"source_id":"RepoAlpha","role":"unknown","order":1}]"#,
        br#"[{"source_id":"RepoAlpha","role":"required","order":1,"extra":true}]"#,
    ] {
        fs::write(&bindings, document).unwrap();
        let before = inventory(root.path());
        invoke(root.path(), &["server", "content", "plan", &id, "--bindings", bindings.to_str().unwrap()], 2);
        assert_eq!(inventory(root.path()), before);
    }
}

#[test]
fn source_mutations_require_ownership_but_preview_can_run_beside_an_owner() {
    let root = fixture();
    let id = add_instance(root.path(), "arma3");
    register(
        root.path(),
        "arma3",
        "RepoAlpha",
        "@mod_01",
        "already-present",
        "",
    );
    let bindings = root.path().join("bindings.json");
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"required","order":1}]"#,
    )
    .unwrap();
    let source_path = root.path().join("games/arma3/server/content-sources.json");
    let original = fs::read(&source_path).unwrap();
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("games/arma3/database.lock"))
        .unwrap();
    lock.lock().unwrap();
    invoke(
        root.path(),
        &[
            "server",
            "content",
            "plan",
            &id,
            "--bindings",
            bindings.to_str().unwrap(),
        ],
        0,
    );
    let folder = root.path().join("another");
    let args = [
        "server",
        "content",
        "add-folder",
        "--id",
        "Other",
        "--path",
        folder.to_str().unwrap(),
    ];
    let mut dry_run = vec!["--dry-run"];
    dry_run.extend(args);
    invoke(root.path(), &dry_run, 0);
    invoke(root.path(), &args, 6);
    assert_eq!(fs::read(source_path).unwrap(), original);
    assert!(!folder.exists());
    assert!(!root.path().join("games/arma3/database.db").exists());
}
