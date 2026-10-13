use super::*;
use crate::core::models::pending_update::{
    fetch_pending_update_for_context, save_pending_update_for_context,
};
use crate::core::tasks::download_files::{MissingRemoteFiles, download_files};
use crate::core::tasks::remote_repository::remote_repository;
use crate::ui::types::HashAlgorithmPreference;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

async fn recover_renamed_file(status: u16, fail_manifest_once: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let repository_url = format!("http://{}/", listener.local_addr().unwrap());
    let missing_requests = Arc::new(AtomicUsize::new(0));
    let manifest_requests = Arc::new(AtomicUsize::new(0));
    let fail_manifest = Arc::new(AtomicBool::new(false));
    let server = {
        let missing_requests = missing_requests.clone();
        let manifest_requests = manifest_requests.clone();
        let fail_manifest = fail_manifest.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 8192];
                let length = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let mut words = request.split_whitespace();
                let method = words.next().unwrap();
                let path = words.next().unwrap();
                let (response_status, body) = match path {
                    "/repo.json" => (
                        200,
                        r#"{"repoName":"MainRepo","checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"}],"optionalMods":[]}"#,
                    ),
                    "/@mod_01/mod.srf" => {
                        manifest_requests.fetch_add(1, Ordering::SeqCst);
                        if fail_manifest.load(Ordering::SeqCst) {
                            (503, "")
                        } else {
                            (
                                200,
                                r#"{"files":[{"path":"renamed.bin","checksum":"FILE","length":4,"parts":[{"path":"renamed.bin","checksum":"PART","start":0,"length":4}]},{"path":"keep.bin","checksum":"KEEP","length":4,"parts":[{"path":"keep.bin","checksum":"KEEP_PART","start":0,"length":4}]}]}"#,
                            )
                        }
                    }
                    "/@mod_01/obsolete.bin" => {
                        if method == "GET" {
                            missing_requests.fetch_add(1, Ordering::SeqCst);
                        }
                        (status, "")
                    }
                    _ => (404, ""),
                };
                let response = format!(
                    "HTTP/1.1 {response_status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        })
    };
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("first");
    let other_root = directory.path().join("second");
    std::fs::create_dir_all(root.join("@mod_01")).unwrap();
    std::fs::write(root.join("@mod_01/keep.bin"), b"keep").unwrap();
    let database = crate::core::tasks::db_turso::build_test_database().await;
    let context_for = |path: &std::path::Path| {
        Arc::new(
            FoxyContext::new(database.clone(), reqwest::Client::new())
                .with_target_local_path(path.to_string_lossy().to_string()),
        )
    };
    let context = context_for(&root);
    let other_context = context_for(&other_root);
    let db = context.db();
    for (id, path) in [(1i64, &root), (2, &other_root)] {
        let local_path = path.to_string_lossy().replace('\\', "/");
        db.execute("INSERT INTO repositories (id, name, remote_url, local_path, remote_checksum, local_checksum, local_content_hash) VALUES (?, 'MainRepo', ?, ?, 'REPO', 'OLD_REPO', 'CONTENT')", params![id, repository_url.clone(), format!("{local_path}/")]).await.unwrap();
        db.execute("INSERT INTO addons (id, name, remote_path, local_path, enabled, required, local_checksum, remote_checksum, local_content_hash, data_order) VALUES (?, '@mod_01', ?, ?, 1, 1, 'OLD_ADDON', 'ADDON', 'CONTENT', 0)", params![id, format!("{repository_url}@mod_01"), format!("{local_path}/@mod_01")]).await.unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (?, ?)",
            params![id, id],
        )
        .await
        .unwrap();
        db.execute("INSERT INTO files (id, name, remote_path, local_path, remote_checksum, length, data_order) VALUES (?, 'obsolete.bin', ?, ?, 'FILE', 4, 0)", params![id, format!("{repository_url}@mod_01/obsolete.bin"), format!("{local_path}/@mod_01/obsolete.bin")]).await.unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
            params![id, id],
        )
        .await
        .unwrap();
        db.execute("INSERT INTO subfiles (id, file_id, path, remote_checksum, local_checksum, remote_start, remote_length, local_start, local_length, data_order) VALUES (?, ?, 'obsolete.bin', 'PART', '', 0, 4, 0, 0, 0)", params![id, id]).await.unwrap();
        db.execute("INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size) VALUES (?, ?, ?, 4)", params![id, format!("{repository_url}@mod_01/obsolete.bin"), format!("{local_path}/@mod_01/obsolete.bin")]).await.unwrap();
    }
    let keep_path = root
        .join("@mod_01/keep.bin")
        .to_string_lossy()
        .replace('\\', "/");
    db.execute("INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, length, data_order) VALUES (3, 'keep.bin', ?, ?, 'KEEP', 'KEEP', 4, 1)", params![format!("{repository_url}@mod_01/keep.bin"), keep_path]).await.unwrap();
    db.execute(
        "INSERT INTO addon_files (addon_id, file_id) VALUES (1, 3)",
        params![],
    )
    .await
    .unwrap();
    db.execute("INSERT INTO subfiles (id, file_id, path, local_checksum, remote_checksum, local_start, local_length, remote_start, remote_length, data_order) VALUES (3, 3, 'keep.bin', '', 'KEEP_PART', 0, 0, 0, 4, 0)", params![]).await.unwrap();
    for context in [context.clone(), other_context.clone()] {
        save_pending_update_for_context(context, &repository_url, "[]")
            .await
            .unwrap();
    }
    let (_pause_tx, pause_rx) = watch::channel(false);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        download_files(
            context.clone(),
            None,
            None,
            pause_rx,
            cancel_rx.clone(),
            None,
            None,
            Some(std::collections::HashSet::from([1])),
            "missing-file-test".to_string(),
            Arc::new(std::sync::OnceLock::new()),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<MissingRemoteFiles>());
    assert!(!*cancel_rx.borrow());
    assert_eq!(missing_requests.load(Ordering::SeqCst), 1);
    invalidate_remote_graph(context.clone(), &repository_url)
        .await
        .unwrap();
    assert!(
        fetch_pending_update_for_context(context, &repository_url)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fetch_pending_update_for_context(other_context, &repository_url)
            .await
            .unwrap()
            .is_some()
    );
    let targets = db
        .query_all("SELECT file_id FROM download_target_file", params![])
        .await
        .unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].get_i64("file_id").unwrap(), 2);
    let other_repo = db
        .query_one(
            "SELECT remote_checksum FROM repositories WHERE id = 2",
            params![],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other_repo.get_string("remote_checksum").unwrap(), "REPO");
    fail_manifest.store(fail_manifest_once, Ordering::SeqCst);
    let restarted = context_for(&root);
    if fail_manifest_once {
        assert!(
            remote_repository(
                restarted.clone(),
                &repository_url,
                Some(&root.to_string_lossy()),
                None,
                false,
                HashAlgorithmPreference::PreferSwifty
            )
            .await
            .is_none()
        );
        let addon = db
            .query_one("SELECT remote_checksum FROM addons WHERE id = 1", params![])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(addon.get_string("remote_checksum").unwrap(), "");
        fail_manifest.store(false, Ordering::SeqCst);
    }
    let result = remote_repository(
        restarted.clone(),
        &repository_url,
        Some(&root.to_string_lossy()),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .unwrap();
    assert!(result.remote_graph_complete);
    assert!(result.remote_graph_fetched);
    let files = db.query_all("SELECT f.name FROM files f JOIN addon_files af ON af.file_id = f.id WHERE af.addon_id = 1 ORDER BY f.data_order", params![]).await.unwrap();
    assert_eq!(
        files
            .iter()
            .map(|row| row.get_string("name").unwrap())
            .collect::<Vec<_>>(),
        ["renamed.bin", "keep.bin"]
    );
    let targets = db.query_all("SELECT dt.download_remote_url FROM download_target_file dt JOIN addon_files af ON af.file_id = dt.file_id WHERE af.addon_id = 1", params![]).await.unwrap();
    assert_eq!(targets.len(), 1);
    assert!(
        targets[0]
            .get_string("download_remote_url")
            .unwrap()
            .ends_with("/renamed.bin")
    );
    let keep = db.query_one("SELECT f.local_checksum, sf.local_checksum AS part_local_checksum, sf.remote_checksum FROM files f JOIN subfiles sf ON sf.file_id = f.id WHERE f.id = 3", params![]).await.unwrap().unwrap();
    assert_eq!(keep.get_string("local_checksum").unwrap(), "KEEP");
    assert_eq!(keep.get_string("part_local_checksum").unwrap(), "");
    assert_eq!(keep.get_string("remote_checksum").unwrap(), "KEEP_PART");
    let before = manifest_requests.load(Ordering::SeqCst);
    let result = remote_repository(
        restarted,
        &repository_url,
        Some(&root.to_string_lossy()),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .unwrap();
    assert!(!result.remote_graph_fetched);
    assert_eq!(manifest_requests.load(Ordering::SeqCst), before);
    server.abort();
}

#[tokio::test]
async fn missing_file_refreshes_renamed_queue_with_unchanged_checksums() {
    recover_renamed_file(404, false).await;
}

#[tokio::test]
async fn gone_file_recovers_even_after_a_failed_manifest_refresh() {
    recover_renamed_file(410, true).await;
}
