use super::*;

#[tokio::test]
async fn prepared_queue_requires_each_enabled_files_remote_graph() {
    let context = Arc::new(FoxyContext::new(
        crate::core::tasks::db_turso::build_test_database().await,
        reqwest::Client::new(),
    ));
    let db = context.db();
    db.execute("INSERT INTO repositories (id, name, remote_url, local_path) VALUES (1, 'MainRepo', 'http://repo.example.invalid:8080/', 'R:/Mods/MainRepo/')", params![]).await.unwrap();
    db.execute("INSERT INTO addons (id, name, required, enabled, remote_checksum) VALUES (1, '@mod_01', 1, 1, 'ADDON')", params![]).await.unwrap();
    db.execute(
        "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 1)",
        params![],
    )
    .await
    .unwrap();
    db.execute("INSERT INTO files (id, name, remote_checksum, length) VALUES (1, 'first.bin', 'FILE', 4), (2, 'second.bin', 'FILE', 4)", params![]).await.unwrap();
    db.execute(
        "INSERT INTO addon_files (addon_id, file_id) VALUES (1, 1), (1, 2)",
        params![],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO subfiles (file_id, path, remote_checksum) VALUES (1, 'first.bin', 'PART')",
        params![],
    )
    .await
    .unwrap();
    assert!(!prepared_remote_graph_is_complete(context.clone(), 1).await);
    db.execute(
        "INSERT INTO subfiles (file_id, path, remote_checksum) VALUES (2, 'second.bin', 'PART')",
        params![],
    )
    .await
    .unwrap();
    assert!(prepared_remote_graph_is_complete(context.clone(), 1).await);
    db.execute(
        "UPDATE subfiles SET remote_checksum = '' WHERE file_id = 2",
        params![],
    )
    .await
    .unwrap();
    assert!(!prepared_remote_graph_is_complete(context.clone(), 1).await);
    db.execute(
        "UPDATE subfiles SET remote_checksum = 'PART' WHERE file_id = 2",
        params![],
    )
    .await
    .unwrap();
    db.execute(
        "UPDATE addons SET remote_checksum = '' WHERE id = 1",
        params![],
    )
    .await
    .unwrap();
    assert!(!prepared_remote_graph_is_complete(context.clone(), 1).await);
    db.execute("UPDATE addons SET enabled = 0 WHERE id = 1", params![])
        .await
        .unwrap();
    assert!(prepared_remote_graph_is_complete(context, 1).await);
}

fn checksum(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_uppercase()
}

fn rollup(checksums: &[String]) -> String {
    checksum(checksums.concat().as_bytes())
}

