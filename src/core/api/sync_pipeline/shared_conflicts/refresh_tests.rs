use super::*;
use crate::core::db::params;
use crate::core::tasks::db_turso::build_test_database;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct SharedVersionFixture {
    context: Arc<FoxyContext>,
    sibling_url: String,
    manifest_requests: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for SharedVersionFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl SharedVersionFixture {
    async fn new(published_file_checksum: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sibling_url = format!("http://{}/", listener.local_addr().unwrap());
        let manifest = serde_json::json!({
            "checksum": "ADDON",
            "files": [{
                "path": "data.bin", "checksum": published_file_checksum, "length": 4,
                "parts": [{"path": "data.bin", "checksum": "PART", "start": 0, "length": 4}]
            }]
        })
        .to_string();
        let manifest_requests = Arc::new(AtomicUsize::new(0));
        let requests = manifest_requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let length = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request.split_whitespace().nth(1).unwrap_or_default();
                let body = match path {
                    "/repo.json" => {
                        r#"{"repoName":"RepoAlpha","checksum":"REPO","foxyMode":"FoxyModeV1"}"#
                    }
                    "/foxy_addons.json" => {
                        r#"{"checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON","enabled":true}],"optionalMods":[{"modName":"@mod_02","checkSum":"OPTIONAL","enabled":true}]}"#
                    }
                    "/@mod_01/foxy_addon.json" => {
                        requests.fetch_add(1, Ordering::Relaxed);
                        &manifest
                    }
                    _ => panic!("Unexpected fixture request: {path}"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let database = build_test_database().await;
        let context = Arc::new(
            FoxyContext::new(database, reqwest::Client::new())
                .with_target_local_path("R:/Mods/Shared"),
        );
        let db = context.db();
        db.execute(
            "INSERT INTO repositories \
             (id,name,remote_url,local_path,local_checksum,remote_checksum,local_content_hash,foxy_mode) VALUES \
             (1,'MainRepo','http://repo.example.invalid:8080/','R:/Mods/Shared/','SOURCE','SOURCE','source-content','FoxyModeV1'), \
             (2,'RepoAlpha',?,'R:/Mods/Shared/','REPO','REPO','sibling-content','FoxyModeV1')",
            params![sibling_url.clone()],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons \
             (id,name,remote_path,local_path,enabled,required,local_checksum,remote_checksum,local_content_hash,data_order) VALUES \
             (1,'@mod_01','http://repo.example.invalid:8080/@mod_01','R:/Mods/Shared/@mod_01',1,1,'OLD_ADDON','ADDON','source-content',0), \
             (2,'@mod_01',?,'R:/Mods/Shared/@mod_01',1,1,'ADDON','ADDON','sibling-content',0), \
             (3,'@mod_02',?,'R:/Mods/Shared/@mod_02',0,0,'OPTIONAL','OPTIONAL','optional-content',0)",
            params![format!("{sibling_url}@mod_01"), format!("{sibling_url}@mod_02")],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id,addon_id) VALUES (1,1),(2,2),(2,3)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files \
             (id,name,remote_path,local_path,local_checksum,remote_checksum,length,data_order) VALUES \
             (10,'data.bin','http://repo.example.invalid:8080/@mod_01/data.bin','R:/Mods/Shared/@mod_01/data.bin','OLD_FILE','NEW_FILE',4,0), \
             (20,'data.bin',?,'R:/Mods/Shared/@mod_01/data.bin','STALE_FILE','STALE_FILE',4,0)",
            params![format!("{sibling_url}@mod_01/data.bin")],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id,file_id) VALUES (1,10),(2,20)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO subfiles \
             (file_id,path,local_length,local_start,remote_length,remote_start,local_checksum,remote_checksum,data_order) VALUES \
             (10,'data.bin#0',4,0,4,0,'OLD_PART','PART',0), \
             (20,'data.bin#0',4,0,4,0,'STALE_PART','STALE_PART',0)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO download_target_file (file_id,download_remote_url,download_local_path,size) \
             VALUES (10,'http://repo.example.invalid:8080/@mod_01/data.bin','R:/Mods/Shared/@mod_01/data.bin',4)",
            params![],
        )
        .await
        .unwrap();
        Self {
            context,
            sibling_url,
            manifest_requests,
            server,
        }
    }

    async fn check(&self) -> SharedVersionConflicts {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            check_shared_version_conflicts_with_cleanup_paths(
                self.context.clone(),
                "http://repo.example.invalid:8080/",
                &[(self.sibling_url.clone(), "R:/Mods/Shared".to_owned())],
                &[],
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }
}

#[tokio::test]
async fn stale_shared_file_metadata_is_refreshed_before_blocking_an_update() {
    let fixture = SharedVersionFixture::new("NEW_FILE").await;
    assert_eq!(fixture.check().await, SharedVersionConflicts::default());
    let requests = fixture.manifest_requests.load(Ordering::Relaxed);
    assert!(requests > 0);
    let db = fixture.context.db();
    let sibling = db
        .query_one("SELECT remote_checksum FROM files WHERE id = 20", params![])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sibling.get_string("remote_checksum").unwrap(), "NEW_FILE");
    let optional = db
        .query_one("SELECT enabled FROM addons WHERE id = 3", params![])
        .await
        .unwrap()
        .unwrap();
    assert!(!optional.get_bool("enabled").unwrap());
    assert_eq!(
        source_download_expectations(&db, 1).await.unwrap(),
        vec![("R:/Mods/Shared/@mod_01/data.bin".into(), "NEW_FILE".into())]
    );
    assert_eq!(fixture.check().await, SharedVersionConflicts::default());
    assert_eq!(fixture.manifest_requests.load(Ordering::Relaxed), requests);
}

#[tokio::test]
async fn genuinely_incompatible_shared_files_still_block_after_manifest_refresh() {
    let fixture = SharedVersionFixture::new("OTHER_FILE").await;
    fixture.context.set_defer_part_inserts(true);
    fixture.context.set_fresh_subfiles_load(true);
    fixture
        .context
        .buffer_deferred_parts(vec![crate::core::models::context::DeferredPartInsert {
            file_id: 10,
            path: "data.bin#0".into(),
            remote_length: 4,
            remote_start: 0,
            remote_checksum: "PART".into(),
            data_order: 0,
        }]);
    let conflicts = fixture.check().await;
    assert_eq!(conflicts.blocking, vec!["RepoAlpha (#2)"]);
    assert!(conflicts.advisory.is_empty());
    assert!(fixture.manifest_requests.load(Ordering::Relaxed) > 0);
    assert_eq!(fixture.context.deferred_part_count(), 1);
    assert!(fixture.context.is_fresh_subfiles_load());
    let sibling_part = fixture
        .context
        .db()
        .query_one(
            "SELECT remote_checksum FROM subfiles WHERE file_id = 20",
            params![],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sibling_part.get_string("remote_checksum").unwrap(), "PART");
}

async fn seed_and_check_cleanup_overlap(
    sibling_repo_checksum: &str,
    sibling_addon_checksum: &str,
    published_addon_checksum: &str,
    published_files_json: &str,
    candidate_name: &str,
) -> Vec<i64> {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Shared");
    let addon_dir = root.join("@mod_01");
    std::fs::create_dir_all(&addon_dir).unwrap();
    let kept = addon_dir.join("kept.pbo");
    let candidate = addon_dir.join(candidate_name);
    std::fs::write(&kept, b"keep").unwrap();
    std::fs::write(&candidate, b"payload").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sibling_url = format!("http://{}/", listener.local_addr().unwrap());
    let repo_addons_body = serde_json::json!({
        "checksum": "REPO_NEW",
        "requiredMods": [{"modName": "@mod_01", "checkSum": published_addon_checksum, "enabled": true}],
        "optionalMods": []
    })
    .to_string();
    let addon_body = published_files_json.to_string();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let length = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let path = request.split_whitespace().nth(1).unwrap_or_default();
            let body = match path {
                "/repo.json" => {
                    r#"{"repoName":"RepoAlpha","checksum":"REPO_NEW","foxyMode":"FoxyModeV1"}"#
                }
                "/foxy_addons.json" => &repo_addons_body,
                "/@mod_01/foxy_addon.json" => &addon_body,
                _ => panic!("Unexpected fixture request: {path}"),
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let database = build_test_database().await;
    let root_str = root.to_string_lossy().replace('\\', "/");
    let addon_str = addon_dir.to_string_lossy().replace('\\', "/");
    let kept_str = kept.to_string_lossy().replace('\\', "/");
    let candidate_str = candidate.to_string_lossy().replace('\\', "/");
    let context = Arc::new(
        FoxyContext::new(database, reqwest::Client::new()).with_target_local_path(&root_str),
    );
    let db = context.db();
    db.execute(
        "INSERT INTO repositories \
         (id,name,remote_url,local_path,local_checksum,remote_checksum,local_content_hash,foxy_mode) VALUES \
         (1,'MainRepo','http://repo.example.invalid:8080/',?,'SOURCE_OLD','SOURCE_NEW','source-content','FoxyModeV1'), \
         (2,'RepoAlpha',?,?,'REPO_OLD',?,'sibling-content','FoxyModeV1')",
        params![
            format!("{root_str}/"),
            sibling_url.clone(),
            format!("{root_str}/"),
            sibling_repo_checksum
        ],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO addons \
         (id,name,remote_path,local_path,enabled,required,local_checksum,remote_checksum,local_content_hash,data_order) VALUES \
         (1,'@mod_01','http://repo.example.invalid:8080/@mod_01',?,1,1,'ADDON_OLD','ADDON_NEW','source-content',0), \
         (2,'@mod_01',?,?,1,1,?,?,'sibling-content',0)",
        params![
            addon_str.clone(),
            format!("{sibling_url}@mod_01"),
            addon_str,
            sibling_addon_checksum,
            sibling_addon_checksum
        ],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO repository_addons (repository_id,addon_id) VALUES (1,1),(2,2)",
        params![],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO files \
         (id,name,remote_path,local_path,local_checksum,remote_checksum,length,data_order) VALUES \
         (10,'kept.pbo','http://repo.example.invalid:8080/@mod_01/kept.pbo',?,'KEPT','KEPT',4,0), \
         (20,'kept.pbo',?,?,'KEPT','KEPT',4,0), \
         (21,?,?,?,'OBSOLETE','OBSOLETE',7,1)",
        params![
            kept_str.clone(),
            format!("{sibling_url}@mod_01/kept.pbo"),
            kept_str,
            candidate_name,
            format!("{sibling_url}@mod_01/{candidate_name}"),
            candidate_str.clone()
        ],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO addon_files (addon_id,file_id) VALUES (1,10),(2,20),(2,21)",
        params![],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO subfiles (file_id,path,local_length,local_start,remote_length,remote_start,local_checksum,remote_checksum,data_order) VALUES \
         (10,'kept.pbo#0',4,0,4,0,'KEPT_PART','KEPT_PART',0), \
         (20,'kept.pbo#0',4,0,4,0,'KEPT_PART','KEPT_PART',0), \
         (21,?,7,0,7,0,'OBSOLETE_PART','OBSOLETE_PART',0)",
        params![format!("{candidate_name}#0")],
    )
    .await
    .unwrap();

    let conflicts = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        check_shared_version_conflicts_with_cleanup_paths(
            context.clone(),
            "http://repo.example.invalid:8080/",
            &[(sibling_url, format!("{root_str}/"))],
            std::slice::from_ref(&candidate_str),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    server.abort();
    assert_eq!(conflicts, SharedVersionConflicts::default());

    db.query_all(
        "SELECT file_id FROM addon_files WHERE addon_id = 2 AND file_id = 21",
        params![],
    )
    .await
    .unwrap()
    .into_iter()
    .filter_map(|row| row.get_i64("file_id").ok())
    .collect()
}

#[tokio::test]
async fn cleanup_candidate_refreshes_stale_sibling_on_deletion_only_update() {
    let remaining = seed_and_check_cleanup_overlap(
        "REPO_OLD",
        "ADDON_OLD",
        "ADDON_NEW",
        r#"{"checksum":"ADDON_NEW","files":[{"path":"kept.pbo","checksum":"KEPT","length":4,"parts":[{"path":"kept.pbo","checksum":"KEPT_PART","start":0,"length":4}]}]}"#,
        "obsolete.pbo",
    )
    .await;
    assert!(
        remaining.is_empty(),
        "sibling refresh should prune removed file link before cleanup runs"
    );
}

#[tokio::test]
async fn cleanup_candidate_heals_sibling_holding_unpruned_links_at_matching_addon_checksum() {
    let remaining = seed_and_check_cleanup_overlap(
        "REPO_NEW",
        "ADDON_NEW",
        "ADDON_NEW",
        r#"{"checksum":"ADDON_NEW","files":[{"path":"kept.pbo","checksum":"KEPT","length":4,"parts":[{"path":"kept.pbo","checksum":"KEPT_PART","start":0,"length":4}]}]}"#,
        "obsolete.pbo",
    )
    .await;
    assert!(
        remaining.is_empty(),
        "sibling holding unpruned links at current addon checksum should be pruned"
    );
}

#[tokio::test]
async fn cleanup_candidate_keeps_protection_when_refreshed_sibling_still_publishes_file() {
    let remaining = seed_and_check_cleanup_overlap(
        "REPO_OLD",
        "ADDON_SIBLING",
        "ADDON_SIBLING",
        r#"{"checksum":"ADDON_SIBLING","files":[{"path":"kept.pbo","checksum":"KEPT","length":4,"parts":[{"path":"kept.pbo","checksum":"KEPT_PART","start":0,"length":4}]},{"path":"sibling_only.pbo","checksum":"STAY","length":7,"parts":[{"path":"sibling_only.pbo","checksum":"STAY_PART","start":0,"length":7}]}]}"#,
        "sibling_only.pbo",
    )
    .await;
    assert_eq!(
        remaining,
        vec![21],
        "refreshed sibling that still publishes the file must retain ownership"
    );
}
