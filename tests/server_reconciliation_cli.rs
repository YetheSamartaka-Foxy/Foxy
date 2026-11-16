use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn invoke(root: &Path, args: &[&str], code: i32) -> Value {
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
        Some(code),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    if !root.exists() {
        return result;
    }
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

fn add_instance(root: &Path, name: &str) -> String {
    invoke(root, &["server", "instance", "add", "--name", name], 0)["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn bind_and_stage(root: &Path, scratch: &Path, id: &str) -> String {
    let bindings = scratch.join(format!("{id}-bindings.json"));
    fs::write(
        &bindings,
        br#"[{"source_id":"Shared","role":"required","order":0}]"#,
    )
    .unwrap();
    let path = bindings.to_str().unwrap();
    let plan = invoke(
        root,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            id,
            "--bindings",
            path,
        ],
        0,
    );
    invoke(
        root,
        &[
            "server",
            "content",
            "bindings",
            "save",
            id,
            "--bindings",
            path,
            "--expected-plan-fingerprint",
            plan["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    let stage = invoke(root, &["server", "content", "stage", "plan", id], 0);
    invoke(
        root,
        &[
            "server",
            "content",
            "stage",
            "apply",
            id,
            "--expected-plan-fingerprint",
            stage["data"]["plan"]["fingerprint"].as_str().unwrap(),
        ],
        0,
    );
    stage["data"]["plan"]["stage_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn shared_source_consumers_and_reviewed_reconciliation_keep_unowned_and_peer_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("config");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let scratch = temp.path().join("scratch");
    let shared = temp.path().join("shared");
    let other = temp.path().join("other");
    fs::create_dir_all(shared.join("@mod_01/addons")).unwrap();
    fs::create_dir_all(other.join("@mod_01/addons")).unwrap();
    fs::create_dir(&scratch).unwrap();
    fs::write(shared.join("@mod_01/addons/a.pbo"), b"first").unwrap();
    let url = "https://repo.example.invalid:8080/main/";
    for (folder, path) in [("shared", &shared), ("other", &other)] {
        invoke(
            &root,
            &[
                "server",
                "content",
                "add-folder",
                "--id",
                folder,
                "--path",
                path.to_str().unwrap(),
            ],
            0,
        );
    }
    for (name, folder, local) in [("Shared", "shared", &shared), ("SameUrl", "other", &other)] {
        invoke(
            &root,
            &[
                "server",
                "content",
                "add-profile",
                "--name",
                name,
                "--folder",
                folder,
                "--url",
                url,
                "--local-path",
                local.to_str().unwrap(),
                "--supply",
                "already-present",
                "--addon",
                "@mod_01",
            ],
            0,
        );
    }
    let space = root.join("games/arma3");
    fs::write(
        space.join("repositories.json"),
        serde_json::to_vec(&json!([
            {"name": "MainRepo", "address": url, "path": shared},
            {"name": "OtherUrl", "address": "https://other.example.invalid/", "path": shared}
        ]))
        .unwrap(),
    )
    .unwrap();
    let alpha = add_instance(&root, "Alpha");
    let beta = add_instance(&root, "Beta");
    let older = bind_and_stage(&root, &scratch, &alpha);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(shared.join("@mod_01/addons/a.pbo"), b"second").unwrap();
    let newest = bind_and_stage(&root, &scratch, &alpha);
    assert_ne!(older, newest);
    let beta_stage = bind_and_stage(&root, &scratch, &beta);

    let report = invoke(&root, &["server", "content", "consumers"], 0)["data"]["report"].clone();
    let sources = report["sources"].as_array().unwrap();
    let shared_report = sources
        .iter()
        .find(|source| source["profile"] == "Shared")
        .unwrap();
    assert_eq!(shared_report["client_repositories"], json!(["MainRepo"]));
    let users = shared_report["instances"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert!(
        users
            .iter()
            .all(|user| user["saved_binding_role"] == "required")
    );
    let same_url = sources
        .iter()
        .find(|source| source["profile"] == "SameUrl")
        .unwrap();
    assert!(same_url["instances"].as_array().unwrap().is_empty());
    assert!(
        same_url["client_repositories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        same_url["shared_with_profiles"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let diagnostics = report["diagnostics"].to_string();
    assert!(diagnostics.contains("OtherUrl"));
    assert!(diagnostics.contains("separate repository instances"));

    let instance_root = space.join("server/instances").join(&alpha);
    let runtime = instance_root
        .join("deployments/runtimes")
        .join(format!("runtime_{}", "a".repeat(32)));
    fs::create_dir_all(runtime.join("engine")).unwrap();
    fs::write(
        runtime.join("manifest.json"),
        format!("{{\"preparation_id\":\"prepared_{}\"}}", "b".repeat(64)),
    )
    .unwrap();
    fs::write(runtime.join("engine/server.bin"), [7u8; 32]).unwrap();
    let leftover = instance_root.join("content-stages/.reconcile-interrupted");
    fs::create_dir_all(&leftover).unwrap();
    fs::write(leftover.join("part"), b"x").unwrap();
    fs::write(
        instance_root.join("content-stages/operator-notes.txt"),
        b"keep",
    )
    .unwrap();

    let strict_runtime_plan = invoke(&root, &["server", "deploy", "reconcile", "plan", &alpha], 0)
        ["data"]["plan"]
        .clone();
    assert!(
        !strict_runtime_plan["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        strict_runtime_plan["unrecognized_entries"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("runtimes/runtime_{}", "a".repeat(32))))
    );
    let strict_runtime_fingerprint = strict_runtime_plan["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let before_strict_runtime_apply = tree(temp.path());
    invoke(
        &root,
        &[
            "--yes",
            "server",
            "deploy",
            "reconcile",
            "apply",
            &alpha,
            "--expected-plan-fingerprint",
            &strict_runtime_fingerprint,
        ],
        2,
    );
    assert_eq!(tree(temp.path()), before_strict_runtime_apply);
    fs::rename(
        &runtime,
        instance_root.join("deployments/runtimes/operator-runtime"),
    )
    .unwrap();
    let operator_runtime = instance_root.join("deployments/runtimes/operator-runtime");

    let plan = invoke(&root, &["server", "deploy", "reconcile", "plan", &alpha], 0)["data"]["plan"]
        .clone();
    let candidates = plan["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(candidates.contains(&older));
    assert!(!candidates.contains(&newest));
    assert!(!candidates.contains(&format!("runtime_{}", "a".repeat(32))));
    assert!(!candidates.contains(&"content-stages/.reconcile-interrupted".to_owned()));
    let unknown = plan["unrecognized_entries"].as_array().unwrap();
    assert!(unknown.contains(&json!("content-stages/operator-notes.txt")));
    assert!(unknown.contains(&json!(format!("content-stages/.reconcile-interrupted"))));
    assert!(unknown.contains(&json!("runtimes/operator-runtime")));
    assert!(plan["blockers"].as_array().unwrap().is_empty());
    let fingerprint = plan["fingerprint"].as_str().unwrap().to_owned();

    let before = tree(temp.path());
    invoke(
        &root,
        &[
            "server",
            "deploy",
            "reconcile",
            "apply",
            &alpha,
            "--expected-plan-fingerprint",
            &fingerprint,
        ],
        2,
    );
    invoke(
        &root,
        &[
            "--yes",
            "server",
            "deploy",
            "reconcile",
            "apply",
            &alpha,
            "--expected-plan-fingerprint",
            &"0".repeat(64),
        ],
        2,
    );
    invoke(
        &root,
        &[
            "--dry-run",
            "server",
            "deploy",
            "reconcile",
            "apply",
            &alpha,
            "--expected-plan-fingerprint",
            &fingerprint,
        ],
        0,
    );
    assert_eq!(tree(temp.path()), before);

    let older_stage = instance_root.join("content-stages").join(&older);
    let payload = older_stage.join("addons/@mod_01/addons/a.pbo");
    let original = fs::read(&payload).unwrap();
    assert_eq!(original.len(), 5);
    fs::write(&payload, b"alter").unwrap();
    invoke(
        &root,
        &[
            "--yes",
            "server",
            "deploy",
            "reconcile",
            "apply",
            &alpha,
            "--expected-plan-fingerprint",
            &fingerprint,
        ],
        2,
    );
    assert!(older_stage.exists());
    fs::write(&payload, &original).unwrap();

    let applied = invoke(
        &root,
        &[
            "--yes",
            "server",
            "deploy",
            "reconcile",
            "apply",
            &alpha,
            "--expected-plan-fingerprint",
            &fingerprint,
        ],
        0,
    );
    assert_eq!(
        applied["data"]["outcome"]["removed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!instance_root.join("content-stages").join(&older).exists());
    assert!(instance_root.join("content-stages").join(&newest).exists());
    assert!(operator_runtime.exists());
    assert_eq!(
        fs::read(operator_runtime.join("engine/server.bin")).unwrap(),
        [7u8; 32]
    );
    assert!(leftover.exists());
    assert_eq!(
        fs::read(instance_root.join("content-stages/operator-notes.txt")).unwrap(),
        b"keep"
    );
    assert!(
        space
            .join("server/instances")
            .join(&beta)
            .join("content-stages")
            .join(&beta_stage)
            .exists()
    );
    assert_eq!(
        fs::read(shared.join("@mod_01/addons/a.pbo")).unwrap(),
        b"second"
    );
    let after = invoke(&root, &["server", "deploy", "reconcile", "plan", &alpha], 0);
    assert!(
        after["data"]["plan"]["candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let pins = invoke(&root, &["server", "deploy", "pins", &alpha], 0);
    assert_eq!(pins["data"]["pins"]["generation"], 0);
    invoke(
        &root,
        &[
            "--yes",
            "server",
            "deploy",
            "pin",
            &alpha,
            &format!("deployment_{}", "c".repeat(64)),
            "--expected-pin-generation",
            "0",
        ],
        3,
    );
    invoke(
        &root,
        &[
            "--dry-run",
            "server",
            "deploy",
            "pin",
            &alpha,
            &format!("deployment_{}", "c".repeat(64)),
            "--expected-pin-generation",
            "0",
        ],
        3,
    );
    invoke(
        &root,
        &[
            "server",
            "deploy",
            "pin",
            &alpha,
            &format!("deployment_{}", "c".repeat(64)),
            "--expected-pin-generation",
            "0",
        ],
        2,
    );
    let unpin = invoke(
        &root,
        &[
            "--dry-run",
            "server",
            "deploy",
            "unpin",
            &alpha,
            &format!("deployment_{}", "d".repeat(64)),
            "--expected-pin-generation",
            "0",
        ],
        0,
    );
    assert_eq!(unpin["data"]["pins"]["generation"], 0);
    assert_eq!(unpin["data"]["writes_performed"], false);
    invoke(
        &root,
        &[
            "--yes",
            "server",
            "deploy",
            "unpin",
            &alpha,
            "bad-id",
            "--expected-pin-generation",
            "0",
        ],
        2,
    );
}
