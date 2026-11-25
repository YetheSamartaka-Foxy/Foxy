use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

fn write_json(path: &Path, value: Value) {
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn mod_line_files_are_rewritten_after_a_successful_create() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("mods").join("@mod");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("data.txt"), b"data").unwrap();
    let script = dir.path().join("start-server.cmd");
    let original = "@echo off\r\narma3server_x64.exe -mod=mods/@stale; -config=server.cfg\r\n";
    std::fs::write(&script, original).unwrap();
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "Test",
            "basePath": dir.path().join("mods"),
            "requiredMods": [{"modName": "@mod"}],
            "modLineFiles": ["start-server.cmd"]
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
    assert!(
        result["details"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["action"] == "update-mod-line")
    );
    assert_eq!(std::fs::read_to_string(&script).unwrap(), original);

    let built = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .args(["--no-progress", "--mod-line-prefix", "mods"])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let result: Value = serde_json::from_slice(&built.stdout).unwrap();
    assert_eq!(result["details"]["modLineFiles"][0]["replaced"], 1);
    assert_eq!(
        std::fs::read_to_string(&script).unwrap(),
        "@echo off\r\narma3server_x64.exe -mod=mods/@mod; -config=server.cfg\r\n"
    );

    std::fs::write(&script, "arma3server_x64.exe -config=server.cfg\r\n").unwrap();
    let missing = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .arg("--no-progress")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let result: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("no -mod parameter"),
        "{}",
        result["error"]
    );
}

