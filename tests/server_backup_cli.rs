use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn invoke(root: &Path, args: &[&str], expected_code: i32) -> Value {
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
        Some(expected_code),
        "args={args:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], expected_code == 0);
    assert!(result["data"].get("database_opened") != Some(&Value::Bool(true)));
    result
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

const SPACE: [&str; 2] = ["--game-space", "arma3"];

fn backup(root: &Path, args: &[&str], expected_code: i32) -> Value {
    let mut full = vec!["server", "backup"];
    full.extend_from_slice(args);
    full.extend_from_slice(&SPACE);
    invoke(root, &full, expected_code)
}

fn instance(root: &Path) -> String {
    let added = invoke(
        root,
        &[
            "server", "instance", "add", "--name", "Main", SPACE[0], SPACE[1],
        ],
        0,
    );
    added["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn seed_instance_files(root: &Path, id: &str) {
    let state = root
        .join("games")
        .join("arma3")
        .join("server/instances")
        .join(id)
        .join("deployments/runtime-state");
    fs::create_dir_all(state.join("home/profiles/Users/FoxyServer")).unwrap();
    fs::create_dir_all(state.join("home/config")).unwrap();
    fs::create_dir_all(state.join("home/private-configuration")).unwrap();
    fs::create_dir_all(state.join("home/battleye")).unwrap();
    fs::create_dir_all(state.join("home/cache")).unwrap();
    fs::create_dir_all(state.join("headless/hc1")).unwrap();
    fs::write(
        state.join("home/profiles/Users/FoxyServer/FoxyServer.Arma3Profile"),
        b"difficulty=\"Custom\";\n",
    )
    .unwrap();
    fs::write(
        state.join("home/config/server.cfg"),
        b"hostname=\"Foxy CLI Test\";\n",
    )
    .unwrap();
    fs::write(
        state.join("home/private-configuration/passwords.json"),
        b"{\"passwordAdmin\":\"secret\"}\n",
    )
    .unwrap();
    fs::write(
        state.join("home/battleye/beserver_x64.cfg"),
        b"RConPassword secret\n",
    )
    .unwrap();
    fs::write(state.join("headless/hc1/join.par"), b"-password=secret\n").unwrap();
    fs::write(state.join("home/cache/temp.bin"), b"transient").unwrap();
    fs::write(state.join("run_old.log"), b"log line\n").unwrap();
}

#[test]
fn server_backup_plan_create_verify_pin_retention_and_prune_roundtrip() {
    let root = fixture();
    let id = instance(root.path());
    seed_instance_files(root.path(), &id);

    let planned = backup(root.path(), &["plan", &id], 0);
    assert_eq!(planned["data"]["database_opened"], false);
    assert_eq!(planned["data"]["writes_performed"], false);
    let plan = &planned["data"]["plan"];
    assert_eq!(plan["consistency"]["mode"], "stopped_verified");
    assert_eq!(plan["consistency"]["stopped_verified"], true);
    assert!(plan["blockers"].as_array().unwrap().is_empty());
    let fp = plan["fingerprint"].as_str().unwrap().to_string();
    assert!(!fp.is_empty());

    let create_dry = backup(
        root.path(),
        &[
            "create",
            &id,
            "--expected-plan-fingerprint",
            &fp,
            "--dry-run",
        ],
        0,
    );
    assert_eq!(create_dry["data"]["writes_performed"], false);
    assert_eq!(create_dry["data"]["dry_run"], true);
    assert!(
        !root
            .path()
            .join("games/arma3/server/instances")
            .join(&id)
            .join("backups")
            .exists()
    );

    backup(
        root.path(),
        &[
            "create",
            &id,
            "--expected-plan-fingerprint",
            "00000000000000000000000000000000",
        ],
        2,
    );

    let created_1 = backup(
        root.path(),
        &["create", &id, "--expected-plan-fingerprint", &fp],
        0,
    );
    assert_eq!(created_1["data"]["writes_performed"], true);
    assert_eq!(created_1["data"]["outcome"]["verified"], true);
    let backup_1 = created_1["data"]["outcome"]["manifest"]["backup_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(backup_1.starts_with("backup_"));
    let entries = created_1["data"]["outcome"]["manifest"]["entries"]
        .as_array()
        .unwrap();
    let archive_paths: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry["archive_path"].as_str())
        .collect();
    assert!(
        archive_paths
            .iter()
            .any(|path| path.ends_with("FoxyServer.Arma3Profile"))
    );
    assert!(
        !archive_paths
            .iter()
            .any(|path| path.contains("private-configuration")
                || path.contains("beserver")
                || path.contains("join.par")
                || path.contains("home/cache")
                || path.contains("run_old.log"))
    );

    let verified = backup(root.path(), &["verify", &id, &backup_1], 0);
    assert_eq!(verified["data"]["verification"]["valid"], true);
    assert_eq!(
        verified["data"]["verification"]["consistency"]["stopped_verified"],
        true
    );

    // Stale generation check on pin fails with exit code 2.
    backup(
        root.path(),
        &["pin", &id, &backup_1, "--expected-generation", "99"],
        2,
    );

    let pinned = backup(
        root.path(),
        &["pin", &id, &backup_1, "--expected-generation", "0"],
        0,
    );
    assert_eq!(pinned["data"]["retention"]["generation"], 1);
    assert_eq!(
        pinned["data"]["retention"]["pinned"].as_array().unwrap(),
        &[Value::String(backup_1.clone())]
    );

    // Create a second stopped-verified backup and a third explicit best-effort live backup.
    let created_2 = backup(root.path(), &["create", &id, "--no-retention"], 0);
    let backup_2 = created_2["data"]["outcome"]["manifest"]["backup_id"]
        .as_str()
        .unwrap()
        .to_string();
    let created_3 = backup(root.path(), &["create", &id, "--live", "--no-retention"], 0);
    let backup_3 = created_3["data"]["outcome"]["manifest"]["backup_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        created_3["data"]["outcome"]["manifest"]["consistency"]["mode"],
        "best_effort_live"
    );
    assert_eq!(
        created_3["data"]["outcome"]["manifest"]["consistency"]["stopped_verified"],
        true
    );

    // Set retention keep_last=1 while backup_1 is pinned.
    let retention = backup(
        root.path(),
        &[
            "retention",
            &id,
            "--expected-generation",
            "1",
            "--keep-last",
            "1",
        ],
        0,
    );
    assert_eq!(retention["data"]["retention"]["generation"], 2);
    assert_eq!(retention["data"]["retention"]["policy"]["keep_last"], 1);

    // Prune plan must keep:
    // - backup_1 (pinned)
    // - backup_2 (newest verified stopped-consistent backup)
    // - backup_3 (newest backup under keep_last=1)
    let prune_plan_pinned = backup(root.path(), &["prune-plan", &id], 0);
    assert_eq!(
        prune_plan_pinned["data"]["plan"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // After unpinning backup_1, keep_last=1 must mark backup_1 for removal while preserving backup_2 and backup_3.
    let unpinned = backup(
        root.path(),
        &["unpin", &id, &backup_1, "--expected-generation", "2"],
        0,
    );
    assert_eq!(unpinned["data"]["retention"]["generation"], 3);

    let prune_plan = backup(root.path(), &["prune-plan", &id], 0);
    let candidates: Vec<&str> = prune_plan["data"]["plan"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["backup_id"].as_str())
        .collect();
    assert_eq!(candidates, vec![backup_1.as_str()]);
    let prune_fp = prune_plan["data"]["plan"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();

    // Pruning without --yes is rejected cleanly with exit code 2.
    backup(
        root.path(),
        &["prune", &id, "--expected-plan-fingerprint", &prune_fp],
        2,
    );

    // Dry run succeeds without deleting backup_1.
    let prune_dry = backup(
        root.path(),
        &[
            "prune",
            &id,
            "--expected-plan-fingerprint",
            &prune_fp,
            "--dry-run",
        ],
        0,
    );
    assert_eq!(prune_dry["data"]["writes_performed"], false);

    // Execute prune with --yes and verify retained list keeps backup_2 and backup_3.
    let pruned = invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "backup",
            "prune",
            &id,
            "--expected-plan-fingerprint",
            &prune_fp,
            SPACE[0],
            SPACE[1],
        ],
        0,
    );
    assert_eq!(pruned["data"]["writes_performed"], true);
    assert!(
        pruned["data"]["outcome"]["receipt_id"]
            .as_str()
            .unwrap()
            .starts_with("prune_")
    );
    let retained: Vec<&str> = pruned["data"]["outcome"]["retained_backups"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(retained.contains(&backup_2.as_str()));
    assert!(retained.contains(&backup_3.as_str()));
    assert!(!retained.contains(&backup_1.as_str()));

    let listed = backup(root.path(), &["list", &id], 0);
    assert_eq!(
        listed["data"]["listing"]["backups"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Corrupting backup_2's archive must fail verify; corrupting its manifest and adding
    // an unrecognized folder must surface both in `list` and keep them untouched by prune-plan.
    let backup_2_dir = root
        .path()
        .join("games/arma3/server/instances")
        .join(&id)
        .join("backups")
        .join(&backup_2);
    let corrupt_archive = backup_2_dir.join("archive.zip");
    fs::write(&corrupt_archive, b"corrupt zip bytes").unwrap();
    backup(root.path(), &["verify", &id, &backup_2], 4);
    fs::write(backup_2_dir.join("manifest.json"), b"{not valid json").unwrap();
    fs::create_dir_all(backup_2_dir.parent().unwrap().join("mystery_entry")).unwrap();
    let after_corrupt = backup(root.path(), &["list", &id], 0);
    assert!(
        after_corrupt["data"]["listing"]["corrupt_backups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some(backup_2.as_str()))
    );
    assert!(
        after_corrupt["data"]["listing"]["unrecognized_entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("mystery_entry"))
    );
    let after_prune_plan = backup(root.path(), &["prune-plan", &id], 0);
    assert!(
        after_prune_plan["data"]["plan"]["candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
