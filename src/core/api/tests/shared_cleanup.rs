use super::*;

#[tokio::test]
async fn nested_addon_file_in_same_repository_is_not_classified_as_cleanup() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    let repo_url = "https://example.invalid/RepoAlpha/";
    let repo_root = temp.path().join("MainRepo");
    let outer_addon = repo_root.join("@mod_01");
    let nested_addon = outer_addon.join("@mod_02");
    std::fs::create_dir_all(&nested_addon).expect("create nested addons");
    let outer_file = outer_addon.join("main.pbo");
    std::fs::write(&outer_file, b"outer addon bytes").expect("write outer file");
    let nested_file = nested_addon.join("data.pbo");
    std::fs::write(&nested_file, b"nested addon bytes").expect("write nested file");

    seed_repository(
        &fdb,
        1,
        "RepoAlpha",
        repo_url,
        &repo_root.to_string_lossy(),
        "REPO_HASH",
        "REPO_HASH",
        "",
    )
    .await;
    for (id, name, remote_path, local_path) in [
        (
            11,
            "@mod_01",
            "https://example.invalid/RepoAlpha/@mod_01/",
            outer_addon.to_string_lossy().into_owned(),
        ),
        (
            12,
            "@mod_02",
            "https://example.invalid/RepoAlpha/@mod_01/@mod_02/",
            nested_addon.to_string_lossy().into_owned(),
        ),
    ] {
        seed_addon(
            &fdb,
            id,
            name,
            remote_path,
            &local_path,
            "ADDON_HASH",
            "ADDON_HASH",
            "",
            true,
        )
        .await;
        seed_repository_addon(&fdb, 1, id).await;
    }
    let outer_file_path = outer_file.to_string_lossy().into_owned();
    seed_file(
        &fdb,
        20,
        "main.pbo",
        "https://example.invalid/RepoAlpha/@mod_01/main.pbo",
        &outer_file_path,
        "FILE_HASH",
        "FILE_HASH",
        "",
        std::fs::metadata(&outer_file)
            .expect("outer file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 11, 20).await;
    let nested_file_path = nested_file.to_string_lossy().into_owned();
    seed_file(
        &fdb,
        21,
        "data.pbo",
        "https://example.invalid/RepoAlpha/@mod_01/@mod_02/data.pbo",
        &nested_file_path,
        "FILE_HASH",
        "FILE_HASH",
        "",
        std::fs::metadata(&nested_file)
            .expect("file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 12, 21).await;

    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(repo_root.to_string_lossy()),
    );
    let outer_only = HashSet::from(["@mod_01".to_string()]);
    let unexpected = super::super::quick_scan::collect_unexpected_files_for_repo_mods(
        context.clone(),
        repo_url,
        &outer_only,
        None,
    )
    .await;

    assert!(
        unexpected.is_empty(),
        "a nested addon file linked through this repository must remain managed"
    );
    assert!(refresh_content_hashes_for_repository(context.clone(), repo_url, None).await);
    fdb.execute(
        "UPDATE addons SET local_content_hash = 'OLD_CONTENT_BASELINE' WHERE id = 11",
        params![],
    )
    .await
    .expect("retain a stale parent-folder baseline from before ownership recovery");
    for scan_index in 0..2 {
        let diff = quick_local_change_diff(
            context.clone(),
            repo_url,
            None,
            None,
            None,
            false,
            true,
            false,
            None,
        )
        .await;
        assert!(
            diff.iter().all(|addon| !addon.needs_update),
            "an unchanged nested managed file must converge to a clean quick scan"
        );
        assert!(
            context.hash_evidence.issues_snapshot().is_empty(),
            "the quick scan must complete instead of returning an empty incomplete result"
        );
        if scan_index == 0 {
            let (_, _, repaired_content_hash) = checksums(&fdb, "addons", 11).await;
            assert!(
                !repaired_content_hash.is_empty()
                    && repaired_content_hash != "OLD_CONTENT_BASELINE",
                "the first complete scan must replace the stale folder baseline"
            );
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_path_alias_cleanup_recovery_clears_saved_pending_update() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    let repo_url = "https://example.invalid/RepoAlpha/";
    let repo_root = temp.path().join("MainRepo");
    let addon_dir = repo_root.join("@mod_01");
    std::fs::create_dir_all(&addon_dir).expect("create addon directory");
    let main_file = addon_dir.join("main.bin");
    std::fs::write(&main_file, b"current repository file").expect("write managed file");
    let sibling_file = addon_dir.join("extra.bin");
    std::fs::write(&sibling_file, b"sibling repository file").expect("write sibling file");
    let addon_alias = addon_dir
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let sibling_file_alias = sibling_file
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();

    seed_repository(
        &fdb,
        1,
        "RepoAlpha",
        repo_url,
        &repo_root.to_string_lossy(),
        "REPO_HASH",
        "REPO_HASH",
        "",
    )
    .await;
    seed_addon(
        &fdb,
        11,
        "@mod_01",
        "https://example.invalid/RepoAlpha/@mod_01/",
        &addon_dir.to_string_lossy(),
        "ADDON_HASH",
        "ADDON_HASH",
        "",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 11).await;

    let sibling_repo_url = "https://example.invalid/RepoBeta/";
    let sibling_repo_root = temp.path().join("SiblingRepo");
    seed_repository(
        &fdb,
        3,
        "RepoBeta",
        sibling_repo_url,
        &sibling_repo_root.to_string_lossy(),
        "REPO_HASH",
        "REPO_HASH",
        "",
    )
    .await;
    seed_addon(
        &fdb,
        13,
        "@mod_01",
        "https://example.invalid/RepoBeta/@mod_01/",
        &addon_alias,
        "ADDON_HASH",
        "ADDON_HASH",
        "",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 3, 13).await;
    seed_file(
        &fdb,
        23,
        "extra.bin",
        "https://example.invalid/RepoBeta/@mod_01/extra.bin",
        &sibling_file_alias,
        "FILE_HASH",
        "FILE_HASH",
        "",
        std::fs::metadata(&sibling_file)
            .expect("sibling file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 13, 23).await;

    let other_root = temp.path().join("OtherInstall");
    seed_repository(
        &fdb,
        2,
        "RepoAlpha independent",
        repo_url,
        &other_root.to_string_lossy(),
        "REPO_HASH",
        "REPO_HASH",
        "",
    )
    .await;
    let other_pending_local_path =
        crate::core::utils::content_hash::normalize_path(&other_root.to_string_lossy());
    let stale_pending = serde_json::to_string(&[ModDiffSummary {
        name: "@mod_01".to_string(),
        needs_update: true,
        total_bytes: 0,
        files: Vec::new(),
    }])
    .unwrap();
    fdb.execute(
        "INSERT INTO pending_updates (repository_url, local_path, diff_json, updated_at) VALUES (?, ?, ?, ?)",
        params![repo_url, other_pending_local_path.clone(), stale_pending.clone(), 1i64],
    )
    .await
    .expect("seed same-URL independent-install pending row");
    seed_file(
        &fdb,
        21,
        "main.bin",
        "https://example.invalid/RepoAlpha/@mod_01/main.bin",
        &main_file.to_string_lossy(),
        "FILE_HASH",
        "FILE_HASH",
        "",
        std::fs::metadata(&main_file)
            .expect("main file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 11, 21).await;

    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(repo_root.to_string_lossy()),
    );
    assert!(refresh_content_hashes_for_repository(context.clone(), repo_url, None).await);
    fdb.execute(
        "UPDATE addons SET local_content_hash = 'STALE_BASELINE' WHERE id = 11",
        params![],
    )
    .await
    .expect("make parent folder baseline stale");
    let pending_local_path =
        crate::core::utils::content_hash::normalize_path(&repo_root.to_string_lossy());
    fdb.execute(
        "INSERT INTO pending_updates (repository_url, local_path, diff_json, updated_at) VALUES (?, ?, ?, ?)",
        params![repo_url, pending_local_path.clone(), stale_pending, 1i64],
    )
    .await
    .expect("seed stale pending update");

    for scan_index in 0..2 {
        let diff = quick_local_change_diff(
            context.clone(),
            repo_url,
            None,
            None,
            None,
            false,
            true,
            false,
            None,
        )
        .await;
        assert!(
            diff.iter().all(|addon| !addon.needs_update),
            "a sibling-owned file in this folder must not become cleanup through an aliased addon root"
        );
        assert!(
            context.hash_evidence.issues_snapshot().is_empty(),
            "both alias-recovery scans must finish with complete evidence"
        );
        if scan_index == 0 {
            let (_, _, healed_hash) = checksums(&fdb, "addons", 11).await;
            assert!(
                !healed_hash.is_empty() && healed_hash != "STALE_BASELINE",
                "the first complete scan must heal the stale addon baseline"
            );
        }
        super::super::quick_scan::persist_pending_updates(context.clone(), repo_url, &diff).await;
    }
    assert!(
        main_file.exists(),
        "quick scan must preserve the current file"
    );
    assert!(
        sibling_file.exists(),
        "quick scan must preserve the sibling-owned file"
    );
    assert!(
        !pending_update_exists(&fdb, repo_url, &pending_local_path).await,
        "a complete clean scan must clear the saved pending payload"
    );
    assert!(
        pending_update_exists(&fdb, repo_url, &other_pending_local_path).await,
        "cleaning this installation must preserve pending work for the same URL at another folder"
    );
}

#[tokio::test]
async fn extras_alone_do_not_create_or_restore_pending_updates() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    let repo_url = "https://example.invalid/RepoAlpha/";
    let repo_root = temp.path().join("MainRepo");
    let addon_dir = repo_root.join("@mod_01");
    std::fs::create_dir_all(&addon_dir).expect("create addon directory");
    let file = addon_dir.join("data.pbo");
    std::fs::write(&file, b"managed file bytes").expect("write managed file");
    seed_repository(
        &fdb,
        1,
        "RepoAlpha",
        repo_url,
        &repo_root.to_string_lossy(),
        "REPO_HASH",
        "REPO_HASH",
        "",
    )
    .await;
    seed_addon(
        &fdb,
        11,
        "@mod_01",
        "https://example.invalid/RepoAlpha/@mod_01/",
        &addon_dir.to_string_lossy(),
        "ADDON_HASH",
        "ADDON_HASH",
        "",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 11).await;
    seed_file(
        &fdb,
        21,
        "data.pbo",
        "https://example.invalid/RepoAlpha/@mod_01/data.pbo",
        &file.to_string_lossy(),
        "FILE_HASH",
        "FILE_HASH",
        "",
        std::fs::metadata(&file).expect("file metadata").len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 11, 21).await;
    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(repo_root.to_string_lossy()),
    );
    assert!(refresh_content_hashes_for_repository(context.clone(), repo_url, None).await);

    let extra = addon_dir.join("user-extra.pbo");
    std::fs::write(&extra, b"user data").expect("write extra file");
    for index in 1..456 {
        std::fs::write(
            addon_dir.join(format!("old_{index:03}.pbo.key_01_v1.bisign")),
            b"old signature",
        )
        .unwrap();
    }
    let stale_payload = serde_json::to_string(&[ModDiffSummary {
        name: "@mod_01".into(),
        needs_update: true,
        total_bytes: 0,
        files: Vec::new(),
    }])
    .unwrap();
    crate::core::models::pending_update::save_pending_update_for_context(
        context.clone(),
        repo_url,
        &stale_payload,
    )
    .await
    .unwrap();
    let diff = quick_local_change_diff(
        context.clone(),
        repo_url,
        None,
        None,
        None,
        false,
        true,
        false,
        None,
    )
    .await;
    assert!(
        diff.is_empty(),
        "matching managed files must not prompt for an update because extras exist"
    );
    super::super::quick_scan::persist_pending_updates(context.clone(), repo_url, &diff).await;
    assert!(
        extra.exists(),
        "preview persistence must not delete the extra file"
    );
    let pending_local_path =
        crate::core::utils::content_hash::normalize_path(&repo_root.to_string_lossy());
    assert!(
        !pending_update_exists(&fdb, repo_url, &pending_local_path).await,
        "a complete check must clear the legacy cleanup-only pending payload"
    );
    assert!(refresh_content_hashes_for_repository(context.clone(), repo_url, None).await);
    let folder_hash =
        crate::core::utils::content_hash::calculate_addon_folder_content_hash(&addon_dir).unwrap();
    assert_eq!(checksums(&fdb, "addons", 11).await.2, folder_hash);
    for _ in 0..2 {
        let (tx, mut rx) = tokio::sync::broadcast::channel(256);
        let next = quick_local_change_diff(
            context.clone(),
            repo_url,
            None,
            None,
            Some(&tx),
            false,
            true,
            false,
            None,
        )
        .await;
        assert!(next.is_empty());
        assert!(!context.hash_evidence.is_incomplete());
        while let Ok(event) = rx.try_recv() {
            if let ProgressEvent::RecheckHashProgress { checked_files, .. } = event {
                assert_eq!(
                    checked_files, 0,
                    "unchanged extras must not cause repeated file checks"
                );
            }
        }
    }
    assert_eq!(std::fs::read(&extra).unwrap(), b"user data");
    std::fs::write(&file, b"changed file bytes").unwrap();
    let changed = quick_local_change_diff(
        context.clone(),
        repo_url,
        None,
        None,
        None,
        false,
        true,
        false,
        None,
    )
    .await;
    assert!(
        changed
            .iter()
            .any(|addon| addon.needs_update
                && addon.files.iter().any(|file| file.name == "data.pbo"))
    );
    std::fs::write(&file, b"managed file bytes").unwrap();
    assert!(refresh_content_hashes_for_repository(context.clone(), repo_url, None).await);
    let tree = Tree::load(context.clone(), repo_url).await.unwrap();
    let baseline = checksums(&fdb, "addons", 11).await.2;
    fdb.execute("DROP TABLE repository_addons", params![])
        .await
        .unwrap();
    assert!(!refresh_content_hashes_for_repository(context.clone(), repo_url, Some(tree)).await);
    assert!(context.hash_evidence.is_incomplete());
    assert_eq!(checksums(&fdb, "addons", 11).await.2, baseline);
    assert!(!pending_update_exists(&fdb, repo_url, &pending_local_path).await);
}

#[tokio::test]
async fn managed_path_query_failure_is_incomplete() {
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
    fdb.execute("DROP TABLE addon_files", params![])
        .await
        .expect("remove ownership relation for fault injection");

    let mod_filter = HashSet::from(["@mod_01".to_string()]);
    let unexpected = super::super::quick_scan::collect_unexpected_files_for_repo_mods(
        context.clone(),
        "https://example.invalid/RepoAlpha/",
        &mod_filter,
        None,
    )
    .await;
    assert!(
        unexpected.is_empty(),
        "failed ownership lookup yields no cleanup candidates"
    );
    assert!(
        context.hash_evidence.is_incomplete(),
        "ownership lookup failure must prevent a clean terminal result"
    );
}
