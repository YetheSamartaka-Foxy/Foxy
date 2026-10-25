use super::*;
use crate::core::models::pending_update::save_pending_update_for_context;
use crate::core::tasks::db_turso::build_test_database;
use crate::ui::types::HashAlgorithmPreference;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
struct IndexState {
    repo_json: String,
    foxy_index: String,
    foxy_index_status: u16,
    cache_control_values: Vec<String>,
}

struct PreparedQueueFixture {
    context: Arc<FoxyContext>,
    repository_url: String,
    state: Arc<Mutex<IndexState>>,
    server: tokio::task::JoinHandle<()>,
}

impl PreparedQueueFixture {
    async fn new(foxy_mode: bool, with_other_folder: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let repository_url = format!("http://{}/", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(IndexState {
            repo_json: if foxy_mode {
                r#"{"repoName":"MainRepo","checksum":"REPO_LEGACY","foxyMode":"FoxyModeV1","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"}],"optionalMods":[]}"#.to_string()
            } else {
                r#"{"repoName":"MainRepo","checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"}],"optionalMods":[]}"#.to_string()
            },
            foxy_index: r#"{"checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON"}],"optionalMods":[]}"#.to_string(),
            foxy_index_status: 200,
            cache_control_values: Vec::new(),
        }));
        let server = {
            let state = state.clone();
            tokio::spawn(async move {
                loop {
                    let (mut stream, _) = match listener.accept().await {
                        Ok(connection) => connection,
                        Err(_) => break,
                    };
                    let mut request = vec![0; 4096];
                    let length = stream.read(&mut request).await.unwrap_or_default();
                    let request = String::from_utf8_lossy(&request[..length]);
                    let path = request.split_whitespace().nth(1).unwrap_or_default();
                    let (status, body) = {
                        let mut state = state.lock().unwrap();
                        if path == "/repo.json" {
                            state.cache_control_values.push(
                                request
                                    .lines()
                                    .find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        name.eq_ignore_ascii_case("cache-control")
                                            .then(|| value.trim().to_ascii_lowercase())
                                    })
                                    .unwrap_or_default(),
                            );
                            (200, state.repo_json.clone())
                        } else if path == "/foxy_addons.json" {
                            state.cache_control_values.push(
                                request
                                    .lines()
                                    .find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        name.eq_ignore_ascii_case("cache-control")
                                            .then(|| value.trim().to_ascii_lowercase())
                                    })
                                    .unwrap_or_default(),
                            );
                            (state.foxy_index_status, state.foxy_index.clone())
                        } else if path == "/@mod_01/mod.srf" {
                            (200, r#"{"files":[{"path":"file.bin","checksum":"FILE","length":4,"parts":[{"path":"file.bin","checksum":"PART","start":0,"length":4}]}]}"#.to_string())
                        } else {
                            (404, String::new())
                        }
                    };
                    let response = format!(
                        "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            })
        };

        let database = build_test_database().await;
        let local_path = "R:/Mods/MainRepo/";
        let context = Arc::new(
            FoxyContext::new(database, reqwest::Client::new())
                .with_target_local_path(local_path.to_string()),
        );
        let db = context.db();
        let foxy_mode = if foxy_mode { "FoxyModeV1" } else { "" };
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path, remote_checksum, local_checksum, foxy_mode) \
             VALUES (1, 'MainRepo', ?, ?, 'REPO', 'LOCAL', ?)",
            params![repository_url.clone(), local_path, foxy_mode],
        )
        .await
        .unwrap();
        Self::seed_addon(&db, 1, 1, local_path, &repository_url, "ADDON").await;
        if with_other_folder {
            let other_local_path = "R:/Mods/OtherRepo/";
            db.execute(
                "INSERT INTO repositories (id, name, remote_url, local_path, remote_checksum, local_checksum, foxy_mode) \
                 VALUES (2, 'OtherRepo', ?, ?, 'REPO', 'LOCAL', ?)",
                params![repository_url.clone(), other_local_path, foxy_mode],
            )
            .await
            .unwrap();
            Self::seed_addon(&db, 2, 2, other_local_path, &repository_url, "OTHER_ADDON").await;
        }
        let payload = serde_json::to_string(&[crate::core::api::ModDiffSummary {
            name: "@mod_01".to_string(),
            needs_update: true,
            total_bytes: 4,
            files: vec![crate::core::api::FileDiffSummary {
                name: "file.bin".to_string(),
                needs_update: true,
                total_bytes: 4,
                changed_parts: 1,
                change_kind: crate::core::api::FileDiffKind::Modified,
            }],
        }])
        .unwrap();
        save_pending_update_for_context(context.clone(), &repository_url, &payload)
            .await
            .unwrap();

