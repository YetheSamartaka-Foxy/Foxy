use super::*;
use crate::core::api::quick_scan::{
    collect_unexpected_files_for_repo_mods, delete_unexpected_local_files,
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
