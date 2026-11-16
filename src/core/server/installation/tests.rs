use super::*;
use std::fs;

#[test]
fn build_inspection_rejects_foreign_and_changed_executable_evidence_without_writes() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("games.json"), br#"{"active_game_space_id":"alpha","last_opened_game_space_id":"alpha","game_spaces":[{"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0}]}"#).unwrap();
    let context = crate::core::server::ServerContext::capture(root.path(), Some("alpha")).unwrap();
    let store = crate::core::server::ServerStore::new(context).unwrap();
    let external = tempfile::tempdir().unwrap();
    let executable = create_native_install(external.path(), ServerDefinitionKind::Arma3);
    let installation = store.inspect_installation(external.path()).unwrap();
    let before = inventory(root.path());
    let observed = store.inspect_installation_build(&installation).unwrap();
    assert!(observed.executable_version.is_none());
    assert!(observed.steam_manifest.is_none());
    assert!(!observed.engine_layout_verified);
    let mut foreign = installation.clone();
    foreign.game = ServerDefinitionKind::Reforger;
    assert!(matches!(
        store.inspect_installation_build(&foreign),
        Err(ServerStoreError::StaleRevision)
    ));
    let mut bytes = fs::read(&executable).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(executable, bytes).unwrap();
    assert!(matches!(
        store.inspect_installation_build(&installation),
        Err(ServerStoreError::StaleRevision)
    ));
    assert_eq!(inventory(root.path()), before);
}

#[test]
fn build_observations_only_use_the_captured_installations_manifest() {
    let root = tempfile::tempdir().unwrap();
    let steamapps = root.path().join("steamapps");
    let directory = steamapps.join("common/ServerAlpha");
    fs::create_dir_all(&directory).unwrap();
    let platform = ServerPlatform::current().unwrap();
    let executable = directory.join(executable_name(ServerDefinitionKind::Arma3, platform));
    fs::write(&executable, executable_bytes(platform)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let installation =
        ServerInstallation::inspect(&directory, ServerDefinitionKind::Arma3).unwrap();
    let manifest = steamapps.join("appmanifest_233780.acf");
    let bytes = br#""AppState" { "appid" "233780" "buildid" "246810" "installdir" "ServerAlpha" "UserConfig" { "BetaKey" "" } }"#;
    fs::write(&manifest, bytes).unwrap();
    let observed = observe_build(&installation);
    let steam = observed.steam_manifest.unwrap();
    assert_eq!(steam.app_id, 233780);
    assert_eq!(steam.build_id, "246810");
    assert_eq!(steam.branch.as_deref(), Some("public"));
    assert!(!steam.branch_conflict);
    assert!(!observed.engine_layout_verified);
    assert!(!observed.vendor_authenticity_verified);
    assert!(!observed.package_completeness_verified);
    assert_eq!(fs::read(&manifest).unwrap(), bytes);
    fs::write(
        &manifest,
        br#""AppState" { "appid" "233780" "buildid" "246810" "installdir" "ServerBeta" }"#,
    )
    .unwrap();
    fs::create_dir(steamapps.join("common/ServerBeta")).unwrap();
    assert!(observe_build(&installation).steam_manifest.is_none());
    fs::write(&manifest, bytes).unwrap();
    fs::write(
        steamapps.join("appmanifest_107410.acf"),
        br#""AppState" { "appid" "107410" "buildid" "246811" "installdir" "ServerAlpha" }"#,
    )
    .unwrap();
    assert!(observe_build(&installation).steam_manifest.is_none());
    installation.verify().unwrap();
}

#[test]
fn overlap_checks_use_path_components_and_windows_prefix_aliases() {
    assert!(paths_overlap(
        Path::new("/instance"),
        Path::new("/instance/config")
    ));
    assert!(!paths_overlap(
        Path::new("/instance"),
        Path::new("/instance-other")
    ));
    #[cfg(windows)]
    {
        assert!(paths_overlap(
            Path::new(r"\\?\R:\Mods\Server"),
            Path::new(r"r:\mods\server\config")
        ));
        assert!(paths_overlap(
            Path::new(r"\\?\UNC\host\share\Server"),
            Path::new(r"\\host\share\server\config")
        ));
    }
}

fn pe_header() -> [u8; 26] {
    let mut header = [0; 26];
    header[..4].copy_from_slice(b"PE\0\0");
    header[4..6].copy_from_slice(&0x8664u16.to_le_bytes());
    header[6..8].copy_from_slice(&1u16.to_le_bytes());
    header[20..22].copy_from_slice(&2u16.to_le_bytes());
    header[22..24].copy_from_slice(&0x0022u16.to_le_bytes());
    header[24..26].copy_from_slice(&0x020bu16.to_le_bytes());
    header
}

fn elf_header() -> [u8; 64] {
    let mut header = [0; 64];
    header[..4].copy_from_slice(b"\x7fELF");
    header[4..7].copy_from_slice(&[2, 1, 1]);
    header[16..18].copy_from_slice(&2u16.to_le_bytes());
    header[18..20].copy_from_slice(&62u16.to_le_bytes());
    header[20..24].copy_from_slice(&1u32.to_le_bytes());
    header[52..54].copy_from_slice(&64u16.to_le_bytes());
    header
}

fn executable_bytes(platform: ServerPlatform) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    match platform {
        ServerPlatform::WindowsX64 => {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
            bytes[64..90].copy_from_slice(&pe_header());
        }
        ServerPlatform::LinuxX64 => bytes[..64].copy_from_slice(&elf_header()),
    }
    bytes
}

