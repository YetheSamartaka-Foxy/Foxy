use super::*;
use std::fs;

fn fixture() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp directory")
}

fn seed(root: &Path) {
    fs::write(root.join("arma3server_x64"), b"engine").unwrap();
    fs::write(root.join("steam_appid.txt"), b"107410\n").unwrap();
    fs::write(root.join("ignored.cfg"), b"private config").unwrap();
    fs::create_dir(root.join("addons")).unwrap();
    fs::write(root.join("addons/core.pbo"), b"addon").unwrap();
    fs::create_dir(root.join("profiles")).unwrap();
    fs::write(root.join("profiles/private"), b"profile").unwrap();
}

fn arma() -> ServerDefinitionKind {
    ServerDefinitionKind::Arma3
}

#[test]
fn reforger_selects_only_game_and_platform_payload_roots() {
    for (platform, executable, selected_library, wrong_library) in [
        (
            ServerPlatform::WindowsX64,
            "ArmaReforgerServer.exe",
            "steam_api64.dll",
            "libunowned.so",
        ),
        (
            ServerPlatform::LinuxX64,
            "ArmaReforgerServer",
            "libReforger.so",
            "unowned.dll",
        ),
    ] {
        let source = fixture();
        fs::write(source.path().join(executable), b"engine").unwrap();
        fs::write(source.path().join("steam_appid.txt"), b"1874880\n").unwrap();
        fs::write(source.path().join("steam_api64.dll"), b"dll").unwrap();
        fs::write(source.path().join("libReforger.so"), b"so").unwrap();
        fs::write(source.path().join(wrong_library), b"wrong platform").unwrap();
        fs::write(source.path().join("ArmaReforgerDiag.exe"), b"diagnostics").unwrap();
        fs::write(source.path().join("crashreporter.exe"), b"crashes").unwrap();
        fs::write(source.path().join("server.json"), b"configuration").unwrap();
        fs::write(source.path().join("unknown.cfg"), b"unknown config").unwrap();
        fs::create_dir(source.path().join("docs")).unwrap();
        fs::write(source.path().join("docs/readme.txt"), b"documentation").unwrap();
        for directory in ["addons", "battleye"] {
            fs::create_dir(source.path().join(directory)).unwrap();
            fs::write(
                source.path().join(directory).join("payload.bin"),
                directory.as_bytes(),
            )
            .unwrap();
        }
        fs::create_dir(source.path().join("profiles")).unwrap();
        fs::write(source.path().join("profiles/private"), b"private").unwrap();

        let control = ContentStageControl::default();
        let game = ServerDefinitionKind::Reforger;
        let inventory = inventory(game, source.path(), platform, &control).unwrap();
        let selected: Vec<_> = inventory
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(selected.contains(&executable));
        assert!(selected.contains(&"addons/payload.bin"));
        assert!(selected.contains(&"battleye/payload.bin"));
        assert!(selected.contains(&selected_library));
        assert!(!selected.contains(&wrong_library));
        for excluded in [
            "ArmaReforgerDiag.exe",
            "crashreporter.exe",
            "server.json",
            "unknown.cfg",
            "docs/readme.txt",
            "profiles/private",
        ] {
            assert!(!selected.contains(&excluded));
        }

        let destination = fixture();
        let output = destination.path().join("payload");
        copy(game, source.path(), &output, &inventory, &control).unwrap();
        assert!(!output.join("unknown.cfg").exists());
        assert!(!output.join("docs").exists());
        assert!(!output.join("ArmaReforgerDiag.exe").exists());
    }
}

#[test]
fn selected_executable_is_required_for_the_requested_game() {
    let source = fixture();
    fs::write(source.path().join("arma3server_x64"), b"engine").unwrap();
    assert_eq!(
        inventory(
            ServerDefinitionKind::Reforger,
            source.path(),
            ServerPlatform::LinuxX64,
            &ContentStageControl::default(),
        ),
        Err(ServerStoreError::Corrupt)
    );

    let arma_inventory = inventory(
        arma(),
        source.path(),
        ServerPlatform::LinuxX64,
        &ContentStageControl::default(),
    )
    .unwrap();
    assert_eq!(
        validate_inventory(ServerDefinitionKind::Reforger, &arma_inventory),
        Err(ServerStoreError::Corrupt)
    );
}