        Self {
            context,
            repository_url,
            state,
            server,
        }
    }

    async fn seed_addon(
        db: &crate::core::db::FoxyDb,
        repository_id: i64,
        id: i64,
        local_path: &str,
        repository_url: &str,
        addon_checksum: &str,
    ) {
        let addon_local_path = format!("{local_path}@mod_01");
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, enabled, required, remote_checksum, local_checksum, data_order) \
             VALUES (?, '@mod_01', ?, ?, 1, 1, ?, 'OLD_ADDON', 0)",
            params![
                id,
                format!("{repository_url}@mod_01"),
                addon_local_path.as_str(),
                addon_checksum
            ],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (?, ?)",
            params![repository_id, id],
        )
        .await
        .unwrap();
        let file_local_path = format!("{addon_local_path}/file.bin");
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path, remote_checksum, local_checksum, length, data_order) \
             VALUES (?, 'file.bin', ?, ?, 'FILE', '', 4, 0)",
            params![id, format!("{repository_url}@mod_01/file.bin"), file_local_path.as_str()],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
            params![id, id],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO subfiles (id, file_id, path, remote_checksum, local_checksum, remote_start, remote_length, local_start, local_length, data_order) \
             VALUES (?, ?, 'file.bin', 'PART', '', 0, 4, 0, 0, 0)",
            params![id, id],
        )
        .await
        .unwrap();
        if repository_id == 1 {
            db.execute(
                "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size) \
                 VALUES (?, ?, ?, 4)",
                params![
                    id,
                    format!("{repository_url}@mod_01/file.bin"),
                    file_local_path
                ],
            )
            .await
            .unwrap();
        }
    }

    async fn can_reuse(&self, preference: HashAlgorithmPreference) -> bool {
        can_reuse_prepared_download_queue(self.context.clone(), &self.repository_url, preference)
            .await
    }

    fn set_repo_json(&self, body: String) {
        self.state.lock().unwrap().repo_json = body;
    }

    fn set_foxy_index(&self, body: String, status: u16) {
        let mut state = self.state.lock().unwrap();
        state.foxy_index = body;
        state.foxy_index_status = status;
    }

    fn assert_fresh_index_reads(&self) {
        let values = &self.state.lock().unwrap().cache_control_values;
        assert!(!values.is_empty());
        assert!(values.iter().all(|value| value == "no-cache"));
    }
}

impl Drop for PreparedQueueFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn legacy_repo_json(addons: &str) -> String {
    format!(
        "{{\"repoName\":\"MainRepo\",\"checksum\":\"REPO\",\"requiredMods\":[{addons}],\"optionalMods\":[]}}"
    )
}

#[tokio::test]
async fn prepared_queue_reuse_rejects_changed_addon_list_with_same_repository_checksum() {
    let fixture = PreparedQueueFixture::new(false, false).await;
    assert!(
        fixture
            .can_reuse(HashAlgorithmPreference::PreferSwifty)
            .await
    );

    for changed_addons in [
        r#"{"modName":"@mod_01","checkSum":"ADDON_NEW"}"#,
        r#"{"modName":"@mod_01","checkSum":"ADDON"},{"modName":"@mod_02","checkSum":"OTHER_ADDON"}"#,
        "",
        r#"{"modName":"@renamed","checkSum":"ADDON"}"#,
        r#"{"modName":"@mod_01"}"#,
        r#"{"modName":"@mod_01","checkSum":"ADDON","clientSide":true}"#,
    ] {
        fixture.set_repo_json(legacy_repo_json(changed_addons));
        assert!(
            !fixture
                .can_reuse(HashAlgorithmPreference::PreferSwifty)
                .await
        );
        fixture.set_repo_json(legacy_repo_json(
            r#"{"modName":"@mod_01","checkSum":"ADDON"}"#,
        ));
    }
    fixture.set_repo_json(
        r#"{"checksum":"REPO","requiredMods":[],"optionalMods":[{"modName":"@mod_01","checkSum":"ADDON"}]}"#.to_string(),
    );
    assert!(
        !fixture
            .can_reuse(HashAlgorithmPreference::PreferSwifty)
            .await
    );
    fixture.assert_fresh_index_reads();
}