pub(crate) fn create_native_install(directory: &Path, game: ServerDefinitionKind) -> PathBuf {
    let platform = ServerPlatform::current().unwrap();
    fs::create_dir_all(directory).unwrap();
    let executable = directory.join(executable_name(game, platform));
    fs::write(&executable, executable_bytes(platform)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
    executable
}

fn inventory(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn visit(root: &Path, directory: &Path, entries: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                entries.push((relative, None));
                visit(root, &path, entries);
            } else {
                entries.push((relative, Some(fs::read(path).unwrap())));
            }
        }
    }
    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

#[test]
fn installation_ids_are_strict_and_round_trip_through_json() {
    let id = InstallationId::new();
    assert_eq!(id.as_str().len(), 40);
    assert_eq!(
        InstallationId::try_from(id.as_str().to_string()).unwrap(),
        id
    );
    assert_eq!(
        serde_json::from_slice::<InstallationId>(&serde_json::to_vec(&id).unwrap()).unwrap(),
        id
    );
    for invalid in [
        String::new(),
        "install_".to_string(),
        format!("install_{}", "a".repeat(31)),
        format!("install_{}", "a".repeat(33)),
        format!("install_{}", "A".repeat(32)),
        format!("install_{}", "g".repeat(32)),
        format!("server_{}", "a".repeat(32)),
        format!("../install_{}", "a".repeat(32)),
        format!("install_{}", "é".repeat(16)),
    ] {
        assert!(InstallationId::try_from(invalid.clone()).is_err());
        assert!(
            serde_json::from_slice::<InstallationId>(&serde_json::to_vec(&invalid).unwrap())
                .is_err()
        );
    }
}

#[test]
fn pe_headers_require_x64_signature_sections_and_pe32_plus() {
    let valid = pe_header();
    assert!(valid_pe(&valid));
    for (offset, replacement) in [(0, 0), (4, 0x4c), (5, 0x01), (24, 0), (25, 0x01)] {
        let mut invalid = valid;
        invalid[offset] = replacement;
        assert!(!valid_pe(&invalid));
    }
    let mut no_sections = valid;
    no_sections[6..8].fill(0);
    assert!(!valid_pe(&no_sections));
    for length in [0u16, 1] {
        let mut short_optional_header = valid;
        short_optional_header[20..22].copy_from_slice(&length.to_le_bytes());
        assert!(!valid_pe(&short_optional_header));
    }
    assert!(!valid_pe(&[0; 26]));
}

#[test]
fn elf_headers_require_little_endian_x64_executable_or_shared_object() {
    let valid = elf_header();
    assert!(valid_elf(&valid));
    let mut shared_object = valid;
    shared_object[16..18].copy_from_slice(&3u16.to_le_bytes());
    assert!(valid_elf(&shared_object));
    for (offset, replacement) in [(0, 0), (4, 1), (5, 2), (6, 0), (18, 3), (19, 1)] {
        let mut invalid = valid;
        invalid[offset] = replacement;
        assert!(!valid_elf(&invalid));
    }
    for kind in [0u16, 1, 4] {
        let mut invalid = valid;
        invalid[16..18].copy_from_slice(&kind.to_le_bytes());
        assert!(!valid_elf(&invalid));
    }
    assert!(!valid_elf(&[0; 64]));
}