#[allow(clippy::too_many_arguments)]
async fn seed_addon_file(
    db: &FoxyDb,
    repository_id: i64,
    addon_id: i64,
    file_id: i64,
    order: i64,
    name: &str,
    repository_url: &str,
    root: &Path,
    local_bytes: &[u8],
    remote_bytes: &[u8],
) -> (String, String) {
    let addon_path = root.join(name);
    std::fs::create_dir_all(&addon_path).unwrap();
    let file_path = addon_path.join("data.pbo");
    std::fs::write(&file_path, local_bytes).unwrap();
    let keep_path = addon_path.join("keep.pbo");
    std::fs::write(&keep_path, b"keep").unwrap();
    let local_part = checksum(local_bytes);
    let remote_part = checksum(remote_bytes);
    let local_file = rollup(std::slice::from_ref(&local_part));
    let remote_file = rollup(std::slice::from_ref(&remote_part));
    let keep_part = checksum(b"keep");
    let keep_file = rollup(std::slice::from_ref(&keep_part));
    let local_addon = rollup(&[local_file.clone(), keep_file.clone()]);
    let remote_addon = rollup(&[remote_file.clone(), keep_file.clone()]);
    db.execute(
        "INSERT INTO addons (id, name, remote_path, local_path, required, enabled, local_checksum, remote_checksum, data_order) \
         VALUES (?, ?, ?, ?, 1, 1, ?, ?, ?)",
        params![addon_id, name, format!("{repository_url}{name}/"), addon_path.to_string_lossy().to_string(), local_addon.clone(), remote_addon.clone(), order],
    ).await.unwrap();
    db.execute(
        "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, length, data_order) \
         VALUES (?, 'data.pbo', ?, ?, ?, ?, ?, 0)",
        params![file_id, format!("{repository_url}{name}/data.pbo"), file_path.to_string_lossy().to_string(), local_file, remote_file, remote_bytes.len() as i64],
    ).await.unwrap();
    let (stored_part, stored_length) = if local_bytes == remote_bytes {
        (String::new(), 0i64)
    } else {
        (local_part, local_bytes.len() as i64)
    };
    db.execute(
        "INSERT INTO subfiles (id, file_id, path, local_checksum, remote_checksum, local_length, remote_length, local_start, remote_start, data_order) \
         VALUES (?, ?, 'part0', ?, ?, ?, ?, 0, 0, 0)",
        params![file_id, file_id, stored_part, remote_part, stored_length, remote_bytes.len() as i64],
    ).await.unwrap();
    db.execute(
        "INSERT INTO repository_addons (repository_id, addon_id) VALUES (?, ?)",
        params![repository_id, addon_id],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
        params![addon_id, file_id],
    )
    .await
    .unwrap();
    let keep_id = file_id + 100;
    db.execute(
        "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, length, data_order) \
         VALUES (?, 'keep.pbo', ?, ?, ?, ?, 4, 1)",
        params![keep_id, format!("{repository_url}{name}/keep.pbo"), keep_path.to_string_lossy().to_string(), keep_file.clone(), keep_file],
    ).await.unwrap();
    db.execute(
        "INSERT INTO subfiles (id, file_id, path, local_checksum, remote_checksum, local_length, remote_length, local_start, remote_start, data_order) \
         VALUES (?, ?, 'part0', '', ?, 0, 4, 0, 0, 0)",
        params![keep_id, keep_id, keep_part],
    ).await.unwrap();
    db.execute(
        "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
        params![addon_id, keep_id],
    )
    .await
    .unwrap();
    (local_addon, remote_addon)
}

async fn file_state(db: &FoxyDb, file_id: i64) -> (String, String, String, i64, i64) {
    let row = db.query_one(
        "SELECT f.local_checksum, f.local_content_hash, sf.local_checksum AS part_checksum, sf.local_length, sf.local_start \
         FROM files f JOIN subfiles sf ON sf.file_id = f.id WHERE f.id = ?",
        params![file_id],
    ).await.unwrap().unwrap();
    (
        row.get_string("local_checksum").unwrap(),
        row.get_string("local_content_hash").unwrap(),
        row.get_string("part_checksum").unwrap(),
        row.get_i64("local_length").unwrap(),
        row.get_i64("local_start").unwrap(),
    )
}

async fn addon_state(db: &FoxyDb, addon_id: i64) -> (String, String) {
    let row = db
        .query_one(
            "SELECT local_checksum, local_content_hash FROM addons WHERE id = ?",
            params![addon_id],
        )
        .await
        .unwrap()
        .unwrap();
    (
        row.get_string("local_checksum").unwrap(),
        row.get_string("local_content_hash").unwrap(),
    )
}

async fn repository_state(db: &FoxyDb, repository_id: i64) -> (String, String) {
    let row = db
        .query_one(
            "SELECT local_checksum, local_content_hash FROM repositories WHERE id = ?",
            params![repository_id],
        )
        .await
        .unwrap()
        .unwrap();
    (
        row.get_string("local_checksum").unwrap(),
        row.get_string("local_content_hash").unwrap(),
    )
}

async fn queue_file(db: &FoxyDb, file_id: i64) {
    db.execute(
        "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size) \
         SELECT id, remote_path, local_path, length FROM files WHERE id = ?",
        params![file_id],
    ).await.unwrap();
}

fn hash_progress(
    receiver: &mut tokio::sync::broadcast::Receiver<ProgressEvent>,
) -> Vec<(usize, usize, u64)> {
    let mut scopes = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let ProgressEvent::RecheckHashProgress {
            total_files,
            total_parts,
            total_bytes,
            ..
        } = event
            && total_bytes > 0
        {
            scopes.push((total_files, total_parts, total_bytes));
        }
    }
    scopes
}