#[test]
fn arma_inventory_serialization_keeps_its_existing_shape() {
    let source = fixture();
    fs::write(source.path().join("arma3server_x64"), b"engine").unwrap();
    let inventory = inventory(
        arma(),
        source.path(),
        ServerPlatform::LinuxX64,
        &ContentStageControl::default(),
    )
    .unwrap();
    let value = serde_json::to_value(inventory).unwrap();
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["platform", "entries", "total_bytes"]
    );
    assert_eq!(value["platform"], "linux_x64");
    assert_eq!(value["entries"][0]["path"], "arma3server_x64");
    assert_eq!(value["entries"][0]["kind"], "file");
    assert_eq!(value["total_bytes"], 6);
}

#[test]
fn selects_native_payload_and_excludes_configuration_and_profiles() {
    let source = fixture();
    seed(source.path());
    let inv = inventory(
        arma(),
        source.path(),
        ServerPlatform::LinuxX64,
        &ContentStageControl::default(),
    )
    .unwrap();
    let paths: Vec<_> = inv.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"arma3server_x64"));
    assert!(paths.contains(&"addons/core.pbo"));
    assert!(
        !paths
            .iter()
            .any(|p| p.starts_with("profiles/") || p == &"ignored.cfg")
    );
}

#[cfg(unix)]
#[test]
fn includes_linux64_and_preserves_executable_intent() {
    use std::os::unix::fs::PermissionsExt;
    let source = fixture();
    seed(source.path());
    fs::set_permissions(
        source.path().join("arma3server_x64"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    fs::create_dir(source.path().join("linux64")).unwrap();
    fs::write(source.path().join("linux64/libsteam_api.so"), b"library").unwrap();
    let inv = inventory(
        arma(),
        source.path(),
        ServerPlatform::LinuxX64,
        &ContentStageControl::default(),
    )
    .unwrap();
    assert!(
        inv.entries
            .iter()
            .any(|e| e.path == "linux64/libsteam_api.so")
    );
    assert!(
        inv.entries
            .iter()
            .find(|e| e.path == "arma3server_x64")
            .unwrap()
            .executable
    );
    let destination = source
        .path()
        .parent()
        .unwrap()
        .join(format!("payload-{}", rand::random::<u64>()));
    copy(
        arma(),
        source.path(),
        &destination,
        &inv,
        &ContentStageControl::default(),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(destination.join("arma3server_x64"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn changed_source_fails_copy_and_verification() {
    let source = fixture();
    seed(source.path());
    let inv = inventory(
        arma(),
        source.path(),
        ServerPlatform::LinuxX64,
        &ContentStageControl::default(),
    )
    .unwrap();
    fs::write(source.path().join("addons/core.pbo"), b"changed").unwrap();
    assert_eq!(
        copy(
            arma(),
            source.path(),
            &source
                .path()
                .parent()
                .unwrap()
                .join(format!("out-{}", rand::random::<u64>())),
            &inv,
            &ContentStageControl::default()
        ),
        Err(ServerStoreError::StaleRevision)
    );
}

#[test]
fn cancellation_stops_inventory() {
    let source = fixture();
    seed(source.path());
    let control = ContentStageControl::default();
    control.cancel();
    assert_eq!(
        inventory(arma(), source.path(), ServerPlatform::LinuxX64, &control),
        Err(ServerStoreError::Cancelled)
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_selected_directory() {
    use std::os::unix::fs::symlink;
    let source = fixture();
    seed(source.path());
    fs::remove_dir_all(source.path().join("addons")).unwrap();
    let outside = fixture();
    fs::write(outside.path().join("secret"), b"x").unwrap();
    symlink(outside.path(), source.path().join("addons")).unwrap();
    assert_eq!(
        inventory(
            arma(),
            source.path(),
            ServerPlatform::LinuxX64,
            &ContentStageControl::default()
        ),
        Err(ServerStoreError::UnsafePath)
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_selected_root_even_with_valid_executable() {
    use std::os::unix::fs::symlink;
    let source = fixture();
    fs::write(source.path().join("arma3server_x64"), b"engine").unwrap();
    let outside = fixture();
    symlink(outside.path(), source.path().join("keys")).unwrap();
    assert_eq!(
        inventory(
            arma(),
            source.path(),
            ServerPlatform::LinuxX64,
            &ContentStageControl::default()
        ),
        Err(ServerStoreError::UnsafePath)
    );
}

#[test]
fn includes_versioned_linux_shared_libraries() {
    let source = fixture();
    seed(source.path());
    fs::write(source.path().join("libsteam_api.so.1"), b"versioned").unwrap();
    let inv = inventory(
        arma(),
        source.path(),
        ServerPlatform::LinuxX64,
        &ContentStageControl::default(),
    )
    .unwrap();
    assert!(inv.entries.iter().any(|e| e.path == "libsteam_api.so.1"));
}

#[test]
fn copies_dlc_expansion_and_loader_directories() {
    let source = fixture();
    seed(source.path());
    for folder in ["Expansion", "Dll", "Enoch", "Tank"] {
        fs::create_dir(source.path().join(folder)).unwrap();
        fs::write(
            source.path().join(folder).join("content.bin"),
            folder.as_bytes(),
        )
        .unwrap();
    }
    let control = ContentStageControl::default();
    let inv = inventory(arma(), source.path(), ServerPlatform::LinuxX64, &control).unwrap();
    let destination = fixture();
    let output = destination.path().join("engine");
    copy(arma(), source.path(), &output, &inv, &control).unwrap();
    for folder in ["Expansion", "Dll", "Enoch", "Tank"] {
        assert_eq!(
            fs::read(output.join(folder).join("content.bin")).unwrap(),
            folder.as_bytes()
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_case_colliding_selected_roots() {
    let source = fixture();
    seed(source.path());
    fs::create_dir(source.path().join("Addons")).unwrap();
    assert_eq!(
        inventory(
            arma(),
            source.path(),
            ServerPlatform::LinuxX64,
            &ContentStageControl::default()
        ),
        Err(ServerStoreError::Corrupt)
    );
}

#[test]
fn rejects_portable_case_collisions_from_serialized_inventory() {
    let directory = |path: &str| PayloadEntry {
        path: path.into(),
        kind: StageEntryKind::Directory,
        bytes: 0,
        blake3: None,
        executable: false,
    };
    let executable = PayloadEntry {
        path: "arma3server_x64".into(),
        kind: StageEntryKind::File,
        bytes: 0,
        blake3: Some(blake3::hash(b"").to_hex().to_string()),
        executable: false,
    };
    let inventory = PayloadInventory {
        platform: ServerPlatform::LinuxX64,
        entries: vec![directory("Addons"), directory("addons"), executable],
        total_bytes: 0,
    };
    assert_eq!(
        validate_inventory(arma(), &inventory),
        Err(ServerStoreError::Corrupt)
    );
}

#[test]
fn rejects_malformed_and_overflowing_inventories() {
    let malformed = PayloadInventory {
        platform: ServerPlatform::LinuxX64,
        entries: vec![PayloadEntry {
            path: "../escape".into(),
            kind: StageEntryKind::File,
            bytes: 1,
            blake3: Some("0".repeat(64)),
            executable: false,
        }],
        total_bytes: 1,
    };
    assert_eq!(
        validate_inventory(arma(), &malformed),
        Err(ServerStoreError::Corrupt)
    );
    let overflow = PayloadInventory {
        platform: ServerPlatform::LinuxX64,
        entries: vec![
            PayloadEntry {
                path: "arma3server_x64".into(),
                kind: StageEntryKind::File,
                bytes: u64::MAX,
                blake3: Some("0".repeat(64)),
                executable: false,
            },
            PayloadEntry {
                path: "steam_appid.txt".into(),
                kind: StageEntryKind::File,
                bytes: 1,
                blake3: Some("0".repeat(64)),
                executable: false,
            },
        ],
        total_bytes: 0,
    };
    assert_eq!(
        validate_inventory(arma(), &overflow),
        Err(ServerStoreError::Corrupt)
    );
    let parentless = PayloadInventory {
        platform: ServerPlatform::LinuxX64,
        entries: vec![
            PayloadEntry {
                path: "addons/core.pbo".into(),
                kind: StageEntryKind::File,
                bytes: 1,
                blake3: Some("0".repeat(64)),
                executable: false,
            },
            PayloadEntry {
                path: "arma3server_x64".into(),
                kind: StageEntryKind::File,
                bytes: 0,
                blake3: Some(blake3::hash(b"").to_hex().to_string()),
                executable: false,
            },
        ],
        total_bytes: 1,
    };
    assert_eq!(
        validate_inventory(arma(), &parentless),
        Err(ServerStoreError::Corrupt)
    );
}
