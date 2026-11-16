use std::fs;

use super::*;

fn source(root: &std::path::Path) -> StageSource {
    StageSource {
        profile_name: "Fixture".into(),
        source: root.join("source"),
        destination: "addons/source".into(),
    }
}

#[test]
fn safe_component_rejects_portable_hazards() {
    for bad in [
        "",
        ".",
        "..",
        "x.",
        "x ",
        "CON",
        "nul.txt",
        "a/b",
        "a\\b",
        "a:b",
        "bad\nname",
    ] {
        assert!(!safe_component(bad), "{bad:?}");
    }
    assert!(safe_component("normal-addon_01"));
    assert!(!safe_component(&"x".repeat(256)));
}

#[test]
fn inspects_sorted_tree_and_keeps_empty_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    fs::create_dir_all(root.join("z-empty")).unwrap();
    fs::create_dir_all(root.join("a")).unwrap();
    fs::write(root.join("a/file.bin"), b"content").unwrap();
    let entries = inspect_sources(&[source(temp.path())], &ContentStageControl::default()).unwrap();
    assert!(entries.windows(2).all(|w| w[0].path <= w[1].path));
    assert!(
        entries
            .iter()
            .any(|e| e.path == "addons/source/z-empty" && e.kind == StageEntryKind::Directory)
    );
    assert_eq!(
        entries
            .iter()
            .find(|e| e.path == "addons/source/a/file.bin")
            .unwrap()
            .blake3
            .as_deref(),
        Some(blake3::hash(b"content").to_hex().as_str())
    );
}

#[test]
fn copies_source_aliases_and_verifies_exact_stage_then_detects_corruption() {
    for destination in ["source", "aaaaaaaaaaaaaaaa"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("file"), b"abc").unwrap();
        let mut selected = source(temp.path());
        selected.destination = format!("addons/{destination}");
        let sources = vec![selected];
        let entries = inspect_sources(&sources, &ContentStageControl::default()).unwrap();
        let total_bytes = entries
            .iter()
            .filter(|e| e.kind == StageEntryKind::File)
            .map(|e| e.bytes)
            .sum();
        let manifest = ContentStageManifest {
            schema_version: 1,
            bindings_fingerprint: String::new(),
            bindings: super::super::ContentBindingsDocument {
                schema_version: 1,
                game_space_id: "space".into(),
                game_id: "arma3".into(),
                instance_id: super::super::super::ServerInstanceId::new(),
                generation: 1,
                instance_revision: 1,
                catalog_revision: 1,
                catalog_fingerprint: String::new(),
                content_sources_fingerprint: None,
                bindings: Vec::new(),
            },
            sources,
            missions: vec![],
            reforger_addons: None,
            acquisitions: vec![],
            entries,
            total_bytes,
        };
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let ctl = ContentStageControl::default();
        copy_sources(&manifest, &stage, &ctl).unwrap();
        verify_stage(&manifest, &stage, &ctl).unwrap();
        fs::write(stage.join("manifest.json"), b"published metadata").unwrap();
        verify_stage(&manifest, &stage, &ctl).unwrap();
        assert_eq!(ctl.snapshot().phase, ContentStagePhase::Checking);
        assert_eq!(ctl.snapshot().bytes_done, 3);
        fs::write(stage.join("unexpected"), b"extra").unwrap();
        assert_eq!(verify_stage(&manifest, &stage, &ctl), Err(Error::Corrupt));
        fs::remove_file(stage.join("unexpected")).unwrap();
        fs::write(stage.join(format!("addons/{destination}/file")), b"xyz").unwrap();
        assert_eq!(verify_stage(&manifest, &stage, &ctl), Err(Error::Corrupt));
    }
}

#[test]
fn copy_repeated_prefixes_as_literal_path_components() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    fs::create_dir_all(root.join("source")).unwrap();
    fs::write(root.join("source/file"), b"nested").unwrap();
    let sources = vec![source(temp.path())];
    let entries = inspect_sources(&sources, &ContentStageControl::default()).unwrap();
    let total_bytes = entries
        .iter()
        .filter(|e| e.kind == StageEntryKind::File)
        .map(|e| e.bytes)
        .sum();
    let manifest = ContentStageManifest {
        schema_version: 1,
        bindings_fingerprint: String::new(),
        bindings: super::super::ContentBindingsDocument {
            schema_version: 1,
            game_space_id: "space".into(),
            game_id: "arma3".into(),
            instance_id: super::super::super::ServerInstanceId::new(),
            generation: 1,
            instance_revision: 1,
            catalog_revision: 1,
            catalog_fingerprint: String::new(),
            content_sources_fingerprint: None,
            bindings: Vec::new(),
        },
        sources,
        missions: vec![],
        reforger_addons: None,
        acquisitions: vec![],
        entries,
        total_bytes,
    };
    let stage = temp.path().join("stage");
    fs::create_dir(&stage).unwrap();
    copy_sources(&manifest, &stage, &ContentStageControl::default()).unwrap();
    assert_eq!(
        fs::read(stage.join("addons/source/source/file")).unwrap(),
        b"nested"
    );
}