#[tokio::test]
async fn changed_addon_requirement_rebuilds_metadata_once_with_same_repository_checksum() {
    let fixture = PreparedQueueFixture::new(false, false).await;
    fixture.set_repo_json(
        r#"{"repoName":"MainRepo","checksum":"REPO","requiredMods":[],"optionalMods":[{"modName":"@mod_01","checkSum":"ADDON"}]}"#.to_string(),
    );
    let context = Arc::new(
        fixture
            .context
            .as_ref()
            .clone()
            .with_download_target_queueing(false),
    );
    for expected_fetch in [true, false] {
        let metadata = remote_repository(
            context.clone(),
            &fixture.repository_url,
            Some("R:/Mods/MainRepo/"),
            None,
            false,
            HashAlgorithmPreference::PreferSwifty,
        )
        .await
        .unwrap();
        assert_eq!(metadata.remote_graph_fetched, expected_fetch);
        let row = context
            .db()
            .query_one("SELECT required FROM addons WHERE id = 1", params![])
            .await
            .unwrap()
            .unwrap();
        assert!(!row.get_bool("required").unwrap());
    }
    fixture.assert_fresh_index_reads();
}

#[tokio::test]
async fn prepared_queue_reuse_scopes_addon_checksums_to_the_current_folder() {
    let fixture = PreparedQueueFixture::new(false, true).await;
    assert!(
        fixture
            .can_reuse(HashAlgorithmPreference::PreferSwifty)
            .await
    );
    fixture.assert_fresh_index_reads();
}

#[tokio::test]
async fn prepared_queue_reuse_uses_fresh_foxy_addon_index() {
    let fixture = PreparedQueueFixture::new(true, false).await;
    assert!(fixture.can_reuse(HashAlgorithmPreference::PreferFoxy).await);
    fixture.set_foxy_index(
        r#"{"checksum":"REPO","requiredMods":[{"modName":"@mod_01","checkSum":"ADDON_NEW"}],"optionalMods":[]}"#.to_string(),
        200,
    );
    assert!(!fixture.can_reuse(HashAlgorithmPreference::PreferFoxy).await);
    fixture.assert_fresh_index_reads();
}

#[tokio::test]
async fn remote_refresh_scope_includes_new_pending_addons_only_in_the_current_instance() {
    let fixture = PreparedQueueFixture::new(false, true).await;
    let db = fixture.context.db();
    db.execute(
        "INSERT INTO addons (id, name, remote_path, local_path, required, enabled, remote_checksum, local_checksum) \
         VALUES (3, '@mod_02', ?, 'R:/Mods/MainRepo/@mod_02', 1, 1, 'ADDON_NEW', 'ADDON_OLD')",
        params![format!("{}@mod_02", fixture.repository_url)],
    ).await.unwrap();
    db.execute(
        "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 3)",
        params![],
    )
    .await
    .unwrap();
    db.execute("UPDATE addons SET enabled = 0 WHERE id = 1", params![])
        .await
        .unwrap();

    let scope = pending_remote_addon_scope(fixture.context.clone(), &fixture.repository_url)
        .await
        .unwrap();
    assert_eq!(scope, HashSet::from(["@mod_02".to_string()]));
    db.execute(
        "UPDATE addons SET local_checksum = remote_checksum WHERE id = 3",
        params![],
    )
    .await
    .unwrap();
    assert!(
        pending_remote_addon_scope(fixture.context.clone(), &fixture.repository_url)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn prepared_queue_reuse_rejects_missing_or_unavailable_foxy_addon_index() {
    let fixture = PreparedQueueFixture::new(true, false).await;
    fixture.set_foxy_index(
        r#"{"checksum":"REPO","requiredMods":[{"modName":"@mod_01"}],"optionalMods":[]}"#
            .to_string(),
        200,
    );
    assert!(!fixture.can_reuse(HashAlgorithmPreference::PreferFoxy).await);
    fixture.set_foxy_index(String::new(), 503);
    assert!(!fixture.can_reuse(HashAlgorithmPreference::PreferFoxy).await);
    fixture.assert_fresh_index_reads();
}