#[test]
fn native_inspection_records_fingerprint_without_writes_for_both_games() {
    let Ok(platform) = ServerPlatform::current() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    for (name, game) in [
        ("arma", ServerDefinitionKind::Arma3),
        ("reforger", ServerDefinitionKind::Reforger),
    ] {
        let directory = root.path().join(name);
        let executable = create_native_install(&directory, game);
        fs::write(directory.join("operator.cfg"), b"keep this configuration").unwrap();
        let before = inventory(root.path());
        let installation = ServerInstallation::inspect(&directory, game).unwrap();
        assert_eq!(installation.game, game);
        assert_eq!(installation.platform, platform);
        assert_eq!(installation.directory, directory.canonicalize().unwrap());
        assert_eq!(installation.executable, executable_name(game, platform));
        assert_eq!(installation.evidence.bytes, 128);
        assert_eq!(
            installation.evidence.blake3,
            blake3::hash(&fs::read(executable).unwrap())
                .to_hex()
                .to_string()
        );
        assert_eq!(installation.ownership, InstallationOwnership::Adopted);
        assert_eq!(
            installation.build_evidence,
            BuildEvidence::ExecutableFingerprintOnly
        );
        assert!(installation.validate(game));
        installation.verify().unwrap();
        assert_eq!(inventory(root.path()), before);
    }
}

#[test]
fn verification_detects_same_length_and_length_changing_executable_drift() {
    if ServerPlatform::current().is_err() {
        return;
    }
    for game in [ServerDefinitionKind::Arma3, ServerDefinitionKind::Reforger] {
        let root = tempfile::tempdir().unwrap();
        let executable = create_native_install(root.path(), game);
        let installation = ServerInstallation::inspect(root.path(), game).unwrap();
        let original = fs::read(&executable).unwrap();
        let mut changed = original.clone();
        *changed.last_mut().unwrap() = 1;
        fs::write(&executable, &changed).unwrap();
        assert_eq!(installation.verify(), Err(ServerStoreError::StaleRevision));
        changed.push(2);
        fs::write(&executable, &changed).unwrap();
        assert_eq!(installation.verify(), Err(ServerStoreError::StaleRevision));
        fs::write(&executable, &original).unwrap();
        installation.verify().unwrap();
        fs::remove_file(&executable).unwrap();
        assert!(installation.verify().is_err());
    }
}

#[test]
fn inspection_rejects_missing_and_client_executables_without_writes() {
    let Ok(platform) = ServerPlatform::current() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    for (name, game, client_windows, client_linux) in [
        (
            "arma",
            ServerDefinitionKind::Arma3,
            "arma3_x64.exe",
            "arma3_x64",
        ),
        (
            "reforger",
            ServerDefinitionKind::Reforger,
            "ArmaReforgerSteam.exe",
            "ArmaReforgerSteam",
        ),
    ] {
        let directory = root.path().join(name);
        let missing_before = inventory(root.path());
        assert!(ServerInstallation::inspect(&directory, game).is_err());
        assert_eq!(inventory(root.path()), missing_before);
        let dedicated = create_native_install(&directory, game);
        let client_name = match platform {
            ServerPlatform::WindowsX64 => client_windows,
            ServerPlatform::LinuxX64 => client_linux,
        };
        fs::rename(dedicated, directory.join(client_name)).unwrap();
        let before = inventory(root.path());
        assert!(ServerInstallation::inspect(&directory, game).is_err());
        assert_eq!(inventory(root.path()), before);
    }
}

