use super::*;
use crate::core::api::quick_scan::{
    collect_unexpected_files_for_repo_mods, delete_unexpected_local_files, pending_update_mod_scope,
};
use crate::core::db::{FoxyDb, params};

#[tokio::test]
async fn transfer_estimate_keeps_startup_removed_file_addon_in_pending_scope() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path().join("MainRepo");
    let transfer_addon = root.join("@mod_01");
    let removal_addon = root.join("@mod_02");
    std::fs::create_dir_all(&transfer_addon).expect("create transfer addon");
    std::fs::create_dir_all(&removal_addon).expect("create removal addon");
    let changed_path = transfer_addon.join("changed.bin");
    let kept_path = removal_addon.join("kept.pbo");
    let removed_path = removal_addon.join("old.pbo");
    std::fs::write(&changed_path, b"new local bytes").expect("write changed file");
    std::fs::write(&kept_path, b"kept bytes").expect("write kept file");
    std::fs::write(&removed_path, b"old bytes").expect("write removed file");

    let database = crate::core::tasks::db_turso::build_test_database().await;
    let db = FoxyDb::from_turso(database.clone());
    let url = "https://repo.example.invalid/RepoAlpha/";
    let root_string = root.to_string_lossy().into_owned();
    let transfer_dir = transfer_addon.to_string_lossy().into_owned();
    let removal_dir = removal_addon.to_string_lossy().into_owned();
    db.execute(
        "INSERT INTO repositories (id, name, remote_url, local_path, local_checksum, remote_checksum, local_content_hash) VALUES (1, 'RepoAlpha', ?, ?, 'REPO', 'REPO', 'BASELINE')",
        params![url, root_string.as_str()],
    )
    .await
    .expect("seed repository");
    for (id, name, remote_path, local_path) in [
        (
            11_i64,
            "@mod_01",
            format!("{url}@mod_01/"),
            transfer_dir.clone(),
        ),
        (
            12_i64,
            "@mod_02",
            format!("{url}@mod_02/"),
            removal_dir.clone(),
        ),
    ] {
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, enabled, local_checksum, remote_checksum, local_content_hash, required, data_order) VALUES (?, ?, ?, ?, 1, 'ADDON', 'ADDON', 'BASELINE', 1, ?)",
            params![id, name, remote_path, local_path, id],
        )
        .await
        .expect("seed addon");
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, ?)",
            params![id],
        )
        .await
        .expect("link addon");
    }
    let changed_length = std::fs::metadata(&changed_path)
        .expect("changed metadata")
        .len();
    for (id, name, remote_path, local_path, addon_id, local_checksum, remote_checksum) in [
        (
            21_i64,
            "changed.bin",
            format!("{url}@mod_01/changed.bin"),
            changed_path.to_string_lossy().into_owned(),
            11_i64,
            "LOCAL_OLD",
            "REMOTE_NEW",
        ),
        (
            22_i64,
            "kept.pbo",
            format!("{url}@mod_02/kept.pbo"),
            kept_path.to_string_lossy().into_owned(),
            12_i64,
            "KEPT",
            "KEPT",
        ),
    ] {
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash, length, data_order) VALUES (?, ?, ?, ?, ?, ?, '', ?, 0)",
            params![id, name, remote_path, local_path, local_checksum, remote_checksum, if id == 21 { changed_length as i64 } else { 10_i64 }],
        )
        .await
        .expect("seed managed file");
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
            params![addon_id, id],
        )
        .await
        .expect("link managed file");
    }
    db.execute(
        "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash, length, data_order) VALUES (23, 'old.pbo', ?, ?, 'OLD', 'OLD', '', 9, 1)",
        params![format!("{url}@mod_02/old.pbo"), removed_path.to_string_lossy().into_owned()],
    )
    .await
    .expect("seed prior orphan file record");
    db.execute(
        "INSERT INTO subfiles (id, file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) VALUES (31, 21, 'changed.bin', ?, 0, ?, 0, 'PART_OLD', 'PART_NEW', 0)",
        params![changed_length as i64, changed_length as i64],
    )
    .await
    .expect("seed changed file part state");
    db.execute(
        "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size) VALUES (21, ?, ?, ?)",
        params![format!("{url}@mod_01/changed.bin"), changed_path.to_string_lossy().into_owned(), changed_length as i64],
    )
    .await
    .expect("seed transfer target");

    let context = Arc::new(
        FoxyContext::new(database, reqwest::Client::new())
            .with_target_local_path(root_string.clone()),
    );
    assert!(
        crate::core::api::quick_scan::refresh_content_hashes_for_repository(
            context.clone(),
            url,
            None
        )
        .await
    );
    db.execute(
        "UPDATE addons SET local_checksum = 'LOCAL_ADDON', remote_checksum = 'REMOTE_ADDON' WHERE id = 11",
        params![],
    )
    .await
    .expect("set known transfer mismatch");
    let shared_cache = Arc::new(Mutex::new(QuickScanSharedCache::default()));
    let outcome = run_quick_scan_worker_repo(
        context.clone(),
        shared_cache,
        url.to_owned(),
        root_string,
        true,
        false,
        None,
    )
    .await;

    assert!(!outcome.result.skipped);
    assert!(
        !context.hash_evidence.is_incomplete(),
        "quick scan should have complete evidence"
    );
    let transfer = outcome
        .result
        .mods
        .iter()
        .find(|addon| addon.name == "@mod_01")
        .expect("transfer addon remains in result");
    assert_eq!(transfer.total_bytes, changed_length);
    let removal = outcome
        .result
        .mods
        .iter()
        .find(|addon| addon.name == "@mod_02")
        .expect("deletion-only addon remains in result");
    assert_eq!(removal.total_bytes, 0);
    assert!(removal.files.iter().any(|file| {
        file.name == "old.pbo" && file.change_kind == FileDiffKind::Deleted && file.total_bytes == 0
    }));

    let payload =
        crate::core::models::pending_update::fetch_pending_update_for_context(context.clone(), url)
            .await
            .expect("read pending payload")
            .expect("pending payload exists");
    let persisted: Vec<ModDiffSummary> = serde_json::from_str(&payload).expect("decode payload");
    assert!(persisted.iter().any(|addon| addon.name == "@mod_01"));
    assert!(persisted.iter().any(|addon| addon.name == "@mod_02"));
    let scope = pending_update_mod_scope(context.clone(), url)
        .await
        .expect("both addons are in pending scope");
    assert_eq!(
        scope,
        HashSet::from(["@mod_01".to_owned(), "@mod_02".to_owned()])
    );

    let unexpected =
        collect_unexpected_files_for_repo_mods(context.clone(), url, &scope, None).await;
    assert!(
        unexpected
            .values()
            .flatten()
            .any(|path| path == removed_path.to_string_lossy().as_ref())
    );
    assert_eq!(delete_unexpected_local_files(&unexpected).await, (1, 0));
    assert!(!removed_path.exists());
    assert!(kept_path.exists());
}
