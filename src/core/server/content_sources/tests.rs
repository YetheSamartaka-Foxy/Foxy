use super::*;

fn profile(supply: AddonSupply, local: &Path, folder: &str) -> RepositoryProfile {
    RepositoryProfile {
        name: "RepoAlpha".into(),
        remote_url: "https://repo.example.invalid/main".into(),
        local_path: local.to_path_buf(),
        folder_id: folder.into(),
        supply,
        addons: vec!["@mod_01".into()],
    }
}

fn sources(root: &Path) -> ContentSources {
    let mut sources = ContentSources::default();
    sources.add_folder("hosted", &root.join("mods")).unwrap();
    sources
        .add_profile(profile(
            AddonSupply::AlreadyPresent,
            &root.join("mods"),
            "hosted",
        ))
        .unwrap();
    sources
}

#[test]
fn an_existing_server_copy_is_not_downloaded_again() {
    let root = tempfile::tempdir().unwrap();
    let mut sources = sources(root.path());
    let resolved = sources.resolve("RepoAlpha").unwrap();
    assert!(!resolved.download);
    assert_eq!(resolved.addon_paths, vec![root.path().join("mods/@mod_01")]);
    assert_eq!(
        sources.add_profile(profile(
            AddonSupply::Download,
            &root.path().join("mods"),
            "hosted"
        )),
        Err(SourceError::DownloadOwnCopy)
    );
    assert!(!client_enablement_changes());
}

#[test]
fn a_distinct_repository_path_can_be_marked_for_download() {
    let root = tempfile::tempdir().unwrap();
    let mut sources = ContentSources::default();
    sources
        .add_folder("incoming", &root.path().join("incoming"))
        .unwrap();
    sources
        .add_profile(profile(
            AddonSupply::Download,
            &root.path().join("client"),
            "incoming",
        ))
        .unwrap();
    assert!(sources.resolve("RepoAlpha").unwrap().download);
    assert!(sources.profiles[0].remote_url.ends_with('/'));
    sources.save(root.path()).unwrap();
    assert_eq!(ContentSources::load(root.path()).unwrap(), sources);
}

#[test]
fn local_only_content_is_allowed_only_without_download_intent() {
    let root = tempfile::tempdir().unwrap();
    let mut sources = ContentSources::default();
    sources
        .add_folder("hosted", &root.path().join("mods"))
        .unwrap();
    let mut local = profile(
        AddonSupply::AlreadyPresent,
        &root.path().join("client"),
        "hosted",
    );
    local.remote_url.clear();
    sources.add_profile(local.clone()).unwrap();
    assert!(sources.profiles[0].remote_url.is_empty());
    assert!(!sources.resolve("RepoAlpha").unwrap().download);
    local.supply = AddonSupply::Download;
    assert_eq!(sources.add_profile(local), Err(SourceError::InvalidUrl));
}

#[test]
fn the_same_url_at_different_local_paths_remains_two_instances() {
    let root = tempfile::tempdir().unwrap();
    let mut sources = sources(root.path());
    let mut second = profile(
        AddonSupply::AlreadyPresent,
        &root.path().join("other"),
        "hosted",
    );
    second.name = "RepoBeta".into();
    sources.add_profile(second.clone()).unwrap();
    assert_eq!(sources.profiles.len(), 2);
    second.name = "RepoGamma".into();
    assert_eq!(
        sources.add_profile(second),
        Err(SourceError::DuplicateProfile)
    );
}

