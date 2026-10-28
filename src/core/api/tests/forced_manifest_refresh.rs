use super::*;
use crate::core::api::ModDiffSummary;
use crate::core::api::quick_scan::{
    collect_unexpected_files_for_repo_mods, delete_unexpected_local_files,
};
use crate::core::api::sync_pipeline::recheck_reapplied_addons;
use crate::core::models::pending_update::{
    clear_pending_update_for_context, fetch_pending_update_for_context,
    save_pending_update_for_context,
};
use crate::core::tasks::remote_repository::remote_repository;
use crate::core::utils::manifest_cache::{ManifestCache, Validators};
use crate::ui::types::HashAlgorithmPreference;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn assert_pending_manifest_refresh(
    local_checksum: &str,
    manual_copy: bool,
    fail_manifest: bool,
    invalid_manifest: bool,
    changed_addon_version: bool,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_url = format!("http://{}/", listener.local_addr().unwrap());
    let manifest_requests = Arc::new(AtomicU64::new(0));
    let requests = manifest_requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            let (status, body) = if request.starts_with("GET /repo.json ") {
                (
                    "200 OK",
                    r#"{"repoName":"MainRepo","checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"},{"modName":"@mod_02","checkSum":"ADDON"}]}"#,
                )
            } else {
                assert!(
                    request.starts_with("GET /@mod_01/mod.srf "),
                    "unexpected request: {:?}",
                    request.lines().next()
                );
                requests.fetch_add(1, Ordering::Relaxed);
                if fail_manifest {
                    ("404 Not Found", "{}")
                } else if invalid_manifest {
                    (
                        "200 OK",
                        r#"{"files":[{"path":"old.bin","checksum":"FILE","length":4,"parts":[{"path":"old.bin","checksum":"PART","start":0,"length":5}]}]}"#,
                    )
                } else if request.to_ascii_lowercase().contains("if-none-match:") {
                    ("304 Not Modified", "")
                } else {
                    (
                        "200 OK",
                        r#"{"files":[{"path":"old.bin","checksum":"FILE","length":4,"parts":[{"path":"old.bin","checksum":"PART","start":0,"length":4}]},{"path":"addons/new.pbo","checksum":"NEW_FILE","length":4,"parts":[{"path":"new.pbo","checksum":"NEW_PART","start":0,"length":4}]}]}"#,
                    )
                }
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let directory = tempfile::tempdir().unwrap();
    let local_path = format!("{}/", directory.path().to_string_lossy().replace('\\', "/"));
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    seed_repository(
        &fdb,
        1,
        "MainRepo",
        &remote_url,
        &local_path,
        local_checksum,
        "REPO",
        "CONTENT",
    )
    .await;
    for id in 1..=2 {
        let name = format!("@mod_{id:02}");
        let addon_path = format!("{local_path}{name}");
        std::fs::create_dir_all(format!("{addon_path}/addons")).unwrap();
        std::fs::write(format!("{addon_path}/old.bin"), b"data").unwrap();
        seed_addon(
            &fdb,
            id,
            &name,
            &format!("{remote_url}{name}"),
            &addon_path,
            "ADDON",
            if changed_addon_version && id == 1 {
                "OLD"
            } else {
                "ADDON"
            },
            "CONTENT",
            true,
        )
        .await;
        seed_repository_addon(&fdb, 1, id).await;
        seed_file(
            &fdb,
            id,
            "old.bin",
            &format!("{remote_url}{name}/old.bin"),
            &format!("{addon_path}/old.bin"),
            "FILE",
            "FILE",
            "CONTENT",
            4,
            0,
        )
        .await;
        seed_addon_file(&fdb, id, id).await;
        seed_subfile(&fdb, id, id, "PART", "PART").await;
    }
    let copied_file = directory.path().join("@mod_01/addons/new.pbo");
    if manual_copy {
        std::fs::write(&copied_file, b"data").unwrap();
    }
    let obsolete_file = directory.path().join("@mod_01/obsolete.bin");
    std::fs::write(&obsolete_file, b"old").unwrap();
    let cache = ManifestCache::in_space(directory.path());
    cache.store(
        &format!("{remote_url}@mod_01/mod.srf"),
        &Validators { etag: Some("\"stale\"".to_string()), last_modified: None },
        br#"{"files":[{"path":"old.bin","checksum":"FILE","length":4,"parts":[{"path":"old.bin","checksum":"PART","start":0,"length":4}]}]}"#,
    ).unwrap();
    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_manifest_cache(cache)
            .with_target_local_path(&local_path)
            .with_forced_mod_refreshes(if changed_addon_version {
                HashSet::new()
            } else {
                HashSet::from(["@mod_01".to_string()])
            }),
    );
    let metadata = remote_repository(
        context.clone(),
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await;
    if fail_manifest || invalid_manifest {
        assert!(metadata.is_none());
        assert!(manifest_requests.load(Ordering::Relaxed) > 0);
        assert_eq!(std::fs::read(&copied_file).unwrap(), b"data");
        assert!(obsolete_file.exists());
        assert_eq!(checksums(&fdb, "addons", 1).await.1, "");
        server.abort();
        return;
    }
    let metadata = metadata.expect("forced manifests should refresh successfully");
    assert!(metadata.remote_graph_fetched);
    assert!(metadata.remote_graph_complete);
    assert!(!metadata.skipped);
    assert_eq!(manifest_requests.load(Ordering::Relaxed), 1);
    let targets = fetch_all_download_targets_with_mod(context.clone())
        .await
        .unwrap();
    assert!(targets.iter().any(|target| {
        target
            .download
            .download_local_path
            .ends_with("/addons/new.pbo")
    }));
    let extras = collect_unexpected_files_for_repo_mods(
        context.clone(),
        &remote_url,
        &HashSet::from(["@mod_01".to_string()]),
        None,
    )
    .await;
    assert_eq!(extras.values().map(Vec::len).sum::<usize>(), 1);
    assert_eq!(delete_unexpected_local_files(&extras).await, (1, 0));
    assert!(!obsolete_file.exists());
    if manual_copy {
        assert_eq!(std::fs::read(&copied_file).unwrap(), b"data");
    }
    let unforced = Arc::new(
        context
            .as_ref()
            .clone()
            .with_forced_mod_refreshes(HashSet::new()),
    );
    for _ in 0..2 {
        let metadata = remote_repository(
            unforced.clone(),
            &remote_url,
            Some(&local_path),
            None,
            false,
            HashAlgorithmPreference::PreferSwifty,
        )
        .await
        .unwrap();
        assert!(!metadata.remote_graph_fetched);
    }
    assert_eq!(manifest_requests.load(Ordering::Relaxed), 1);
    server.abort();
}

#[tokio::test]
async fn pending_manifest_refresh_preserves_published_manual_copy_with_clean_repo_checksum() {
    assert_pending_manifest_refresh("REPO", true, false, false, false).await;
}

#[tokio::test]
async fn pending_manifest_refresh_queues_missing_file_with_unchanged_remote_graph() {
    assert_pending_manifest_refresh("LOCAL", false, false, false, false).await;
}

#[tokio::test]
async fn pending_manifest_refresh_failure_refuses_stale_metadata_and_preserves_manual_copy() {
    assert_pending_manifest_refresh("REPO", true, true, false, false).await;
}

#[tokio::test]
async fn pending_manifest_refresh_rejects_invalid_layout_and_preserves_manual_copy() {
    assert_pending_manifest_refresh("REPO", true, false, true, false).await;
}

#[tokio::test]
async fn changed_addon_version_bypasses_stale_manifest_cache_outside_pending_scope() {
    assert_pending_manifest_refresh("REPO", true, false, false, true).await;
}

#[tokio::test]
async fn a_recheck_revalidates_a_saved_pending_addon_and_drops_its_stale_links() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_url = format!("http://{}/", listener.local_addr().unwrap());
    let unconditional_manifest_requests = Arc::new(AtomicU64::new(0));
    let conditional_manifest_requests = Arc::new(AtomicU64::new(0));
    let server = {
        let unconditional = unconditional_manifest_requests.clone();
        let conditional = conditional_manifest_requests.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let read = stream.read(&mut request).await.unwrap();
                let request = std::str::from_utf8(&request[..read]).unwrap();
                let (status, body) = if request.starts_with("GET /repo.json ") {
                    (
                        "200 OK",
                        r#"{"repoName":"MainRepo","checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"}]}"#,
                    )
                } else if request.starts_with("GET /@mod_01/mod.srf ") {
                    if request.to_ascii_lowercase().contains("if-none-match:") {
                        conditional.fetch_add(1, Ordering::Relaxed);
                        ("304 Not Modified", "")
                    } else {
                        unconditional.fetch_add(1, Ordering::Relaxed);
                        (
                            "200 OK",
                            r#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]}]}"#,
                        )
                    }
                } else {
                    ("404 Not Found", "")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        })
    };
    let directory = tempfile::tempdir().unwrap();
    let local_path = format!("{}/", directory.path().to_string_lossy().replace('\\', "/"));
    let addon_path = format!("{local_path}@mod_01");
    std::fs::create_dir_all(&addon_path).unwrap();
    std::fs::write(format!("{addon_path}/keep.bin"), b"data").unwrap();
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    seed_repository(
        &fdb,
        1,
        "MainRepo",
        &remote_url,
        &local_path,
        "REPO",
        "REPO",
        "CONTENT",
    )
    .await;
    seed_addon(
        &fdb,
        1,
        "@mod_01",
        &format!("{remote_url}@mod_01"),
        &addon_path,
        "ADDON",
        "ADDON",
        "CONTENT",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    for (id, name) in [(1, "keep.bin"), (2, "obsolete.bin")] {
        seed_file(
            &fdb,
            id,
            name,
            &format!("{remote_url}@mod_01/{name}"),
            &format!("{addon_path}/{name}"),
            "FILE",
            "FILE",
            "CONTENT",
            4,
            id - 1,
        )
        .await;
        seed_addon_file(&fdb, 1, id).await;
        seed_subfile(&fdb, id, id, "PART", "PART").await;
    }
    let cache = ManifestCache::in_space(directory.path());
    cache.store(
        &format!("{remote_url}@mod_01/mod.srf"),
        &Validators { etag: Some("\"current\"".to_string()), last_modified: None },
        br#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]}]}"#,
    ).unwrap();
    let context = FoxyContext::new(db, reqwest::Client::new())
        .with_manifest_cache(cache)
        .with_target_local_path(&local_path);
    let linked_files = || async {
        fdb.query_all(
            "SELECT file_id FROM addon_files WHERE addon_id = 1 ORDER BY file_id",
            params![],
        )
        .await
        .unwrap()
        .iter()
        .filter_map(|row| row.get_i64("file_id").ok())
        .collect::<Vec<_>>()
    };

    let unchanged = remote_repository(
        Arc::new(context.clone()),
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .unwrap();
    assert!(!unchanged.remote_graph_fetched);
    assert_eq!(linked_files().await, vec![1, 2]);

    let rechecked = remote_repository(
        Arc::new(context.with_reapplied_mod_manifests(HashSet::from(["@mod_01".to_string()]))),
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .unwrap();
    server.abort();
    assert!(rechecked.remote_graph_fetched);
    assert_eq!(linked_files().await, vec![1]);
    assert_eq!(conditional_manifest_requests.load(Ordering::Relaxed), 1);
    assert_eq!(unconditional_manifest_requests.load(Ordering::Relaxed), 0);
    assert_eq!(checksums(&fdb, "files", 1).await.0, "FILE");
}

async fn seed_one_addon_repository(
    fdb: &FoxyDb,
    remote_url: &str,
    local_path: &str,
    repository_remote_checksum: &str,
    addon_remote_checksum: &str,
) {
    let addon_path = format!("{local_path}@mod_01");
    std::fs::create_dir_all(&addon_path).unwrap();
    std::fs::write(format!("{addon_path}/keep.bin"), b"data").unwrap();
    seed_repository(
        fdb,
        1,
        "MainRepo",
        remote_url,
        local_path,
        "LOCAL",
        repository_remote_checksum,
        "CONTENT",
    )
    .await;
    seed_addon(
        fdb,
        1,
        "@mod_01",
        &format!("{remote_url}@mod_01"),
        &addon_path,
        "OLD",
        addon_remote_checksum,
        "CONTENT",
        true,
    )
    .await;
    seed_repository_addon(fdb, 1, 1).await;
    seed_file(
        fdb,
        1,
        "keep.bin",
        &format!("{remote_url}@mod_01/keep.bin"),
        &format!("{addon_path}/keep.bin"),
        "FILE",
        "FILE",
        "CONTENT",
        4,
        0,
    )
    .await;
    seed_addon_file(fdb, 1, 1).await;
    seed_subfile(fdb, 1, 1, "PART", "PART").await;
}

#[tokio::test]
async fn an_interrupted_refresh_keeps_the_previous_repository_and_addon_checksums() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_url = format!("http://{}/", listener.local_addr().unwrap());
    let directory = tempfile::tempdir().unwrap();
    let local_path = format!("{}/", directory.path().to_string_lossy().replace('\\', "/"));
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    seed_one_addon_repository(&fdb, &remote_url, &local_path, "OLDREPO", "OLD").await;
    let seen_addon_checksum = Arc::new(std::sync::Mutex::new(String::new()));
    let seen = seen_addon_checksum.clone();
    let reader = FoxyDb::from_turso(db.clone());
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            let (status, body) = if request.starts_with("GET /repo.json ") {
                (
                    "200 OK",
                    r#"{"repoName":"MainRepo","checksum":"NEWREPO","requiredMods":[{"modName":"@mod_01","checkSum":"NEW"}]}"#,
                )
            } else if request.starts_with("GET /@mod_01/mod.srf ") {
                let row = reader
                    .query_one("SELECT remote_checksum FROM addons WHERE id = 1", params![])
                    .await
                    .unwrap()
                    .unwrap();
                *seen.lock().unwrap() = row.get_string("remote_checksum").unwrap();
                ("500 Internal Server Error", "")
            } else {
                ("404 Not Found", "")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let context =
        Arc::new(FoxyContext::new(db, reqwest::Client::new()).with_target_local_path(&local_path));
    let metadata = remote_repository(
        context,
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await;
    server.abort();
    assert!(metadata.is_none());
    assert_eq!(
        seen_addon_checksum.lock().unwrap().as_str(),
        "OLD",
        "the addon upsert must not store the published checksum before the manifest is applied"
    );
    assert_eq!(checksums(&fdb, "addons", 1).await.1, "");
    let (_, stored_repository_checksum, _) = checksums(&fdb, "repositories", 1).await;
    assert_eq!(stored_repository_checksum, "OLDREPO");
    assert_eq!(
        super::super::quick_scan::remote_freshness("NEWREPO", &stored_repository_checksum, "LOCAL"),
        super::super::quick_scan::RemoteFreshness::Changed
    );
}

#[tokio::test]
async fn a_completed_refresh_stores_both_checksums_and_the_next_refresh_skips() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_url = format!("http://{}/", listener.local_addr().unwrap());
    let directory = tempfile::tempdir().unwrap();
    let local_path = format!("{}/", directory.path().to_string_lossy().replace('\\', "/"));
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    seed_one_addon_repository(&fdb, &remote_url, &local_path, "OLDREPO", "OLD").await;
    let manifest_requests = Arc::new(AtomicU64::new(0));
    let requests = manifest_requests.clone();
    let seen_addon_checksum = Arc::new(std::sync::Mutex::new(String::new()));
    let seen = seen_addon_checksum.clone();
    let reader = FoxyDb::from_turso(db.clone());
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let read = stream.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..read]).unwrap();
            let (status, body) = if request.starts_with("GET /repo.json ") {
                (
                    "200 OK",
                    r#"{"repoName":"MainRepo","checksum":"NEWREPO","requiredMods":[{"modName":"@mod_01","checkSum":"NEW"}]}"#,
                )
            } else if request.starts_with("GET /@mod_01/mod.srf ") {
                requests.fetch_add(1, Ordering::Relaxed);
                if seen.lock().unwrap().is_empty() {
                    let row = reader
                        .query_one("SELECT remote_checksum FROM addons WHERE id = 1", params![])
                        .await
                        .unwrap()
                        .unwrap();
                    *seen.lock().unwrap() = row.get_string("remote_checksum").unwrap();
                }
                (
                    "200 OK",
                    r#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]}]}"#,
                )
            } else {
                ("404 Not Found", "")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let context =
        Arc::new(FoxyContext::new(db, reqwest::Client::new()).with_target_local_path(&local_path));
    let metadata = remote_repository(
        context.clone(),
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .expect("completed refresh");
    assert!(metadata.remote_graph_fetched);
    assert_eq!(seen_addon_checksum.lock().unwrap().as_str(), "OLD");
    assert_eq!(checksums(&fdb, "addons", 1).await.1, "NEW");
    assert_eq!(checksums(&fdb, "repositories", 1).await.1, "NEWREPO");
    assert_eq!(manifest_requests.load(Ordering::Relaxed), 1);
    let again = remote_repository(
        context,
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .unwrap();
    server.abort();
    assert!(!again.remote_graph_fetched);
    assert_eq!(manifest_requests.load(Ordering::Relaxed), 1);
}

async fn linked_file_names(fdb: &FoxyDb, addon_id: i64) -> Vec<String> {
    fdb.query_all(
        "SELECT files.name AS name FROM files \
         JOIN addon_files ON addon_files.file_id = files.id \
         WHERE addon_files.addon_id = ? ORDER BY files.name",
        params![addon_id],
    )
    .await
    .expect("query linked files")
    .iter()
    .map(|row| row.get_string("name").expect("file name"))
    .collect()
}

fn http_response(status: &str, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}{extra_headers}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn a_startup_refresh_of_a_saved_pending_update_drops_the_stale_link() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_url = format!("http://{}/", listener.local_addr().unwrap());
    let unconditional_manifest_requests = Arc::new(AtomicU64::new(0));
    let conditional_manifest_requests = Arc::new(AtomicU64::new(0));
    let server = {
        let unconditional = unconditional_manifest_requests.clone();
        let conditional = conditional_manifest_requests.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let read = stream.read(&mut request).await.unwrap();
                let request = std::str::from_utf8(&request[..read]).unwrap();
                let response = if request.starts_with("GET /repo.json ") {
                    http_response(
                        "200 OK",
                        "",
                        r#"{"repoName":"MainRepo","checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"}]}"#,
                    )
                } else if request.starts_with("GET /@mod_01/mod.srf ") {
                    if request.to_ascii_lowercase().contains("if-none-match:") {
                        conditional.fetch_add(1, Ordering::Relaxed);
                        http_response("304 Not Modified", "", "")
                    } else {
                        unconditional.fetch_add(1, Ordering::Relaxed);
                        http_response(
                            "200 OK",
                            "",
                            r#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]}]}"#,
                        )
                    }
                } else {
                    http_response("404 Not Found", "", "")
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        })
    };
    let directory = tempfile::tempdir().unwrap();
    let local_path = format!("{}/", directory.path().to_string_lossy().replace('\\', "/"));
    let addon_path = format!("{local_path}@mod_01");
    std::fs::create_dir_all(&addon_path).unwrap();
    std::fs::write(format!("{addon_path}/keep.bin"), b"data").unwrap();
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    seed_repository(
        &fdb,
        1,
        "MainRepo",
        &remote_url,
        &local_path,
        "REPO",
        "REPO",
        "CONTENT",
    )
    .await;
    seed_addon(
        &fdb,
        1,
        "@mod_01",
        &format!("{remote_url}@mod_01"),
        &addon_path,
        "ADDON",
        "ADDON",
        "CONTENT",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    for (id, name) in [(1, "keep.bin"), (2, "obsolete.bin")] {
        seed_file(
            &fdb,
            id,
            name,
            &format!("{remote_url}@mod_01/{name}"),
            &format!("{addon_path}/{name}"),
            "FILE",
            "FILE",
            "CONTENT",
            4,
            id - 1,
        )
        .await;
        seed_addon_file(&fdb, 1, id).await;
        seed_subfile(&fdb, id, id, "PART", "PART").await;
    }
    let cache = ManifestCache::in_space(directory.path());
    cache
        .store(
            &format!("{remote_url}@mod_01/mod.srf"),
            &Validators { etag: Some("\"current\"".to_string()), last_modified: None },
            br#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]}]}"#,
        )
        .unwrap();
    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_manifest_cache(cache)
            .with_target_local_path(&local_path),
    );
    let payload = serde_json::to_string(&[ModDiffSummary {
        name: "@mod_01".to_string(),
        needs_update: true,
        total_bytes: 4,
        files: Vec::new(),
    }])
    .unwrap();
    save_pending_update_for_context(context.clone(), &remote_url, &payload)
        .await
        .unwrap();
    let pending_key = crate::core::utils::content_hash::normalize_path(&local_path);
    assert!(pending_update_exists(&fdb, &remote_url, &pending_key).await);
    let saved = fetch_pending_update_for_context(context.clone(), &remote_url)
        .await
        .unwrap()
        .expect("saved pending update");
    let saved_mods: Vec<ModDiffSummary> = serde_json::from_str(&saved).unwrap();
    let reapplied = recheck_reapplied_addons(&saved_mods);
    assert_eq!(reapplied, HashSet::from(["@mod_01".to_string()]));

    let refreshed = remote_repository(
        Arc::new(
            context
                .as_ref()
                .clone()
                .with_reapplied_mod_manifests(reapplied),
        ),
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .unwrap();
    assert!(refreshed.remote_graph_fetched);
    assert_eq!(
        linked_file_names(&fdb, 1).await,
        vec!["keep.bin".to_string()]
    );
    assert_eq!(conditional_manifest_requests.load(Ordering::Relaxed), 1);
    assert_eq!(unconditional_manifest_requests.load(Ordering::Relaxed), 0);
    clear_pending_update_for_context(context, &remote_url)
        .await
        .unwrap();
    server.abort();
    assert!(!pending_update_exists(&fdb, &remote_url, &pending_key).await);
}

#[tokio::test]
async fn a_startup_refresh_applies_a_later_manifest_to_the_pending_addon_and_the_changed_one() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_url = format!("http://{}/", listener.local_addr().unwrap());
    let addon_one_requests = Arc::new(AtomicU64::new(0));
    let addon_two_requests = Arc::new(AtomicU64::new(0));
    let server = {
        let addon_one_requests = addon_one_requests.clone();
        let addon_two_requests = addon_two_requests.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let read = stream.read(&mut request).await.unwrap();
                let request = std::str::from_utf8(&request[..read]).unwrap();
                let response = if request.starts_with("GET /repo.json ") {
                    http_response(
                        "200 OK",
                        "",
                        r#"{"repoName":"MainRepo","checksum":"NEWREPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"},{"modName":"@mod_02","checkSum":"ADDON2B"}]}"#,
                    )
                } else if request.starts_with("GET /@mod_01/mod.srf ") {
                    addon_one_requests.fetch_add(1, Ordering::Relaxed);
                    http_response(
                        "200 OK",
                        "\r\nETag: \"b\"",
                        r#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]}]}"#,
                    )
                } else if request.starts_with("GET /@mod_02/mod.srf ") {
                    addon_two_requests.fetch_add(1, Ordering::Relaxed);
                    http_response(
                        "200 OK",
                        "\r\nETag: \"b\"",
                        r#"{"files":[{"path":"fresh.bin","checksum":"FILE2","length":4,"parts":[{"path":"fresh.bin","checksum":"PART2","start":0,"length":4}]}]}"#,
                    )
                } else {
                    http_response("404 Not Found", "", "")
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        })
    };
    let directory = tempfile::tempdir().unwrap();
    let local_path = format!("{}/", directory.path().to_string_lossy().replace('\\', "/"));
    let addon_one = format!("{local_path}@mod_01");
    let addon_two = format!("{local_path}@mod_02");
    std::fs::create_dir_all(&addon_one).unwrap();
    std::fs::create_dir_all(&addon_two).unwrap();
    std::fs::write(format!("{addon_one}/keep.bin"), b"data").unwrap();
    std::fs::write(format!("{addon_two}/fresh.bin"), b"data").unwrap();
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    seed_repository(
        &fdb,
        1,
        "MainRepo",
        &remote_url,
        &local_path,
        "LOCAL",
        "OLDREPO",
        "CONTENT",
    )
    .await;
    seed_addon(
        &fdb,
        1,
        "@mod_01",
        &format!("{remote_url}@mod_01"),
        &addon_one,
        "ADDON",
        "ADDON",
        "CONTENT",
        true,
    )
    .await;
    seed_addon(
        &fdb,
        2,
        "@mod_02",
        &format!("{remote_url}@mod_02"),
        &addon_two,
        "ADDON2",
        "ADDON2",
        "CONTENT",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    seed_repository_addon(&fdb, 1, 2).await;
    for (id, addon_id, folder, name) in [
        (1_i64, 1_i64, &addon_one, "keep.bin"),
        (2, 1, &addon_one, "extra.bin"),
        (3, 2, &addon_two, "old.bin"),
    ] {
        seed_file(
            &fdb,
            id,
            name,
            &format!("{remote_url}@mod_0{addon_id}/{name}"),
            &format!("{folder}/{name}"),
            "FILE",
            "FILE",
            "CONTENT",
            4,
            id - 1,
        )
        .await;
        seed_addon_file(&fdb, addon_id, id).await;
        seed_subfile(&fdb, id, id, "PART", "PART").await;
    }
    let cache = ManifestCache::in_space(directory.path());
    cache
        .store(
            &format!("{remote_url}@mod_01/mod.srf"),
            &Validators { etag: Some("\"a\"".to_string()), last_modified: None },
            br#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]},{"path":"extra.bin","checksum":"FILE","length":4,"parts":[{"path":"extra.bin","checksum":"PART","start":0,"length":4}]}]}"#,
        )
        .unwrap();
    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_manifest_cache(cache)
            .with_target_local_path(&local_path),
    );
    let payload = serde_json::to_string(&[ModDiffSummary {
        name: "@mod_01".to_string(),
        needs_update: true,
        total_bytes: 4,
        files: Vec::new(),
    }])
    .unwrap();
    save_pending_update_for_context(context.clone(), &remote_url, &payload)
        .await
        .unwrap();
    let saved = fetch_pending_update_for_context(context.clone(), &remote_url)
        .await
        .unwrap()
        .unwrap();
    let reapplied =
        recheck_reapplied_addons(&serde_json::from_str::<Vec<ModDiffSummary>>(&saved).unwrap());
    assert_eq!(reapplied, HashSet::from(["@mod_01".to_string()]));
    let refreshed = remote_repository(
        Arc::new(
            context
                .as_ref()
                .clone()
                .with_reapplied_mod_manifests(reapplied),
        ),
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .expect("startup refresh applies both manifests");
    server.abort();
    assert!(refreshed.remote_graph_fetched);
    assert_eq!(addon_one_requests.load(Ordering::Relaxed), 1);
    assert_eq!(addon_two_requests.load(Ordering::Relaxed), 1);
    assert_eq!(
        linked_file_names(&fdb, 1).await,
        vec!["keep.bin".to_string()]
    );
    assert_eq!(
        linked_file_names(&fdb, 2).await,
        vec!["fresh.bin".to_string()]
    );
    assert_eq!(checksums(&fdb, "repositories", 1).await.1, "NEWREPO");
    assert_eq!(checksums(&fdb, "addons", 2).await.1, "ADDON2B");
}

#[tokio::test]
async fn a_startup_refresh_applies_a_republished_manifest_when_the_checksum_stays() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_url = format!("http://{}/", listener.local_addr().unwrap());
    let conditional_new_body = Arc::new(AtomicU64::new(0));
    let server = {
        let conditional_new_body = conditional_new_body.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let read = stream.read(&mut request).await.unwrap();
                let request = std::str::from_utf8(&request[..read]).unwrap();
                let response = if request.starts_with("GET /repo.json ") {
                    http_response(
                        "200 OK",
                        "",
                        r#"{"repoName":"MainRepo","checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"}]}"#,
                    )
                } else if request.starts_with("GET /@mod_01/mod.srf ") {
                    assert!(
                        request.to_ascii_lowercase().contains("if-none-match:"),
                        "the startup refresh revalidates the saved manifest"
                    );
                    conditional_new_body.fetch_add(1, Ordering::Relaxed);
                    http_response(
                        "200 OK",
                        "\r\nETag: \"republished\"",
                        r#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]}]}"#,
                    )
                } else {
                    http_response("404 Not Found", "", "")
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        })
    };
    let directory = tempfile::tempdir().unwrap();
    let local_path = format!("{}/", directory.path().to_string_lossy().replace('\\', "/"));
    let addon_path = format!("{local_path}@mod_01");
    std::fs::create_dir_all(&addon_path).unwrap();
    std::fs::write(format!("{addon_path}/keep.bin"), b"data").unwrap();
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());
    seed_repository(
        &fdb,
        1,
        "MainRepo",
        &remote_url,
        &local_path,
        "REPO",
        "REPO",
        "CONTENT",
    )
    .await;
    seed_addon(
        &fdb,
        1,
        "@mod_01",
        &format!("{remote_url}@mod_01"),
        &addon_path,
        "ADDON",
        "ADDON",
        "CONTENT",
        true,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    for (id, name) in [(1, "keep.bin"), (2, "obsolete.bin")] {
        seed_file(
            &fdb,
            id,
            name,
            &format!("{remote_url}@mod_01/{name}"),
            &format!("{addon_path}/{name}"),
            "FILE",
            "FILE",
            "CONTENT",
            4,
            id - 1,
        )
        .await;
        seed_addon_file(&fdb, 1, id).await;
        seed_subfile(&fdb, id, id, "PART", "PART").await;
    }
    let cache = ManifestCache::in_space(directory.path());
    cache
        .store(
            &format!("{remote_url}@mod_01/mod.srf"),
            &Validators { etag: Some("\"previous\"".to_string()), last_modified: None },
            br#"{"files":[{"path":"keep.bin","checksum":"FILE","length":4,"parts":[{"path":"keep.bin","checksum":"PART","start":0,"length":4}]},{"path":"obsolete.bin","checksum":"FILE","length":4,"parts":[{"path":"obsolete.bin","checksum":"PART","start":0,"length":4}]}]}"#,
        )
        .unwrap();
    let context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_manifest_cache(cache)
            .with_target_local_path(&local_path),
    );
    let payload = serde_json::to_string(&[ModDiffSummary {
        name: "@mod_01".to_string(),
        needs_update: true,
        total_bytes: 4,
        files: Vec::new(),
    }])
    .unwrap();
    save_pending_update_for_context(context.clone(), &remote_url, &payload)
        .await
        .unwrap();
    let saved = fetch_pending_update_for_context(context.clone(), &remote_url)
        .await
        .unwrap()
        .unwrap();
    let reapplied =
        recheck_reapplied_addons(&serde_json::from_str::<Vec<ModDiffSummary>>(&saved).unwrap());
    let refreshed = remote_repository(
        Arc::new(
            context
                .as_ref()
                .clone()
                .with_reapplied_mod_manifests(reapplied),
        ),
        &remote_url,
        Some(&local_path),
        None,
        false,
        HashAlgorithmPreference::PreferSwifty,
    )
    .await
    .expect("startup refresh applies the republished manifest");
    server.abort();
    assert!(refreshed.remote_graph_fetched);
    assert_eq!(conditional_new_body.load(Ordering::Relaxed), 1);
    assert_eq!(
        linked_file_names(&fdb, 1).await,
        vec!["keep.bin".to_string()]
    );
    assert_eq!(checksums(&fdb, "addons", 1).await.1, "ADDON");
    assert_eq!(checksums(&fdb, "repositories", 1).await.1, "REPO");
}