#[tokio::test]
async fn rolled_back_shared_file_recovers_in_the_pending_scope_without_rehashing_other_addons() {
    let temporary = tempfile::tempdir().unwrap();
    let shared_root = temporary.path().join("shared");
    let other_root = temporary.path().join("other");
    let source_url = "https://repo.example.invalid/RepoAlpha/";
    let sibling_url = "https://repo.example.invalid/RepoBeta/";
    let database = crate::core::tasks::db_turso::build_test_database().await;
    let db = FoxyDb::from_turso(database.clone());
    for (id, url, root) in [
        (1i64, source_url, &shared_root),
        (2, sibling_url, &shared_root),
        (3, sibling_url, &other_root),
    ] {
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (?, 'RepoAlpha', ?, ?)",
            params![id, url, root.to_string_lossy().to_string()],
        ).await.unwrap();
    }
    let source = seed_addon_file(
        &db,
        1,
        11,
        21,
        0,
        "@mod_01",
        source_url,
        &shared_root,
        b"new1",
        b"new1",
    )
    .await;
    let shared = seed_addon_file(
        &db,
        2,
        12,
        22,
        0,
        "@mod_01",
        sibling_url,
        &shared_root,
        b"new1",
        b"new1",
    )
    .await;
    let pending = seed_addon_file(
        &db,
        2,
        13,
        23,
        1,
        "@mod_02",
        sibling_url,
        &shared_root,
        b"old2",
        b"new2",
    )
    .await;
    let clean = seed_addon_file(
        &db,
        2,
        14,
        24,
        2,
        "@mod_03",
        sibling_url,
        &shared_root,
        b"same",
        b"same",
    )
    .await;
    let other = seed_addon_file(
        &db,
        3,
        15,
        25,
        0,
        "@mod_01",
        sibling_url,
        &other_root,
        b"new1",
        b"new1",
    )
    .await;
    for (id, local, remote) in [
        (1i64, rollup(&[source.0]), rollup(&[source.1])),
        (
            2,
            rollup(&[shared.0, pending.0, clean.0]),
            rollup(&[shared.1, pending.1, clean.1]),
        ),
        (3, rollup(&[other.0]), rollup(&[other.1])),
    ] {
        db.execute(
            "UPDATE repositories SET local_checksum = ?, remote_checksum = ? WHERE id = ?",
            params![local, remote, id],
        )
        .await
        .unwrap();
    }
    let context_for = |root: &Path| {
        Arc::new(
            FoxyContext::new(database.clone(), reqwest::Client::new())
                .with_target_local_path(root.to_string_lossy().to_string()),
        )
    };
    let source_context = context_for(&shared_root);
    let sibling_context = context_for(&shared_root);
    let other_context = context_for(&other_root);
    for (context, url) in [
        (source_context.clone(), source_url),
        (sibling_context.clone(), sibling_url),
        (other_context.clone(), sibling_url),
    ] {
        assert!(refresh_content_hashes_for_repository(context, url, None).await);
    }
    persist_pending_updates(
        sibling_context.clone(),
        sibling_url,
        &[ModDiffSummary {
            name: "@mod_02".to_string(),
            needs_update: true,
            total_bytes: 4,
            files: Vec::new(),
        }],
    )
    .await;
    persist_pending_updates(
        other_context.clone(),
        sibling_url,
        &[ModDiffSummary {
            name: "@mod_03".to_string(),
            needs_update: true,
            total_bytes: 4,
            files: Vec::new(),
        }],
    )
    .await;
    queue_file(&db, 23).await;
    assert!(prepared_queue_covers_unverified_files(sibling_context.clone(), 2).await);
    assert_eq!(
        pending_update_mod_scope(sibling_context.clone(), sibling_url).await,
        Some(HashSet::from(["@mod_02".to_string()])),
    );
    let pending_before = file_state(&db, 23).await;
    let shared_keep_before = file_state(&db, 122).await;
    let clean_before = file_state(&db, 24).await;
    let other_before = file_state(&db, 25).await;
    let clean_addon_before = addon_state(&db, 14).await;
    let other_addon_before = addon_state(&db, 15).await;
    let other_repository_before = repository_state(&db, 3).await;
    assert!(clean_before.2.is_empty());
    assert!(shared_keep_before.2.is_empty());

    std::fs::write(shared_root.join("@mod_01/data.pbo"), b"old1").unwrap();
    invalidate_local_hash_baseline(source_context, &HashSet::from([21]))
        .await
        .unwrap();
    let shared_addon = addon_state(&db, 12).await;
    let sibling_repository = repository_state(&db, 2).await;
    assert!(!shared_addon.0.is_empty());
    assert!(!sibling_repository.0.is_empty());
    assert!(shared_addon.1.is_empty());
    assert!(sibling_repository.1.is_empty());
    let parent_mismatch = db.query_one(
        "SELECT a.local_checksum != a.remote_checksum AS addon_mismatch, r.local_checksum != r.remote_checksum AS repository_mismatch \
         FROM addons a JOIN repositories r ON r.id = 2 WHERE a.id = 12",
        params![],
    ).await.unwrap().unwrap();
    assert_eq!(parent_mismatch.get_i64("addon_mismatch").unwrap(), 1);
    assert_eq!(parent_mismatch.get_i64("repository_mismatch").unwrap(), 1);
    assert!(!prepared_queue_covers_unverified_files(sibling_context.clone(), 2).await);
    assert!(prepared_queue_covers_unverified_files(other_context.clone(), 3).await);
    let scope = pending_update_mod_scope(sibling_context.clone(), sibling_url)
        .await
        .unwrap();
    assert_eq!(
        scope,
        HashSet::from(["@mod_01".to_string(), "@mod_02".to_string()])
    );
    assert_eq!(
        pending_update_mod_scope(other_context, sibling_url).await,
        Some(HashSet::from(["@mod_03".to_string()])),
    );
    queue_file(&db, 22).await;
    assert!(prepared_queue_covers_unverified_files(sibling_context.clone(), 2).await);

    let (progress, mut receiver) = tokio::sync::broadcast::channel(1024);
    let diffs = quick_local_change_diff(
        sibling_context.clone(),
        sibling_url,
        Some(&scope),
        None,
        Some(&progress),
        true,
        true,
        false,
        None,
    )
    .await;
    let updated: HashSet<String> = diffs
        .iter()
        .filter(|addon| addon.needs_update)
        .map(|addon| addon.name.clone())
        .collect();
    assert_eq!(updated, scope);
    let scopes = hash_progress(&mut receiver);
    assert!(!scopes.is_empty());
    assert!(scopes.iter().all(|scope| *scope == (1, 1, 4)));
    assert_eq!(file_state(&db, 22).await.2, checksum(b"old1"));
    assert_eq!(file_state(&db, 23).await, pending_before);
    assert_eq!(file_state(&db, 122).await, shared_keep_before);
    assert_eq!(file_state(&db, 24).await, clean_before);
    assert_eq!(file_state(&db, 25).await, other_before);
    assert_eq!(addon_state(&db, 14).await, clean_addon_before);
    assert_eq!(addon_state(&db, 15).await, other_addon_before);
    assert_eq!(repository_state(&db, 3).await, other_repository_before);

    persist_pending_updates(sibling_context.clone(), sibling_url, &diffs).await;
    let repeat = quick_local_change_diff(
        sibling_context.clone(),
        sibling_url,
        Some(&scope),
        None,
        Some(&progress),
        true,
        true,
        false,
        None,
    )
    .await;
    assert_eq!(
        repeat
            .iter()
            .filter(|addon| addon.needs_update)
            .map(|addon| addon.name.clone())
            .collect::<HashSet<_>>(),
        scope,
    );
    assert!(hash_progress(&mut receiver).is_empty());
    assert_eq!(file_state(&db, 122).await, shared_keep_before);
    assert_eq!(file_state(&db, 24).await, clean_before);
    assert_eq!(repository_state(&db, 3).await, other_repository_before);
}
