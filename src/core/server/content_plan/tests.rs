use super::*;
use crate::core::server::content_sources::{AddonFolder, ContentSources, RepositoryProfile};
use crate::core::server::{ServerCatalog, ServerContext, ServerDefinitionKind, ServerInstance};
use serde_json::json;
use std::fs;

fn fixture(root: &Path) {
    fs::write(
        root.join("games.json"),
        serde_json::to_vec(&json!({
            "active_game_space_id": "alpha",
            "last_opened_game_space_id": "alpha",
            "game_spaces": [
                {"id":"alpha", "game_id":"arma3", "display_name":"Alpha", "created_at":0},
                {"id":"beta", "game_id":"reforger", "display_name":"Beta", "created_at":0}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    for space in ["alpha", "beta"] {
        fs::create_dir_all(root.join("games").join(space)).unwrap();
    }
}

fn store(root: &Path, space: &str) -> ServerStore {
    ServerStore::new(ServerContext::capture(root, Some(space)).unwrap()).unwrap()
}

fn prepare_store(
    root: &Path,
    space: &str,
    profile: RepositoryProfile,
    folder: AddonFolder,
) -> ServerStore {
    let store = store(root, space);
    let instance = ServerInstance::draft(
        space,
        if space == "alpha" {
            ServerDefinitionKind::Arma3
        } else {
            ServerDefinitionKind::Reforger
        },
        "MainServer",
    )
    .unwrap();
    let catalog = ServerCatalog {
        schema_version: super::super::definition::SERVER_SCHEMA_VERSION,
        game_space_id: space.into(),
        game_id: if space == "alpha" {
            "arma3"
        } else {
            "reforger"
        }
        .into(),
        revision: 1,
        instances: vec![instance],
        installations: vec![],
    };
    let catalog_path = store.path();
    let server_dir = catalog_path.parent().unwrap();
    fs::create_dir_all(server_dir).unwrap();
    fs::write(store.path(), serde_json::to_vec(&catalog).unwrap()).unwrap();
    let source_dir = root.join(format!("source-{space}"));
    fs::create_dir_all(&source_dir).unwrap();
    let sources = ContentSources {
        schema_version: 1,
        folders: vec![AddonFolder {
            id: folder.id,
            path: source_dir,
        }],
        profiles: vec![profile],
    };
    sources.save(&store.context().space_dir()).unwrap();
    store
}

fn request(store: &ServerStore, profile: &str) -> ContentPlanRequest {
    let catalog = store.load().unwrap();
    let snapshot = store.content_sources().unwrap();
    ContentPlanRequest {
        instance_id: catalog.instances[0].id.clone(),
        expected_instance_revision: catalog.instances[0].revision,
        expected_catalog_revision: catalog.revision,
        content_sources_fingerprint: snapshot.fingerprint,
        bindings: vec![ServerAddonBinding {
            source_id: profile.into(),
            role: ServerAddonRole::Required,
            order: 4,
        }],
    }
}

fn profile(name: &str, supply: AddonSupply, remote_url: &str, local: &Path) -> RepositoryProfile {
    RepositoryProfile {
        name: name.into(),
        remote_url: remote_url.into(),
        local_path: local.to_path_buf(),
        folder_id: "content".into(),
        supply,
        addons: vec!["@mod_01".into()],
    }
}

#[test]
fn expanded_source_path_bytes_are_bounded_before_metadata_observation() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = prepare_store(
        root.path(),
        "alpha",
        profile(
            "local",
            AddonSupply::AlreadyPresent,
            "",
            &root.path().join("client"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("source-alpha"),
        },
    );
    let mut sources = store.content_sources().unwrap().sources;
    for _ in 0..120 {
        sources.folders[0].path.push("x".repeat(200));
    }
    sources.profiles[0].addons = (0..180).map(|index| format!("@mod_{index:03}")).collect();
    sources.save(&store.context().space_dir()).unwrap();
    assert_eq!(
        store.plan_content(request(&store, "local")).unwrap_err(),
        ServerStoreError::Invalid("Content plan exceeds its path size limit".into())
    );
    assert!(
        !store
            .context()
            .space_dir()
            .join("server/instances")
            .exists()
    );
}

fn replace_source_folder_path(store: &ServerStore, path: &Path, clear_addons: bool) {
    let mut sources = store.content_sources().unwrap().sources;
    sources.folders[0].path = path.to_path_buf();
    if clear_addons {
        sources.profiles[0].addons.clear();
    }
    sources.save(&store.context().space_dir()).unwrap();
}

#[test]
fn local_already_present_content_is_observed_without_writes_in_both_spaces() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    for (space, remote) in [
        ("alpha", "https://repo.example.invalid/main/"),
        ("beta", "https://repo.example.invalid/reforger/"),
    ] {
        let profile = profile(
            "local",
            AddonSupply::AlreadyPresent,
            remote,
            &root.path().join(format!("client-{space}")),
        );
        let store = prepare_store(
            root.path(),
            space,
            profile,
            AddonFolder {
                id: "content".into(),
                path: root.path().join(format!("source-{space}")),
            },
        );
        let source = root.path().join(format!("source-{space}/@mod_01"));
        fs::create_dir_all(&source).unwrap();
        let catalog_before = fs::read(store.path()).unwrap();
        let registry_before = fs::read(root.path().join("games.json")).unwrap();
        let result = store.plan_content(request(&store, "local")).unwrap();
        assert_eq!(result.game_space_id, space);
        assert_eq!(result.bindings[0].addon_paths[0].source, source);
        assert!(matches!(
            result.bindings[0].addon_paths[0].state,
            ContentSourceState::PresentUnverified
        ));
        assert!(!result.deployment_ready);
        assert!(!result.downloads_started);
        assert!(!result.writes_performed);
        assert!(!super::super::addons::client_enablement_changes());
        assert!(
            result
                .blockers
                .iter()
                .any(|blocker| blocker.code == "runtime_deployment_unavailable")
        );
        assert_eq!(fs::read(store.path()).unwrap(), catalog_before);
        assert_eq!(
            fs::read(root.path().join("games.json")).unwrap(),
            registry_before
        );
    }
}

#[test]
fn download_is_never_resolved_from_existing_bytes_and_local_only_url_is_allowed() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let download_store = prepare_store(
        root.path(),
        "alpha",
        profile(
            "download",
            AddonSupply::Download,
            "https://repo.example.invalid/main/",
            &root.path().join("client"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("source-alpha"),
        },
    );
    fs::create_dir_all(root.path().join("source-alpha/@mod_01")).unwrap();
    let result = download_store
        .plan_content(request(&download_store, "download"))
        .unwrap();
    assert!(matches!(
        result.bindings[0].addon_paths[0].state,
        ContentSourceState::AcquisitionRequired
    ));
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "content_acquisition_required")
    );

    let local_store = prepare_store(
        root.path(),
        "beta",
        profile(
            "local",
            AddonSupply::AlreadyPresent,
            "",
            &root.path().join("client"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("source-beta"),
        },
    );
    fs::create_dir_all(root.path().join("source-beta/@mod_01")).unwrap();
    assert!(
        local_store
            .plan_content(request(&local_store, "local"))
            .is_ok()
    );
}

#[test]
fn missing_sources_and_stale_inputs_are_reported_or_rejected() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = prepare_store(
        root.path(),
        "alpha",
        profile(
            "local",
            AddonSupply::AlreadyPresent,
            "",
            &root.path().join("client"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("source-alpha"),
        },
    );
    let request = request(&store, "local");
    let result = store.plan_content(request.clone()).unwrap();
    assert!(matches!(
        result.bindings[0].addon_paths[0].state,
        ContentSourceState::Missing
    ));
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "content_source_missing")
    );
    let mut stale = request.clone();
    stale.expected_catalog_revision += 1;
    assert!(matches!(
        store.plan_content(stale),
        Err(ServerStoreError::StaleRevision)
    ));
    let mut stale = request.clone();
    stale.expected_instance_revision += 1;
    assert!(matches!(
        store.plan_content(stale),
        Err(ServerStoreError::StaleRevision)
    ));
    let mut stale = request;
    stale.content_sources_fingerprint = Some("stale".into());
    assert!(matches!(
        store.plan_content(stale),
        Err(ServerStoreError::StaleRevision)
    ));
}

#[test]
fn source_root_overlapping_content_root_is_blocked() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = prepare_store(
        root.path(),
        "alpha",
        profile(
            "local",
            AddonSupply::AlreadyPresent,
            "",
            &root.path().join("client"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("unused"),
        },
    );
    let source_root = store.context().space_dir().join("server/instances");
    fs::create_dir_all(&source_root).unwrap();
    replace_source_folder_path(&store, &source_root, true);
    let result = store.plan_content(request(&store, "local")).unwrap();
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "content_source_destination_overlap")
    );

    let instance_id = store.load().unwrap().instances[0].id.clone();
    let content_root = store
        .context()
        .space_dir()
        .join("server/instances")
        .join(instance_id.as_str())
        .join("content");
    let nested_source = content_root.join("nested");
    fs::create_dir_all(nested_source.join("nested")).unwrap();
    let mut sources = store.content_sources().unwrap().sources;
    sources.folders[0].path = nested_source;
    sources.profiles[0].addons = vec!["nested".into()];
    sources.save(&store.context().space_dir()).unwrap();
    let result = store.plan_content(request(&store, "local")).unwrap();
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "content_source_destination_overlap")
    );
}

