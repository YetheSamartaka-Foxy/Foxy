#![cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn invoke(root: &Path, args: &[&str], expected_code: i32) -> Value {
    invoke_outcome(root, args, expected_code, expected_code == 0)
}

fn invoke_outcome(root: &Path, args: &[&str], expected_code: i32, expected_ok: bool) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .env("FOXY_CONFIG_DIR", root);
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
    assert_eq!(result["ok"], expected_ok);
    assert_eq!(result.as_object().unwrap().len(), 4);
    if !expected_ok {
        assert!(result["data"].is_null());
    }
    result
}

fn selected(root: &Path, space: &str, args: &[&str], expected_code: i32) -> Value {
    let mut args = args.to_vec();
    args.extend(["--game-space", space]);
    invoke(root, &args, expected_code)
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

fn inventory(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();
    if !root.exists() {
        return result;
    }
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let relative = PathBuf::from(path.file_name().unwrap());
        if path.is_dir() {
            result.insert(relative.clone(), None);
            for (nested, bytes) in inventory(&path) {
                result.insert(relative.join(nested), bytes);
            }
        } else {
            result.insert(relative, Some(fs::read(path).unwrap()));
        }
    }
    result
}

fn executable_name(game: &str) -> &'static str {
    match (game, cfg!(windows)) {
        ("arma3", true) => "arma3server_x64.exe",
        ("arma3", false) => "arma3server_x64",
        ("reforger", true) => "ArmaReforgerServer.exe",
        ("reforger", false) => "ArmaReforgerServer",
        _ => unreachable!(),
    }
}

fn native_installation(directory: &Path, game: &str) -> PathBuf {
    fs::create_dir_all(directory).unwrap();
    let mut bytes = [0u8; 128];
    if cfg!(windows) {
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
        bytes[84..86].copy_from_slice(&2u16.to_le_bytes());
        bytes[88..90].copy_from_slice(&0x020bu16.to_le_bytes());
    } else {
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4..7].copy_from_slice(&[2, 1, 1]);
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
    }
    let executable = directory.join(executable_name(game));
    fs::write(&executable, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        directory.join("server.cfg"),
        b"operator-managed configuration",
    )
    .unwrap();
    fs::write(directory.join("unknown.bin"), b"preserve this content").unwrap();
    executable
}