#[test]
fn hand_edited_registries_are_validated_without_repairing_bytes() {
    let root = tempfile::tempdir().unwrap();
    let valid = sources(root.path());
    let path = root.path().join(CONTENT_SOURCES_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let value = serde_json::to_value(valid).unwrap();
    for mutation in [
        "duplicate_name",
        "duplicate_folder",
        "relative_path",
        "escape",
        "duplicate_addon",
        "url",
        "unknown_folder",
    ] {
        let mut document = value.clone();
        match mutation {
            "duplicate_name" => {
                let mut profile = document["profiles"][0].clone();
                profile["local_path"] = root
                    .path()
                    .join("other")
                    .to_string_lossy()
                    .to_string()
                    .into();
                profile["name"] = "repoalpha".into();
                document["profiles"].as_array_mut().unwrap().push(profile);
            }
            "duplicate_folder" => {
                let folder = document["folders"][0].clone();
                document["folders"].as_array_mut().unwrap().push(folder);
            }
            "relative_path" => document["folders"][0]["path"] = "relative".into(),
            "escape" => document["profiles"][0]["addons"][0] = "../@mod_01".into(),
            "duplicate_addon" => {
                document["profiles"][0]["addons"] = serde_json::json!(["@mod_01", "@MOD_01"])
            }
            "url" => document["profiles"][0]["remote_url"] = "file:///mods".into(),
            "unknown_folder" => document["profiles"][0]["folder_id"] = "unknown".into(),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&document).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(ContentSources::load(root.path()).is_err(), "{mutation}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn urls_and_addon_components_are_safe_and_unambiguous() {
    assert_eq!(
        normalize_repository_url(" https://repo.example.invalid/Main ").unwrap(),
        "https://repo.example.invalid/Main/"
    );
    for url in [
        "ftp://repo.example.invalid/",
        "https://user:secret@repo.example.invalid/",
        "https://repo.example.invalid/?token=value",
        "https://repo.example.invalid/#part",
        "https://repo.example.invalid/a b",
        "https://repo.example.invalid/a\tb",
    ] {
        assert_eq!(normalize_repository_url(url), Err(SourceError::InvalidUrl));
    }
    for name in [
        "",
        ".",
        "..",
        "../mod",
        "C:mod",
        "mod\\file",
        "mod\n",
        "mod.",
        "mod ",
    ] {
        assert!(!is_addon_name(name));
    }
    assert!(is_addon_name("@mod_01"));
}

#[test]
fn absent_and_changed_documents_have_distinct_raw_fingerprints_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let snapshot = ContentSources::read_snapshot(root.path()).unwrap();
    assert!(snapshot.fingerprint.is_none());
    assert!(!root.path().join("server").exists());
    sources(root.path()).save(root.path()).unwrap();
    let path = root.path().join(CONTENT_SOURCES_FILE);
    let before = ContentSources::read_snapshot(root.path()).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(b'\n');
    std::fs::write(&path, &bytes).unwrap();
    let after = ContentSources::read_snapshot(root.path()).unwrap();
    assert_eq!(before.sources, after.sources);
    assert_ne!(before.fingerprint, after.fingerprint);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn oversized_or_nonregular_registry_is_rejected_without_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(CONTENT_SOURCES_FILE);
    std::fs::create_dir_all(&path).unwrap();
    assert!(ContentSources::load(root.path()).is_err());
    std::fs::remove_dir(&path).unwrap();
    let bytes = vec![b' '; 4 * 1024 * 1024 + 1];
    std::fs::write(&path, &bytes).unwrap();
    assert!(ContentSources::load(root.path()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn claimed_source_writes_reject_changed_review_inputs() {
    const CHILD: &str = "FOXY_CONTENT_SOURCE_WRITE_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::server::content_sources::tests::claimed_source_writes_reject_changed_review_inputs",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("games.json"),
        include_str!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    let context = crate::core::server::ServerContext::capture(root.path(), Some("arma3")).unwrap();
    let store = ServerStore::new(context).unwrap();
    let mut sources = sources(root.path());
    let path = store.context().space_dir().join(CONTENT_SOURCES_FILE);
    assert_eq!(
        store.save_content_sources(&sources, None),
        Err(ServerStoreError::Ownership)
    );
    assert!(!path.exists());
    let claim = store.validate_claim_path().unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    store.save_content_sources(&sources, None).unwrap();
    let reviewed = store.content_sources().unwrap();
    let mut changed = std::fs::read(&path).unwrap();
    changed.push(b'\n');
    std::fs::write(&path, &changed).unwrap();
    sources
        .add_folder("second", &root.path().join("second"))
        .unwrap();
    assert_eq!(
        store.save_content_sources(&sources, reviewed.fingerprint.as_deref()),
        Err(ServerStoreError::StaleRevision)
    );
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    let current = store.content_sources().unwrap();
    store
        .save_content_sources(&sources, current.fingerprint.as_deref())
        .unwrap();
    assert_eq!(store.content_sources().unwrap().sources, sources);
    assert!(!claim.exists());
    crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
}