#[test]
fn inspection_rejects_truncated_wrong_platform_and_wrong_architecture_binaries() {
    let Ok(platform) = ServerPlatform::current() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let executable = create_native_install(root.path(), ServerDefinitionKind::Arma3);
    let original = fs::read(&executable).unwrap();
    let mut lengths = vec![0, 1, 63];
    if platform == ServerPlatform::WindowsX64 {
        lengths.extend([64, 89]);
    }
    for length in lengths {
        fs::write(&executable, &original[..length]).unwrap();
        assert!(ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3).is_err());
    }
    let other_platform = match platform {
        ServerPlatform::WindowsX64 => ServerPlatform::LinuxX64,
        ServerPlatform::LinuxX64 => ServerPlatform::WindowsX64,
    };
    fs::write(&executable, executable_bytes(other_platform)).unwrap();
    assert!(ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3).is_err());
    let mut wrong_architecture = original;
    match platform {
        ServerPlatform::WindowsX64 => {
            wrong_architecture[68..70].copy_from_slice(&0x014cu16.to_le_bytes())
        }
        ServerPlatform::LinuxX64 => wrong_architecture[18..20].copy_from_slice(&3u16.to_le_bytes()),
    }
    fs::write(executable, wrong_architecture).unwrap();
    assert!(ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3).is_err());
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
#[test]
fn inspection_rejects_invalid_dos_signature_and_pe_offsets() {
    let root = tempfile::tempdir().unwrap();
    let executable = create_native_install(root.path(), ServerDefinitionKind::Arma3);
    let original = fs::read(&executable).unwrap();
    let mut invalid_signature = original.clone();
    invalid_signature[0] = 0;
    fs::write(&executable, invalid_signature).unwrap();
    assert!(ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3).is_err());
    for offset in [0u32, 63, 103, u32::MAX] {
        let mut invalid_offset = original.clone();
        invalid_offset[60..64].copy_from_slice(&offset.to_le_bytes());
        fs::write(&executable, invalid_offset).unwrap();
        assert!(ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3).is_err());
    }
}

#[test]
fn record_validation_rejects_wrong_game_paths_names_sizes_and_hashes() {
    if ServerPlatform::current().is_err() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    create_native_install(root.path(), ServerDefinitionKind::Arma3);
    let original = ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3).unwrap();
    assert!(!original.validate(ServerDefinitionKind::Reforger));
    let mut parent = original.directory.as_os_str().to_os_string();
    parent.push(std::path::MAIN_SEPARATOR_STR);
    parent.push("..");
    for directory in [PathBuf::from("relative"), PathBuf::from(parent)] {
        let mut invalid = original.clone();
        invalid.directory = directory;
        assert!(!invalid.validate(ServerDefinitionKind::Arma3));
    }
    for executable in ["", "client.exe", "../arma3server_x64.exe"] {
        let mut invalid = original.clone();
        invalid.executable = executable.to_string();
        assert!(!invalid.validate(ServerDefinitionKind::Arma3));
    }
    for bytes in [0, 63, 1024 * 1024 * 1024 + 1] {
        let mut invalid = original.clone();
        invalid.evidence.bytes = bytes;
        assert!(!invalid.validate(ServerDefinitionKind::Arma3));
    }
    for hash in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        let mut invalid = original.clone();
        invalid.evidence.blake3 = hash;
        assert!(!invalid.validate(ServerDefinitionKind::Arma3));
    }
    let mut value = serde_json::to_value(&original).unwrap();
    value["runtime"] = serde_json::Value::Bool(true);
    assert!(serde_json::from_value::<ServerInstallation>(value).is_err());
}

#[cfg(unix)]
#[test]
fn inspection_rejects_files_without_execute_permission() {
    if ServerPlatform::current().is_err() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let executable = create_native_install(root.path(), ServerDefinitionKind::Arma3);
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o600)).unwrap();
    let before = inventory(root.path());
    assert!(ServerInstallation::inspect(root.path(), ServerDefinitionKind::Arma3).is_err());
    assert_eq!(inventory(root.path()), before);
}

#[cfg(any(unix, windows))]
#[test]
fn inspection_rejects_linked_directories_and_executables() {
    if ServerPlatform::current().is_err() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("install");
    let executable = create_native_install(&directory, ServerDefinitionKind::Arma3);
    let alias = root.path().join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&directory, &alias).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_dir(&directory, &alias) {
        if error.kind() == std::io::ErrorKind::PermissionDenied
            || error.raw_os_error() == Some(1314)
        {
            return;
        }
        panic!("Cannot create test directory link: {}", error.kind());
    }
    assert_eq!(
        ServerInstallation::inspect(&alias, ServerDefinitionKind::Arma3),
        Err(ServerStoreError::UnsafePath)
    );
    let target = root.path().join("executable-payload");
    fs::rename(&executable, &target).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &executable).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&target, &executable).unwrap();
    let bytes = fs::read(&target).unwrap();
    assert_eq!(
        ServerInstallation::inspect(&directory, ServerDefinitionKind::Arma3),
        Err(ServerStoreError::UnsafePath)
    );
    assert_eq!(fs::read(target).unwrap(), bytes);
}