fn add(root: &Path, space: &str) -> String {
    selected(
        root,
        space,
        &["server", "instance", "add", "--name", "MainServer"],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn preflight(root: &Path, space: &str, id: &str) -> Value {
    let result = selected(root, space, &["server", "instance", "preflight", id], 0);
    assert_eq!(result["action"], "server.instance.preflight");
    let preflight = result["data"]["preflight"].clone();
    assert_eq!(preflight["deployment_ready"], false);
    assert_eq!(preflight["writes_performed"], false);
    assert_eq!(preflight["instance_id"], id);
    let lifecycle = matches!(space, "arma3" | "reforger")
        && cfg!(all(
            target_arch = "x86_64",
            any(windows, target_os = "linux")
        ));
    assert!(has_blocker(
        &preflight,
        if lifecycle {
            "foreground_owner_required"
        } else {
            "deployment_unavailable"
        }
    ));
    if lifecycle {
        assert!(!has_blocker(&preflight, "deployment_unavailable"));
    }
    assert!(has_blocker(&preflight, "runtime_isolation_unverified"));
    for blocker in preflight["blockers"].as_array().unwrap() {
        assert!(!blocker["code"].as_str().unwrap().is_empty());
        assert!(!blocker["message"].as_str().unwrap().is_empty());
    }
    preflight
}

fn has_blocker(preflight: &Value, code: &str) -> bool {
    preflight["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|blocker| blocker["code"] == code)
}

fn proposed_paths(preflight: &Value, base: &Path) -> BTreeSet<PathBuf> {
    let mut roles = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for proposed in preflight["paths"].as_array().unwrap() {
        assert!(roles.insert(proposed["role"].as_str().unwrap()));
        let path = PathBuf::from(proposed["path"].as_str().unwrap());
        assert!(path.starts_with(base));
        assert!(!path.exists());
        assert!(paths.insert(path));
    }
    for role in ["runtime", "profiles", "config", "content"] {
        assert!(roles.contains(role));
    }
    paths
}

#[test]
fn installation_inspection_and_empty_listing_are_write_free_for_both_games() {
    let root = fixture();
    let external = tempfile::tempdir().unwrap();
    let before = inventory(root.path());
    for game in ["arma3", "reforger"] {
        let directory = external.path().join(game);
        native_installation(&directory, game);
        let external_before = inventory(external.path());
        let inspected = selected(
            root.path(),
            game,
            &[
                "server",
                "installation",
                "inspect",
                "--directory",
                directory.to_str().unwrap(),
            ],
            0,
        );
        assert_eq!(inspected["action"], "server.installation.inspect");
        let build = &inspected["data"]["build_observation"];
        assert!(build["executable_version"].is_null());
        assert!(build["steam_manifest"].is_null());
        assert!(build["tested_engine_layout"].is_null());
        assert_eq!(build["engine_layout_verified"], false);
        assert_eq!(build["vendor_authenticity_verified"], false);
        assert_eq!(build["package_completeness_verified"], false);
        let installation = &inspected["data"]["installation"];
        assert_eq!(installation["game"], game);
        assert_eq!(installation["ownership"], "adopted");
        assert_eq!(
            installation["build_evidence"],
            "executable_fingerprint_only"
        );
        assert_eq!(installation["executable"], executable_name(game));
        assert_eq!(installation["evidence"]["bytes"], 128);
        assert_eq!(
            installation["evidence"]["blake3"].as_str().unwrap().len(),
            64
        );
        assert_eq!(
            PathBuf::from(installation["directory"].as_str().unwrap()),
            directory.canonicalize().unwrap()
        );
        let listed = selected(root.path(), game, &["server", "installation", "list"], 0);
        assert!(
            listed["data"]["installations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(inventory(root.path()), before);
        assert_eq!(inventory(external.path()), external_before);

        fs::write(directory.join(executable_name(game)), [0u8; 128]).unwrap();
        let invalid_before = inventory(external.path());
        selected(
            root.path(),
            game,
            &[
                "server",
                "installation",
                "inspect",
                "--directory",
                directory.to_str().unwrap(),
            ],
            2,
        );
        assert_eq!(inventory(root.path()), before);
        assert_eq!(inventory(external.path()), invalid_before);
    }
}

#[test]
fn adoption_binding_and_preflight_share_revision_and_isolation_rules_for_both_games() {
    let root = fixture();
    let registry = fs::read(root.path().join("games.json")).unwrap();
    let external = tempfile::tempdir().unwrap();
    for game in ["arma3", "reforger"] {
        let directory = external.path().join(game);
        let executable = native_installation(&directory, game);
        let external_before = inventory(external.path());
        let id = add(root.path(), game);
        let adopt_args = [
            "server",
            "instance",
            "adopt",
            &id,
            "--directory",
            directory.to_str().unwrap(),
            "--expected-revision",
            "1",
        ];
        let before = inventory(root.path());
        let mut dry_adopt = adopt_args.to_vec();
        dry_adopt.push("--dry-run");
        let preview = selected(root.path(), game, &dry_adopt, 0);
        assert_eq!(preview["data"]["migrates_from_v1"], false);
        assert_eq!(inventory(root.path()), before);
        let adopted = selected(root.path(), game, &adopt_args, 0);
        assert_eq!(adopted["data"]["instance"]["revision"], 2);
        assert_eq!(adopted["data"]["migrates_from_v1"], false);
        let installation_id = adopted["data"]["instance"]["installation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let before_consumers = inventory(root.path());
        let inspected_consumers = selected(
            root.path(),
            game,
            &["server", "installation", "consumers", &installation_id],
            0,
        );
        let consumers = &inspected_consumers["data"]["consumer_inspection"];
        assert_eq!(consumers["inspection_complete"], true);
        assert_eq!(consumers["cleanup_required"], false);
        assert_eq!(consumers["recorded_consumers"], serde_json::json!([]));
        assert_eq!(consumers["liveness_verified"], false);
        assert_eq!(consumers["exclusive_maintenance_authorized"], false);
        assert_eq!(consumers["writes_performed"], false);
        let recovery = &inspected_consumers["data"]["recovery_inspection"];
        assert_eq!(recovery["inspection_complete"], true);
        assert_eq!(recovery["maintenance_status"], "clear");
        assert_eq!(recovery["receipt_count"], 0);
        assert_eq!(recovery["consumers"], *consumers);
        for field in [
            "activation_available",
            "resolution_available",
            "liveness_verified",
            "installation_leases_acquired",
            "archives_verified",
            "writes_performed",
        ] {
            assert_eq!(recovery[field], false, "{field}");
        }
        assert_eq!(inventory(root.path()), before_consumers);
        assert_eq!(inventory(external.path()), external_before);
        let review = preflight(root.path(), game, &id);
        assert_eq!(review["installation_consumers"], *consumers);
        assert_eq!(review["installation_recovery"], *recovery);
        let moved = external.path().join(format!("{game}-moved"));
        fs::rename(&directory, &moved).unwrap();
        let moved_before = inventory(external.path());
        let unknown = invoke_outcome(
            root.path(),
            &[
                "server",
                "installation",
                "consumers",
                &installation_id,
                "--game-space",
                game,
            ],
            4,
            true,
        );
        let unknown = &unknown["data"]["consumer_inspection"];
        assert_eq!(unknown["inspection_complete"], false);
        assert!(unknown["cleanup_required"].is_null());
        assert!(unknown["inspection_error"].is_string());
        assert_eq!(unknown["liveness_verified"], false);
        assert_eq!(unknown["exclusive_maintenance_authorized"], false);
        assert_eq!(unknown["writes_performed"], false);
        let incomplete = preflight(root.path(), game, &id);
        assert_eq!(incomplete["installation_consumers"], *unknown);
        assert!(has_blocker(&incomplete, "consumer_inspection_incomplete"));
        assert_eq!(
            incomplete["installation_recovery"]["inspection_complete"],
            false
        );
        assert_eq!(
            incomplete["installation_recovery"]["maintenance_status"],
            "unknown"
        );
        assert!(has_blocker(&incomplete, "maintenance_recovery_unknown"));
        assert_eq!(inventory(root.path()), before_consumers);
        assert_eq!(inventory(external.path()), moved_before);
        fs::rename(&moved, &directory).unwrap();
        selected(
            root.path(),
            game,
            &[
                "server",
                "installation",
                "consumers",
                "install_00000000000000000000000000000000",
            ],
            3,
        );
        selected(
            root.path(),
            game,
            &["server", "installation", "consumers", "../invalid"],
            2,
        );
        let shown = selected(root.path(), game, &["server", "instance", "show", &id], 0);
        assert_eq!(shown["data"]["instance"], adopted["data"]["instance"]);
        let before_stale = inventory(root.path());
        selected(root.path(), game, &adopt_args, 2);
        assert_eq!(inventory(root.path()), before_stale);

        let cloned = selected(
            root.path(),
            game,
            &[
                "server",
                "instance",
                "clone",
                &id,
                "--name",
                "SecondServer",
                "--expected-revision",
                "2",
            ],
            0,
        );
        let sibling = cloned["data"]["instance"]["id"].as_str().unwrap();
        assert_ne!(sibling, id);
        assert_eq!(
            cloned["data"]["instance"]["installation_id"],
            installation_id
        );
        let canonical_alias = directory.join(".");
        let deduplicated = selected(
            root.path(),
            game,
            &[
                "server",
                "instance",
                "adopt",
                sibling,
                "--directory",
                canonical_alias.to_str().unwrap(),
                "--expected-revision",
                "1",
            ],
            0,
        );
        assert_eq!(
            deduplicated["data"]["instance"]["installation_id"],
            installation_id
        );
        let listed = selected(root.path(), game, &["server", "installation", "list"], 0);
        assert_eq!(listed["data"]["installations"].as_array().unwrap().len(), 1);
        assert_eq!(listed["data"]["installations"][0]["id"], installation_id);

        let before_preflight = inventory(root.path());
        let first = preflight(root.path(), game, &id);
        let second = preflight(root.path(), game, sibling);
        assert!(has_blocker(&first, "port_collision"));
        assert!(has_blocker(&second, "port_collision"));
        assert_eq!(first["installation"]["id"], installation_id);
        assert_eq!(second["installation"]["id"], installation_id);
        let instance_root = root
            .path()
            .join("games")
            .join(game)
            .join("server/instances");
        let first_paths = proposed_paths(&first, &instance_root.join(&id));
        let second_paths = proposed_paths(&second, &instance_root.join(sibling));
        assert!(first_paths.is_disjoint(&second_paths));
        assert_eq!(inventory(root.path()), before_preflight);

        let cleared = selected(
            root.path(),
            game,
            &[
                "server",
                "instance",
                "bind",
                sibling,
                "--clear",
                "--expected-revision",
                "2",
            ],
            0,
        );
        assert!(cleared["data"]["instance"]["installation_id"].is_null());
        assert_eq!(cleared["data"]["instance"]["revision"], 3);
        let before_bind = inventory(root.path());
        assert!(has_blocker(
            &preflight(root.path(), game, sibling),
            "installation_missing"
        ));
        selected(
            root.path(),
            game,
            &[
                "server",
                "instance",
                "bind",
                sibling,
                "--clear",
                "--expected-revision",
                "2",
            ],
            2,
        );
        let bind_args = [
            "server",
            "instance",
            "bind",
            sibling,
            "--installation",
            &installation_id,
            "--expected-revision",
            "3",
        ];
        let mut dry_bind = bind_args.to_vec();
        dry_bind.push("--dry-run");
        selected(root.path(), game, &dry_bind, 0);
        assert_eq!(inventory(root.path()), before_bind);
        let rebound = selected(root.path(), game, &bind_args, 0);
        assert_eq!(
            rebound["data"]["instance"]["installation_id"],
            installation_id
        );
        assert_eq!(rebound["data"]["instance"]["revision"], 4);
        assert_eq!(inventory(external.path()), external_before);

        fs::OpenOptions::new()
            .append(true)
            .open(executable)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        let drifted_external = inventory(external.path());
        let before_drift = inventory(root.path());
        let drifted = preflight(root.path(), game, &id);
        assert!(has_blocker(&drifted, "installation_changed"));
        selected(
            root.path(),
            game,
            &[
                "server",
                "instance",
                "bind",
                sibling,
                "--installation",
                &installation_id,
                "--expected-revision",
                "4",
            ],
            2,
        );
        selected(
            root.path(),
            game,
            &[
                "server",
                "instance",
                "adopt",
                sibling,
                "--directory",
                directory.to_str().unwrap(),
                "--expected-revision",
                "4",
            ],
            2,
        );
        assert_eq!(inventory(root.path()), before_drift);
        assert_eq!(inventory(external.path()), drifted_external);
        assert!(
            !root
                .path()
                .join("games")
                .join(game)
                .join("database.db")
                .exists()
        );
    }
    assert_eq!(fs::read(root.path().join("games.json")).unwrap(), registry);
    assert!(!root.path().join("logs").exists());
    assert!(!root.path().join("app_settings.json").exists());
}

#[test]
fn adopting_a_legacy_draft_migrates_only_on_apply_and_keeps_its_exact_backup() {
    let root = fixture();
    let external = tempfile::tempdir().unwrap();
    native_installation(external.path(), "arma3");
    let id = add(root.path(), "arma3");
    let catalog_path = root.path().join("games/arma3/server/servers.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    legacy["schema_version"] = 1.into();
    legacy.as_object_mut().unwrap().remove("installations");
    for instance in legacy["instances"].as_array_mut().unwrap() {
        instance["schema_version"] = 1.into();
        instance.as_object_mut().unwrap().remove("installation_id");
    }
    let legacy_bytes = serde_json::to_vec_pretty(&legacy).unwrap();
    fs::write(&catalog_path, &legacy_bytes).unwrap();
    let before = inventory(root.path());
    let external_before = inventory(external.path());
    selected(root.path(), "arma3", &["server", "installation", "list"], 0);
    assert!(has_blocker(
        &preflight(root.path(), "arma3", &id),
        "installation_missing"
    ));
    let adopt_args = [
        "server",
        "instance",
        "adopt",
        &id,
        "--directory",
        external.path().to_str().unwrap(),
        "--expected-revision",
        "1",
    ];
    let mut dry_args = adopt_args.to_vec();
    dry_args.push("--dry-run");
    let preview = selected(root.path(), "arma3", &dry_args, 0);
    assert_eq!(preview["data"]["migrates_from_v1"], true);
    assert_eq!(inventory(root.path()), before);
    let saved = selected(root.path(), "arma3", &adopt_args, 0);
    assert_eq!(saved["data"]["migrates_from_v1"], true);
    assert_eq!(saved["data"]["instance"]["revision"], 2);
    let catalog: Value = serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    assert_eq!(catalog["schema_version"], 2);
    assert_eq!(catalog["installations"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["instances"][0]["installation_id"],
        catalog["installations"][0]["id"]
    );
    assert_eq!(
        catalog["instances"][0]["definition"],
        legacy["instances"][0]["definition"]
    );
    let backups = fs::read_dir(catalog_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("servers.v1.")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), legacy_bytes);
    let after = inventory(root.path());
    preflight(root.path(), "arma3", &id);
    assert_eq!(inventory(root.path()), after);
    assert_eq!(inventory(external.path()), external_before);
}

#[test]
fn installation_commands_reject_unsupported_spaces_wipes_and_overlapping_roots_without_bootstrap() {
    let root = fixture();
    let external = tempfile::tempdir().unwrap();
    native_installation(external.path(), "arma3");
    let directory = external.path().to_str().unwrap();
    let id = "server_00000000000000000000000000000001";
    let before = inventory(root.path());
    let external_before = inventory(external.path());
    for space in ["generic", "twwh3"] {
        selected(root.path(), space, &["server", "installation", "list"], 2);
        selected(
            root.path(),
            space,
            &[
                "server",
                "installation",
                "inspect",
                "--directory",
                directory,
            ],
            2,
        );
        selected(
            root.path(),
            space,
            &["server", "instance", "preflight", id],
            2,
        );
    }
    for args in [
        vec!["server", "installation", "list"],
        vec![
            "server",
            "installation",
            "inspect",
            "--directory",
            directory,
        ],
        vec!["server", "instance", "preflight", id],
        vec![
            "server",
            "installation",
            "consumers",
            "install_00000000000000000000000000000000",
        ],
        vec![
            "server",
            "instance",
            "adopt",
            id,
            "--directory",
            directory,
            "--expected-revision",
            "1",
        ],
        vec![
            "server",
            "instance",
            "bind",
            id,
            "--clear",
            "--expected-revision",
            "1",
        ],
    ] {
        let mut args_with_wipe = vec!["--wipe-db", "--yes"];
        args_with_wipe.extend(args);
        invoke(root.path(), &args_with_wipe, 2);
    }
    assert_eq!(inventory(root.path()), before);
    assert_eq!(inventory(external.path()), external_before);
    let missing = root.path().join("missing");
    invoke(&missing, &["server", "installation", "list"], 2);
    assert!(!missing.exists());

    native_installation(root.path(), "arma3");
    let overlap_before = inventory(root.path());
    invoke(
        root.path(),
        &[
            "server",
            "installation",
            "inspect",
            "--directory",
            root.path().to_str().unwrap(),
        ],
        2,
    );
    assert_eq!(inventory(root.path()), overlap_before);
}
