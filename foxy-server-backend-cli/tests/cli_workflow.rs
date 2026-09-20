use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

fn write_json(path: &Path, value: Value) {
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn json_preview_atomic_create_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("mods").join("@mod");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("data.txt"), b"data").unwrap();
    std::fs::write(source.join("extra.txt"), b"extra").unwrap();
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "Test",
            "basePath": dir.path().join("mods"),
            "requiredMods": [{"modName": "@mod"}]
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");

    let preview = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(preview.status.success());
    let result: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(result["command"], "create");
    assert!(result["details"]["actions"].as_array().unwrap().len() >= 2);
    assert!(!output.exists());

    let built = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .args(["--atomic", "--yes", "--no-progress", "--mode", "hybrid"])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let result: Value = serde_json::from_slice(&built.stdout).unwrap();
    assert_eq!(result["status"], "ok");
    assert!(output.join("repo.json").exists());
    assert!(
        Command::new(bin)
            .arg("verify")
            .arg(&output)
            .output()
            .unwrap()
            .status
            .success()
    );

    std::fs::write(source.join("data.txt"), b"updated").unwrap();
    let updated = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .args(["--incremental", "--report", "--no-progress"])
        .output()
        .unwrap();
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let result: Value = serde_json::from_slice(&updated.stdout).unwrap();
    assert_eq!(result["details"]["changes"][0]["kind"], "changed");
    assert!(output.join(".foxy-hash-cache.json").exists());

    let verified = Command::new(bin)
        .args(["--json", "verify"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(verified.status.success());
    let result: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(result["details"]["mods"], 1);
    std::fs::write(output.join("@mod").join("data.txt"), b"different").unwrap();
    let failed = Command::new(bin)
        .args(["--json", "verify"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let result: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(result["status"], "error");

    let invalid = Command::new(bin)
        .args(["--json", "create"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let result: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(result["status"], "error");
}

#[test]
fn only_rebuilds_selected_repository_and_preserves_space_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(mods.join("@one")).unwrap();
    std::fs::create_dir_all(mods.join("@two")).unwrap();
    std::fs::write(mods.join("@one").join("data.txt"), b"one").unwrap();
    std::fs::write(mods.join("@two").join("data.txt"), b"two").unwrap();
    for (folder, mod_name) in [("one", "@one"), ("two", "@two")] {
        write_json(
            &dir.path().join(format!("{folder}.json")),
            json!({
                "repoName": folder,
                "basePath": mods,
                "requiredMods": [{"modName": mod_name}]
            }),
        );
    }
    let space = dir.path().join("space.json");
    write_json(
        &space,
        json!({
            "name": "Test Space",
            "baseUrl": "https://example.com/repos/",
            "repositories": [
                {"config": "one.json", "folder": "one"},
                {"config": "two.json", "folder": "two"}
            ]
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");
    let first = Command::new(bin)
        .arg("create-space")
        .arg(&space)
        .arg(&output)
        .arg("--no-progress")
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let untouched = std::fs::read(output.join("two").join("repo.json")).unwrap();
    std::fs::write(mods.join("@one").join("data.txt"), b"one changed").unwrap();

    let second = Command::new(bin)
        .args(["--json", "create-space"])
        .arg(&space)
        .arg(&output)
        .args(["--only", "one", "--no-progress"])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let result: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(
        std::fs::read(output.join("two").join("repo.json")).unwrap(),
        untouched
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(output.join("repository_space.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["entries"].as_array().unwrap().len(), 2);
}