#[test]
fn adopted_installation_may_not_overlap_a_content_source() {
    use crate::core::server::installation::{
        BuildEvidence, ExecutableEvidence, InstallationOwnership,
    };
    use crate::core::server::{InstallationId, ServerInstallation, ServerPlatform};

    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = prepare_store(
        root.path(),
        "alpha",
        profile(
            "local",
            AddonSupply::AlreadyPresent,
            "",
            &root.path().join("client"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("source-alpha"),
        },
    );
    let source_dir = root.path().join("source-alpha").canonicalize().unwrap();
    fs::create_dir_all(source_dir.join("@mod_01")).unwrap();
    let platform = ServerPlatform::current().unwrap();
    let executable = match platform {
        ServerPlatform::WindowsX64 => "arma3server_x64.exe",
        ServerPlatform::LinuxX64 => "arma3server_x64",
    };
    let installation = ServerInstallation {
        id: InstallationId::new(),
        game: ServerDefinitionKind::Arma3,
        platform,
        directory: source_dir,
        executable: executable.into(),
        evidence: ExecutableEvidence {
            bytes: 64,
            blake3: "0".repeat(64),
        },
        ownership: InstallationOwnership::Adopted,
        build_evidence: BuildEvidence::ExecutableFingerprintOnly,
    };
    let path = store.path();
    let mut catalog: ServerCatalog = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    catalog.instances[0].installation_id = Some(installation.id.clone());
    catalog.installations.push(installation);
    fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let result = store.plan_content(request(&store, "local")).unwrap();
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "content_source_installation_overlap")
    );
}