#[test]
fn rejects_linked_source_entries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(temp.path().join("missing"), root.join("link")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(temp.path().join("missing"), root.join("link")).unwrap();
    assert_eq!(
        inspect_sources(&[source(temp.path())], &ContentStageControl::default()),
        Err(Error::UnsafePath)
    );
}

#[test]
fn observes_cancellation_before_tree_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    fs::create_dir(&root).unwrap();
    let ctl = ContentStageControl::default();
    ctl.cancel();
    assert_eq!(
        inspect_sources(&[source(temp.path())], &ctl),
        Err(Error::Cancelled)
    );
}

#[test]
fn changed_source_size_is_rejected_before_creating_a_copy() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("copy.bin");
    fs::write(&source, b"changed size").unwrap();
    let expected = StageEntry {
        path: "addons/source/file.bin".into(),
        kind: StageEntryKind::File,
        bytes: 3,
        blake3: Some(blake3::hash(b"old").to_hex().to_string()),
    };
    assert_eq!(
        copy_file(
            &source,
            &destination,
            &expected,
            &ContentStageControl::default()
        ),
        Err(Error::StaleRevision)
    );
    assert!(!destination.exists());
}

#[test]
fn archive_manifest_does_not_consume_the_content_entry_budget() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("file.bin"), b"payload").unwrap();
    fs::write(temp.path().join("manifest.json"), b"{}").unwrap();
    let mut entries = Vec::new();
    let mut scan = ScanState {
        hash_files: true,
        exclude_manifest: true,
        path_bytes: 0,
        entry_count: MAX_ENTRIES - 1,
    };
    inventory(
        temp.path(),
        "",
        &mut entries,
        &mut BTreeMap::new(),
        &ContentStageControl::default(),
        &mut scan,
    )
    .unwrap();
    assert_eq!(scan.entry_count, MAX_ENTRIES);
    assert_eq!(scan.path_bytes, "file.bin".len());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "file.bin");
}

fn file_entry(path: &str, bytes: &[u8]) -> StageEntry {
    StageEntry {
        path: path.into(),
        kind: StageEntryKind::File,
        bytes: bytes.len() as u64,
        blake3: Some(blake3::hash(bytes).to_hex().to_string()),
    }
}

#[test]
fn verifies_generic_configuration_and_content_roots_including_nested_manifests() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("configuration/nested")).unwrap();
    fs::create_dir_all(root.join("content")).unwrap();
    fs::write(root.join("manifest.json"), b"root manifest").unwrap();
    fs::write(
        root.join("configuration/nested/manifest.json"),
        b"config manifest",
    )
    .unwrap();
    fs::write(root.join("content/manifest.json"), b"content manifest").unwrap();
    let mut entries = vec![
        StageEntry {
            path: "configuration".into(),
            kind: StageEntryKind::Directory,
            bytes: 0,
            blake3: None,
        },
        StageEntry {
            path: "configuration/nested".into(),
            kind: StageEntryKind::Directory,
            bytes: 0,
            blake3: None,
        },
        file_entry("configuration/nested/manifest.json", b"config manifest"),
        StageEntry {
            path: "content".into(),
            kind: StageEntryKind::Directory,
            bytes: 0,
            blake3: None,
        },
        file_entry("content/manifest.json", b"content manifest"),
    ];
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let bytes = entries
        .iter()
        .filter(|entry| entry.kind == StageEntryKind::File)
        .map(|entry| entry.bytes)
        .sum();
    assert_eq!(
        verify_tree(&entries, bytes, root, &ContentStageControl::default()),
        Ok(())
    );
    fs::write(root.join("content/extra"), b"extra").unwrap();
    assert_eq!(
        verify_tree(&entries, bytes, root, &ContentStageControl::default()),
        Err(Error::Corrupt)
    );
}

#[test]
fn generic_tree_rejects_case_collisions_bad_hierarchy_and_inconsistent_totals() {
    let dir = |path: &str| StageEntry {
        path: path.into(),
        kind: StageEntryKind::Directory,
        bytes: 0,
        blake3: None,
    };
    let file = file_entry("Content/file", b"x");
    assert!(validate_generic_entries(&[dir("Content"), file.clone(), dir("content")], 1).is_err());
    assert!(validate_generic_entries(std::slice::from_ref(&file), 1).is_err());
    assert!(validate_generic_entries(&[dir("Content"), file.clone()], 2).is_err());
    assert!(validate_generic_entries(&[dir("Content"), file], 1).is_ok());
}

#[test]
fn verified_copy_refuses_overwrite_and_honors_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::write(&src, b"payload").unwrap();
    let entry = file_entry("content/file", b"payload");
    fs::write(&dst, b"existing").unwrap();
    assert_eq!(
        copy_verified_file(&src, &dst, &entry, &ContentStageControl::default()),
        Err(Error::Io)
    );
    assert_eq!(fs::read(&dst).unwrap(), b"existing");
    fs::remove_file(&dst).unwrap();
    let control = ContentStageControl::default();
    control.cancel();
    assert_eq!(
        copy_verified_file(&src, &dst, &entry, &control),
        Err(Error::Cancelled)
    );
}