#[test]
fn no_mod_line_files_skips_checking_previewing_and_rewriting_launch_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("mods").join("@mod");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("data.txt"), b"data").unwrap();
    let script = dir.path().join("start-server.cmd");
    let original = "@echo off\r\narma3server_x64.exe -mod=mods/@stale; -config=server.cfg\r\n";
    std::fs::write(&script, original).unwrap();
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "Test",
            "basePath": dir.path().join("mods"),
            "requiredMods": [{"modName": "@mod"}],
            "modLineFiles": ["start-server.cmd", "missing-for-test.cmd"]
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");

    let preview = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .args(["--dry-run", "--no-mod-line-files"])
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let result: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert!(
        !result["details"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["action"] == "update-mod-line")
    );

    let built = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .args([
            "--no-progress",
            "--no-mod-line-files",
            "--mod-line-prefix",
            "mods",
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let result: Value = serde_json::from_slice(&built.stdout).unwrap();
    assert!(result["details"].get("modLineFiles").is_none());
    assert_eq!(std::fs::read_to_string(&script).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(output.join("server_mod_line.txt")).unwrap(),
        "-mod=mods/@mod;\n"
    );

    let space = dir.path().join("space.json");
    write_json(
        &space,
        json!({
            "name": "Test Space",
            "baseUrl": "https://example.com/repos/",
            "repositories": [{"config": "config.json", "folder": "main"}]
        }),
    );
    let space_output = dir.path().join("space-output");
    let space_built = Command::new(bin)
        .args(["--json", "create-space"])
        .arg(&space)
        .arg(&space_output)
        .args([
            "--no-progress",
            "--no-mod-line-files",
            "--mod-line-prefix",
            "mods",
        ])
        .output()
        .unwrap();
    assert!(
        space_built.status.success(),
        "{}",
        String::from_utf8_lossy(&space_built.stderr)
    );
    let space_result: Value = serde_json::from_slice(&space_built.stdout).unwrap();
    assert!(space_result["details"].get("modLineFiles").is_none());
    assert_eq!(std::fs::read_to_string(&script).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(space_output.join("main").join("server_mod_line.txt")).unwrap(),
        "-mod=mods/@mod;\n"
    );
}

#[test]
fn nested_disabled_mod_in_config_publishes_flattened_with_enabled_false() {
    let dir = tempfile::tempdir().unwrap();
    let mods = dir.path().join("mods");
    let active = mods.join("@active");
    let nested_disabled = mods.join("folder").join("@mod");
    std::fs::create_dir_all(&active).unwrap();
    std::fs::create_dir_all(&nested_disabled).unwrap();
    std::fs::write(active.join("active.pbo"), b"active").unwrap();
    std::fs::write(nested_disabled.join("mod.pbo"), b"disabled").unwrap();
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "Test",
            "basePath": mods,
            "requiredMods": [
                {"modName": "@active", "enabled": true},
                {"modName": "folder/@mod", "enabled": false}
            ]
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");

    let built = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .args(["--no-progress", "--mode", "hybrid"])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert!(output.join("@mod").join("mod.pbo").exists());
    assert!(!output.join("folder").exists());

    let foxy_addons: Value =
        serde_json::from_slice(&std::fs::read(output.join("foxy_addons.json")).unwrap()).unwrap();
    let foxy_disabled = foxy_addons["requiredMods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["modName"] == "@mod")
        .unwrap();
    assert_eq!(foxy_disabled["enabled"], false);

    let repo_json: Value =
        serde_json::from_slice(&std::fs::read(output.join("repo.json")).unwrap()).unwrap();
    let repo_disabled = repo_json["requiredMods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["modName"] == "@mod")
        .unwrap();
    assert_eq!(repo_disabled["enabled"], false);
    assert_eq!(
        std::fs::read_to_string(output.join("server_mod_line.txt")).unwrap(),
        "-mod=@active;\n"
    );
    assert!(
        Command::new(bin)
            .arg("verify")
            .arg(&output)
            .output()
            .unwrap()
            .status
            .success()
    );
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

#[test]
fn create_space_writes_optional_repository_descriptions_non_destructively() {
    let dir = tempfile::tempdir().unwrap();
    let mods = dir.path().join("mods");
    for (repo_name, mod_name) in [("main", "@main"), ("events", "@events")] {
        let mod_dir = mods.join(mod_name);
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("data.txt"), repo_name.as_bytes()).unwrap();
        write_json(
            &dir.path().join(format!("{repo_name}.json")),
            json!({
                "repoName": repo_name,
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
                {"config": "main.json", "folder": "main", "description": "Core weekly operations modset"},
                {"config": "events.json", "folder": "events", "required": false}
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
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(output.join("repository_space.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["entries"][0]["Description"],
        "Core weekly operations modset"
    );
    assert!(manifest["entries"][1].get("Description").is_none());
    let checksum_with_desc = manifest["spaceChecksum"].as_str().unwrap().to_string();

    write_json(
        &space,
        json!({
            "name": "Test Space",
            "baseUrl": "https://example.com/repos/",
            "repositories": [
                {"config": "main.json", "folder": "main", "description": "Updated operations modset"},
                {"config": "events.json", "folder": "events", "required": false}
            ]
        }),
    );
    let second = Command::new(bin)
        .arg("create-space")
        .arg(&space)
        .arg(&output)
        .arg("--no-progress")
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let updated: Value =
        serde_json::from_slice(&std::fs::read(output.join("repository_space.json")).unwrap())
            .unwrap();
    assert_eq!(
        updated["entries"][0]["Description"],
        "Updated operations modset"
    );
    assert_ne!(
        updated["spaceChecksum"].as_str().unwrap(),
        checksum_with_desc
    );
}

#[test]
fn space_prune_asks_for_yes_only_when_published_optionals_exist() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("mods").join("@one");
    std::fs::create_dir_all(source.join("optionals")).unwrap();
    std::fs::write(source.join("data.txt"), b"one").unwrap();
    std::fs::write(source.join("optionals").join("unused.pbo"), b"unused").unwrap();
    write_json(
        &dir.path().join("one.json"),
        json!({
            "repoName": "one",
            "basePath": dir.path().join("mods"),
            "requiredMods": [{"modName": "@one"}]
        }),
    );
    let space = dir.path().join("space.json");
    write_json(
        &space,
        json!({
            "name": "Test Space",
            "baseUrl": "https://example.com/repos/",
            "repositories": [{"config": "one.json", "folder": "one"}]
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");
    let run = |yes: bool| {
        let mut command = Command::new(bin);
        command
            .args(["--json", "create-space"])
            .arg(&space)
            .arg(&output)
            .args(["--no-progress", "--prune-unused-optionals"]);
        if yes {
            command.arg("--yes");
        }
        command.output().unwrap()
    };

    let fresh = run(false);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    let published = output.join("one").join("@one").join("optionals");
    assert!(output.join("one").join("@one").join("data.txt").exists());
    assert!(!published.exists());

    std::fs::create_dir_all(&published).unwrap();
    std::fs::write(published.join("old.pbo"), b"old").unwrap();
    let refused = run(false);
    assert!(!refused.status.success());
    let result: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert!(result["error"].as_str().unwrap().contains("--yes"));
    assert!(published.join("old.pbo").exists());

    let accepted = run(true);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert!(!published.exists());
    assert!(source.join("optionals").join("unused.pbo").exists());
}

#[test]
fn per_repo_keys_with_additional_keys_skips_the_combined_keys_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mods = dir.path().join("mods");
    for (mod_name, key) in [("@one", "one.bikey"), ("@two", "two.bikey")] {
        std::fs::create_dir_all(mods.join(mod_name).join("keys")).unwrap();
        std::fs::write(mods.join(mod_name).join("data.txt"), mod_name).unwrap();
        std::fs::write(mods.join(mod_name).join("keys").join(key), key).unwrap();
    }
    let extra = dir.path().join("extra-keys");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(extra.join("a3.bikey"), b"a3").unwrap();
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
    let run = |collect: bool| {
        let mut command = Command::new(bin);
        command
            .arg("create-space")
            .arg(&space)
            .arg(&output)
            .args(["--no-progress", "--per-repo-keys", "--additional-keys"])
            .arg(&extra);
        if collect {
            command.arg("--collect-keys");
        }
        let built = command.output().unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
    };

    run(false);
    assert!(!output.join("keys").exists());
    let one_keys = output.join("one").join("keys");
    let two_keys = output.join("two").join("keys");
    assert!(one_keys.join("one.bikey").exists());
    assert!(one_keys.join("a3.bikey").exists());
    assert!(!one_keys.join("two.bikey").exists());
    assert!(two_keys.join("two.bikey").exists());
    assert!(two_keys.join("a3.bikey").exists());

    run(true);
    for key in ["one.bikey", "two.bikey", "a3.bikey"] {
        assert!(output.join("keys").join(key).exists(), "{key}");
    }
}

#[test]
fn wildcard_with_explicit_disabled_override_excludes_mod_from_server_line() {
    let dir = tempfile::tempdir().unwrap();
    let core = dir.path().join("mods").join("core");
    for mod_name in ["@mod_01", "@mod_02"] {
        std::fs::create_dir_all(core.join(mod_name)).unwrap();
        std::fs::write(core.join(mod_name).join("data.txt"), mod_name).unwrap();
    }
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "MainRepo",
            "basePath": dir.path().join("mods"),
            "requiredMods": [
                { "modName": "core/@*" },
                { "modName": "core/@mod_02", "enabled": false }
            ]
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");

    let validated = Command::new(bin)
        .args(["--json", "validate"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        validated.status.success(),
        "{}",
        String::from_utf8_lossy(&validated.stderr)
    );

    let created = Command::new(bin)
        .args(["--json", "create"])
        .arg(&config)
        .arg(&output)
        .args(["--no-progress", "--mod-line-prefix", "mods"])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let mod_line = std::fs::read_to_string(output.join("server_mod_line.txt")).unwrap();
    assert_eq!(mod_line, "-mod=mods/@mod_01;\n");

    let addons: Value =
        serde_json::from_slice(&std::fs::read(output.join("foxy_addons.json")).unwrap()).unwrap();
    let required = addons["requiredMods"].as_array().unwrap();
    assert_eq!(required.len(), 2);
    let mod_02 = required
        .iter()
        .find(|entry| entry["modName"] == "@mod_02")
        .unwrap();
    assert_eq!(mod_02["enabled"], false);
}

#[test]
fn create_prunes_stale_output_keys_but_preserves_external_keys_output() {
    let dir = tempfile::tempdir().unwrap();
    let mods = dir.path().join("mods");
    for (mod_name, key) in [("@mod_01", "one.bikey"), ("@mod_02", "two.bikey")] {
        std::fs::create_dir_all(mods.join(mod_name).join("keys")).unwrap();
        std::fs::write(mods.join(mod_name).join("data.txt"), mod_name).unwrap();
        std::fs::write(mods.join(mod_name).join("keys").join(key), key).unwrap();
    }
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "MainRepo",
            "basePath": mods,
            "requiredMods": [{"modName": "@mod_01"}, {"modName": "@mod_02"}]
        }),
    );
    let output = dir.path().join("output");
    let external_keys = dir.path().join("external-keys");
    std::fs::create_dir_all(&external_keys).unwrap();
    std::fs::write(external_keys.join("manual.bikey"), b"manual").unwrap();
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");

    let run = |extra_args: &[&str]| {
        let mut cmd = Command::new(bin);
        cmd.arg("create")
            .arg(&config)
            .arg(&output)
            .arg("--no-progress")
            .args(extra_args);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run(&["--collect-keys"]);
    assert!(output.join("keys").join("one.bikey").exists());
    assert!(output.join("keys").join("two.bikey").exists());

    write_json(
        &config,
        json!({
            "repoName": "MainRepo",
            "basePath": mods,
            "requiredMods": [{"modName": "@mod_01"}]
        }),
    );

    run(&["--collect-keys"]);
    assert!(output.join("keys").join("one.bikey").exists());
    assert!(!output.join("keys").join("two.bikey").exists());

    let ext = external_keys.to_string_lossy().into_owned();
    run(&["--keys-output", &ext]);
    assert!(external_keys.join("one.bikey").exists());
    assert!(external_keys.join("manual.bikey").exists());
}

#[test]
fn space_clean_removes_orphan_pool_mod_and_stale_keys_without_yes() {
    let dir = tempfile::tempdir().unwrap();
    let mods = dir.path().join("mods");
    for (mod_name, key) in [("@mod_01", "one.bikey"), ("@mod_02", "two.bikey")] {
        std::fs::create_dir_all(mods.join(mod_name).join("keys")).unwrap();
        std::fs::write(mods.join(mod_name).join("data.txt"), mod_name).unwrap();
        std::fs::write(mods.join(mod_name).join("keys").join(key), key).unwrap();
    }
    let repo_cfg = dir.path().join("one.json");
    write_json(
        &repo_cfg,
        json!({
            "repoName": "MainRepo",
            "basePath": mods,
            "requiredMods": [{"modName": "@mod_01"}, {"modName": "@mod_02"}]
        }),
    );
    let space = dir.path().join("space.json");
    write_json(
        &space,
        json!({
            "name": "MainSpace",
            "baseUrl": "https://repo.example.invalid/space/",
            "repositories": [{"config": "one.json", "folder": "one"}]
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");

    let first = Command::new(bin)
        .arg("create-space")
        .arg(&space)
        .arg(&output)
        .args([
            "--layout",
            "pool",
            "--collect-keys",
            "--per-repo-keys",
            "--no-progress",
        ])
        .output()
        .unwrap();
    if !first.status.success() {
        let stderr = String::from_utf8_lossy(&first.stderr);
        if cfg!(windows) && stderr.contains("privilege") {
            return;
        }
        panic!("first create-space failed: {stderr}");
    }
    assert!(output.join("pool").join("@mod_02").exists());
    assert!(output.join("keys").join("two.bikey").exists());
    assert!(output.join("one").join("keys").join("two.bikey").exists());

    write_json(
        &repo_cfg,
        json!({
            "repoName": "MainRepo",
            "basePath": mods,
            "requiredMods": [{"modName": "@mod_01"}]
        }),
    );

    let second = Command::new(bin)
        .arg("create-space")
        .arg(&space)
        .arg(&output)
        .args([
            "--layout",
            "pool",
            "--clean",
            "--collect-keys",
            "--per-repo-keys",
            "--no-progress",
        ])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(output.join("pool").join("@mod_01").exists());
    assert!(!output.join("pool").join("@mod_02").exists());
    assert!(std::fs::symlink_metadata(output.join("one").join("@mod_02")).is_err());
    assert!(output.join("keys").join("one.bikey").exists());
    assert!(!output.join("keys").join("two.bikey").exists());
    assert!(output.join("one").join("keys").join("one.bikey").exists());
    assert!(!output.join("one").join("keys").join("two.bikey").exists());
}

fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn published_parts(output: &Path, mod_name: &str, file: &str) -> Vec<(String, u64)> {
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(output.join(mod_name).join("foxy_addon.json")).unwrap(),
    )
    .unwrap();
    manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == file)
        .unwrap()["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| {
            (
                part["checksum"].as_str().unwrap().to_string(),
                part["length"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn custom_game_build_publishes_content_chunked_parts_that_survive_a_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("source").join("MyGame");
    std::fs::create_dir_all(&game).unwrap();
    let build = pseudo_random_bytes(12 << 20, 5);
    std::fs::write(game.join("game.exe"), &build).unwrap();
    std::fs::write(game.join("readme.txt"), b"play").unwrap();
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "Custom",
            "game": "generic",
            "chunking": "content",
            "basePath": dir.path().join("source"),
            "gameBuild": {"folder": "MyGame", "executable": "game.exe"}
        }),
    );
    let output = dir.path().join("output");
    let bin = env!("CARGO_BIN_EXE_foxy-server-backend-cli");
    let create = || {
        let built = Command::new(bin)
            .args(["--json", "create"])
            .arg(&config)
            .arg(&output)
            .arg("--no-progress")
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        serde_json::from_slice::<Value>(&built.stdout).unwrap()
    };

    let result = create();
    assert!(result["details"]["serverLine"].is_null());
    assert!(!output.join("server_mod_line.txt").exists());
    let repo: Value =
        serde_json::from_slice(&std::fs::read(output.join("repo.json")).unwrap()).unwrap();
    assert_eq!(repo["game"], "generic");
    assert_eq!(repo["chunking"], "content");
    assert_eq!(
        repo["gameBuild"],
        json!({"modName": "MyGame", "executable": "game.exe"})
    );
    let before = published_parts(&output, "MyGame", "game.exe");
    assert!(before.len() > 3);
    assert_eq!(
        before.iter().map(|(_, len)| len).sum::<u64>(),
        build.len() as u64
    );
    assert!(
        Command::new(bin)
            .arg("verify")
            .arg(&output)
            .output()
            .unwrap()
            .status
            .success()
    );

    let mut rebuilt = build[..(4 << 20)].to_vec();
    rebuilt.extend_from_slice(b"patched code");
    rebuilt.extend_from_slice(&build[(4 << 20)..]);
    std::fs::write(game.join("game.exe"), &rebuilt).unwrap();
    create();
    let after = published_parts(&output, "MyGame", "game.exe");
    let reused = after.iter().filter(|part| before.contains(part)).count();
    assert!(
        reused + 2 >= after.len(),
        "reused {reused} of {} parts",
        after.len()
    );
    assert!(
        Command::new(bin)
            .arg("verify")
            .arg(&output)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn custom_game_refuses_mod_line_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("source").join("MyGame")).unwrap();
    std::fs::write(dir.path().join("source").join("MyGame").join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("start.cmd"), "game.exe -mod=x\r\n").unwrap();
    let config = dir.path().join("config.json");
    write_json(
        &config,
        json!({
            "repoName": "Custom",
            "game": "generic",
            "basePath": dir.path().join("source"),
            "requiredMods": [{"modName": "MyGame"}],
            "modLineFiles": ["start.cmd"]
        }),
    );
    let built = Command::new(env!("CARGO_BIN_EXE_foxy-server-backend-cli"))
        .args(["--json", "create"])
        .arg(&config)
        .arg(dir.path().join("output"))
        .arg("--no-progress")
        .output()
        .unwrap();
    assert!(!built.status.success());
    let result: Value = serde_json::from_slice(&built.stdout).unwrap();
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("no server launch line"),
        "{}",
        result["error"]
    );
}

#[test]
fn example_backend_configs_and_manifests_parse() {
    use foxy_server_backend_cli::types::{RepoConfig, RepoGame};
    let _space: foxy_server_backend_cli::space::SpaceConfig = serde_json::from_str(include_str!(
        "../../examples/json/server_backend/space.json"
    ))
    .expect("parse space.json");
    let reforger: RepoConfig = serde_json::from_str(include_str!(
        "../../examples/json/server_backend/reforger_config.json"
    ))
    .expect("parse reforger_config.json");
    assert_eq!(reforger.game, RepoGame::Reforger);
    let generic: RepoConfig = serde_json::from_str(include_str!(
        "../../examples/json/server_backend/generic_config.json"
    ))
    .expect("parse generic_config.json");
    assert_eq!(generic.game, RepoGame::Generic);

    let generated = foxy_server_backend_cli::reforger_addon_fragment(&[
        foxy_server_backend_cli::ReforgerModReference {
            id: "ABCDEF1234567890".into(),
            name: Some("MyReforgerMod".into()),
            version: None,
            required: None,
        },
    ])
    .unwrap();
    let expected: Value = serde_json::from_str(include_str!(
        "../../examples/json/server_backend/reforger_server_mods.json"
    ))
    .expect("parse reforger_server_mods.json");
    let actual: Value = serde_json::from_slice(&generated).unwrap();
    assert_eq!(actual, expected);

    let repo: Value = serde_json::from_str(include_str!(
        "../../examples/json/remote_repositories/repo.json"
    ))
    .expect("parse repo.json");
    assert!(repo["repoName"].is_string());
    assert!(repo["requiredMods"].is_array());
}