#[cfg(unix)]
#[test]
fn linked_sources_are_unsafe_and_distinct_repository_paths_keep_identity() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = prepare_store(
        root.path(),
        "alpha",
        profile(
            "first",
            AddonSupply::AlreadyPresent,
            "https://repo.example.invalid/shared/",
            &root.path().join("client-first"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("source-alpha"),
        },
    );
    let outside = root.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.path().join("source-alpha/@mod_01")).unwrap();
    let mut sources = store.content_sources().unwrap().sources;
    sources.profiles.push(profile(
        "second",
        AddonSupply::AlreadyPresent,
        "https://repo.example.invalid/shared/",
        &root.path().join("client-second"),
    ));
    sources.save(&store.context().space_dir()).unwrap();
    let mut request = request(&store, "first");
    request.bindings.push(ServerAddonBinding {
        source_id: "second".into(),
        role: ServerAddonRole::Optional,
        order: 5,
    });
    request.content_sources_fingerprint = store.content_sources().unwrap().fingerprint;
    let result = store.plan_content(request).unwrap();
    assert!(matches!(
        result.bindings[0].addon_paths[0].state,
        ContentSourceState::Unsafe
    ));
    assert_eq!(
        result.bindings[0].repository_local_path,
        root.path().join("client-first")
    );
    assert_eq!(
        result.bindings[1].repository_local_path,
        root.path().join("client-second")
    );
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "destination_collision")
    );
    assert!(result.blockers.iter().any(|blocker| {
        blocker.code == "destination_collision"
            && blocker.message == "Two selected content sources propose overlapping destinations."
    }));
}

#[cfg(unix)]
#[test]
fn linked_download_source_is_unsafe_and_remains_unacquired() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = prepare_store(
        root.path(),
        "alpha",
        profile(
            "download",
            AddonSupply::Download,
            "https://repo.example.invalid/main/",
            &root.path().join("client"),
        ),
        AddonFolder {
            id: "content".into(),
            path: root.path().join("source-alpha"),
        },
    );
    let outside = root.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    symlink(outside, root.path().join("source-alpha/@mod_01")).unwrap();
    let result = store.plan_content(request(&store, "download")).unwrap();
    assert!(matches!(
        result.bindings[0].addon_paths[0].state,
        ContentSourceState::Unsafe
    ));
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "content_source_unsafe")
    );
    assert!(
        result
            .blockers
            .iter()
            .any(|blocker| blocker.code == "content_acquisition_required")
    );
}
