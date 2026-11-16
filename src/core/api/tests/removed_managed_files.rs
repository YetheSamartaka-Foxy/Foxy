use super::*;
use crate::core::api::FileDiffKind;
use crate::core::api::quick_scan::{
    collect_unexpected_files_for_repo_mods, delete_unexpected_local_files,
};

#[tokio::test]
async fn pruned_file_link_reports_deleted_and_then_converges_after_cleanup() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    let repo_url = "https://repo.example.invalid/RepoAlpha/";
    let repo_root = temp.path().join("MainRepo");
    let addon_dir = repo_root.join("@mod_01");
    std::fs::create_dir_all(&addon_dir).expect("create addon directory");
    let kept = addon_dir.join("kept.pbo");
    let removed = addon_dir.join("old.pbo");
    std::fs::write(&kept, b"kept payload").expect("write kept file");
    std::fs::write(&removed, b"old payload").expect("write obsolete file");

    seed_repository(
        &fdb,
        1,
        "RepoAlpha",
        repo_url,
        &repo_root.to_string_lossy(),
        "REPO",
        "REPO",
        "",
    )
    .await;
    seed_addon(
        &fdb,
        11,
        "@mod_01",
        "https://repo.example.invalid/RepoAlpha/@mod_01/",
        &addon_dir.to_string_lossy(),
        "ADDON",
        "ADDON",
        "",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 11).await;
    seed_addon(
        &fdb,
        12,
        "@mod_02",
        "https://repo.example.invalid/RepoAlpha/@mod_02/",
        &addon_dir.to_string_lossy(),
        "ADDON",
        "ADDON",
        "",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 12).await;
    for (id, name, path) in [(21, "kept.pbo", &kept), (22, "old.pbo", &removed)] {
        seed_file(
            &fdb,
            id,
            name,
            &format!("https://repo.example.invalid/RepoAlpha/@mod_01/{name}"),
            &path.to_string_lossy(),
            "FILE",
            "FILE",
            "",
            std::fs::metadata(path).expect("file metadata").len() as i64,
            id,
        )
        .await;
        seed_addon_file(&fdb, 11, id).await;
    }
    seed_file(
        &fdb,
        23,
        "kept-alias.pbo",
        "https://repo.example.invalid/RepoAlpha/@mod_02/kept-alias.pbo",
        &kept.to_string_lossy(),
        "FILE",
        "FILE",
        "",
        std::fs::metadata(&kept)
            .expect("sibling file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 12, 23).await;

    let other_root = temp.path().join("OtherInstall");
    seed_repository(
        &fdb,
        2,
        "RepoAlpha second install",
        repo_url,
        &other_root.to_string_lossy(),
        "REPO",
        "REPO",
        "",
    )
    .await;

    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(repo_root.to_string_lossy()),
    );
    assert!(refresh_content_hashes_for_repository(context.clone(), repo_url, None).await);

    fdb.execute(
        "DELETE FROM addon_files WHERE addon_id = ? AND file_id = ?",
        params![11i64, 22i64],
    )
    .await
    .expect("prune obsolete addon/file link");

    let scan = || {
        quick_local_change_diff(
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
    };
    let first = scan().await;
    assert_eq!(first.len(), 1);
    assert!(first[0].needs_update);
    assert_eq!(first[0].total_bytes, 0);
    assert_eq!(first[0].files.len(), 1);
    let deleted = &first[0].files[0];
    assert_eq!(deleted.name, "old.pbo");
    assert!(deleted.needs_update);
    assert_eq!(deleted.change_kind, FileDiffKind::Deleted);
    assert_eq!(deleted.total_bytes, 0);
    assert!(removed.exists(), "quick scan must not delete local files");
    crate::core::api::quick_scan::persist_pending_updates(context.clone(), repo_url, &first).await;
    let payload = crate::core::models::pending_update::fetch_pending_update_for_context(
        context.clone(),
        repo_url,
    )
    .await
    .expect("fetch pending update")
    .expect("deleted file summary must persist");
    let payload: serde_json::Value = serde_json::from_str(&payload).expect("parse pending update");
    assert_eq!(payload[0]["files"][0]["change_kind"], "deleted");
    assert_eq!(payload[0]["files"][0]["total_bytes"], 0);
    assert_eq!(
        scan().await,
        first,
        "a repeated check must retain the deletion"
    );
    let disabled = HashMap::from([("@mod_01".to_string(), false)]);
    let disabled_diff = quick_local_change_diff(
        context.clone(),
        repo_url,
        None,
        Some(&disabled),
        None,
        false,
        true,
        false,
        None,
    )
    .await;
    assert!(disabled_diff.iter().all(|addon| !addon.needs_update));
    let other_addon = HashSet::from(["@mod_02".to_string()]);
    let scoped_diff = quick_local_change_diff(
        context.clone(),
        repo_url,
        Some(&other_addon),
        None,
        None,
        false,
        true,
        false,
        None,
    )
    .await;
    assert!(scoped_diff.iter().all(|addon| !addon.needs_update));

    let unexpected = collect_unexpected_files_for_repo_mods(
        context.clone(),
        repo_url,
        &HashSet::from(["@mod_01".to_string()]),
        None,
    )
    .await;
    assert_eq!(delete_unexpected_local_files(&unexpected).await, (1, 0));
    assert!(!removed.exists());

    for _ in 0..2 {
        let clean = scan().await;
        assert!(
            clean.iter().all(|addon| !addon.needs_update),
            "after cleanup, repeated quick scans must be clean"
        );
        assert!(context.hash_evidence.issues_snapshot().is_empty());
    }
    assert_eq!(
        std::fs::read(&kept).expect("kept file remains"),
        b"kept payload"
    );
}

#[tokio::test]
async fn repository_instance_file_ownership_is_scoped_by_folder_and_normalized_path() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    let repo_url = "https://repo.example.invalid/RepoAlpha/";
    let repo1 = temp.path().join("InstallOne");
    let repo2 = temp.path().join("InstallTwo");
    let addon1 = repo1.join("@mod_01");
    let addon2 = repo2.join("@mod_01");
    std::fs::create_dir_all(&addon1).expect("create first addon");
    std::fs::create_dir_all(&addon2).expect("create second addon");
    let sibling_owned = addon2.join("shared.pbo");
    let repo2_removed = addon2.join("old.pbo");
    let repo1_kept = addon1.join("kept.pbo");
    std::fs::write(&sibling_owned, b"owned by second install").expect("write shared file");
    std::fs::write(&repo2_removed, b"obsolete second install file")
        .expect("write second obsolete file");
    std::fs::write(&repo1_kept, b"first install file").expect("write first install file");

    for (repo_id, path, addon_id, addon_path) in
        [(1, &repo1, 11, &addon1), (2, &repo2, 12, &addon2)]
    {
        seed_repository(
            &fdb,
            repo_id,
            "RepoAlpha",
            repo_url,
            &path.to_string_lossy(),
            "REPO",
            "REPO",
            "",
        )
        .await;
        seed_addon(
            &fdb,
            addon_id,
            "@mod_01",
            "https://repo.example.invalid/RepoAlpha/@mod_01/",
            &addon_path.to_string_lossy(),
            "ADDON",
            "ADDON",
            "",
            true,
        )
        .await;
        seed_repository_addon(&fdb, repo_id, addon_id).await;
    }

    seed_file(
        &fdb,
        24,
        "kept.pbo",
        "https://repo.example.invalid/RepoAlpha/@mod_01/kept.pbo",
        &repo1_kept.to_string_lossy(),
        "FILE",
        "FILE",
        "",
        std::fs::metadata(&repo1_kept)
            .expect("first install file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 11, 24).await;

    let shared_path = sibling_owned.to_string_lossy().replace('\\', "/");
    let shared_path_alias = if cfg!(windows) {
        shared_path.to_ascii_lowercase()
    } else {
        shared_path.clone()
    };
    seed_file(
        &fdb,
        21,
        "former-shared.pbo",
        "https://repo.example.invalid/RepoAlpha/@mod_01/former-shared.pbo",
        &shared_path_alias,
        "FILE",
        "FILE",
        "",
        std::fs::metadata(&sibling_owned)
            .expect("shared file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 11, 21).await;
    seed_file(
        &fdb,
        22,
        "shared.pbo",
        "https://repo.example.invalid/RepoAlpha/@mod_01/shared.pbo",
        &shared_path,
        "FILE",
        "FILE",
        "",
        std::fs::metadata(&sibling_owned)
            .expect("shared file metadata")
            .len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 12, 22).await;
    seed_file(
        &fdb,
        23,
        "old.pbo",
        "https://repo.example.invalid/RepoAlpha/@mod_01/old.pbo",
        &repo2_removed.to_string_lossy(),
        "FILE",
        "FILE",
        "",
        std::fs::metadata(&repo2_removed)
            .expect("obsolete file metadata")
            .len() as i64,
        1,
    )
    .await;
    seed_addon_file(&fdb, 12, 23).await;

    let context1 = Arc::new(
        FoxyContext::new(db.clone(), reqwest::Client::new())
            .with_target_local_path(repo1.to_string_lossy()),
    );
    let context2 = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(repo2.to_string_lossy()),
    );
    assert!(refresh_content_hashes_for_repository(context1.clone(), repo_url, None).await);
    assert!(refresh_content_hashes_for_repository(context2.clone(), repo_url, None).await);

    for (addon_id, file_id) in [(11i64, 21i64), (12i64, 23i64)] {
        fdb.execute(
            "DELETE FROM addon_files WHERE addon_id = ? AND file_id = ?",
            params![addon_id, file_id],
        )
        .await
        .expect("prune obsolete manifest link");
    }

    let scan = |context: Arc<FoxyContext>| async move {
        quick_local_change_diff(
            context, repo_url, None, None, None, false, true, false, None,
        )
        .await
    };
    let first_install_diff = scan(context1).await;
    assert!(
        first_install_diff.iter().all(|addon| !addon.needs_update),
        "the orphan from the first install must remain protected by the second install's owned path"
    );
    let second_install_diff = scan(context2).await;
    let deleted = second_install_diff
        .iter()
        .flat_map(|addon| &addon.files)
        .find(|file| file.name == "old.pbo")
        .expect("the second install must report its own removed file");
    assert_eq!(deleted.change_kind, FileDiffKind::Deleted);
    assert!(deleted.needs_update);
    assert_eq!(deleted.total_bytes, 0);
    assert!(sibling_owned.exists());
}

#[tokio::test]
async fn never_managed_extra_stays_clean_and_is_preserved() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    let repo_url = "https://repo.example.invalid/RepoAlpha/";
    let repo_root = temp.path().join("MainRepo");
    let addon_dir = repo_root.join("@mod_01");
    std::fs::create_dir_all(&addon_dir).expect("create addon directory");
    let managed = addon_dir.join("kept.pbo");
    let extra = addon_dir.join("user-extra.pbo");
    std::fs::write(&managed, b"managed").expect("write managed file");
    std::fs::write(&extra, b"user data").expect("write extra file");
    seed_repository(
        &fdb,
        1,
        "RepoAlpha",
        repo_url,
        &repo_root.to_string_lossy(),
        "REPO",
        "REPO",
        "",
    )
    .await;
    seed_addon(
        &fdb,
        11,
        "@mod_01",
        "https://repo.example.invalid/RepoAlpha/@mod_01/",
        &addon_dir.to_string_lossy(),
        "ADDON",
        "ADDON",
        "",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 11).await;
    seed_file(
        &fdb,
        21,
        "kept.pbo",
        "https://repo.example.invalid/RepoAlpha/@mod_01/kept.pbo",
        &managed.to_string_lossy(),
        "FILE",
        "FILE",
        "",
        std::fs::metadata(&managed).expect("file metadata").len() as i64,
        0,
    )
    .await;
    seed_addon_file(&fdb, 11, 21).await;
    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(repo_root.to_string_lossy()),
    );
    assert!(refresh_content_hashes_for_repository(context.clone(), repo_url, None).await);

    for _ in 0..2 {
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
        assert!(diff.iter().all(|addon| !addon.needs_update));
        assert!(context.hash_evidence.issues_snapshot().is_empty());
    }
    assert_eq!(
        std::fs::read(&extra).expect("extra file remains"),
        b"user data"
    );
}
