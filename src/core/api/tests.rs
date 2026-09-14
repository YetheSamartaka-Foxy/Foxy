use super::quick_scan::{
    StartupQuickScanEligibility, batch_eligible_repos, content_hash_baseline_ready_joined,
    launch_quick_scan_repo_eligible_joined, launch_quick_scan_repo_startup_eligibility,
    quick_local_change_diff, refresh_content_hashes_for_repository,
    remote_checksum_state_ready_joined,
};
use super::*;
use crate::core::db::{FoxyDb, params};
use crate::core::tasks::calculate_hashes::propagate_checksums_to_siblings;
use std::collections::HashSet;

/// Build a fresh Turso test database (full bootstrap schema) for fixtures.
async fn build_db() -> std::sync::Arc<turso::Database> {
    crate::core::tasks::db_turso::build_test_database().await
}

// ---------------------------------------------------------------------------
// Raw-SQL fixture seeding (the seam's typed inserts; replaces SeaORM ActiveModels)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn seed_repository(
    fdb: &FoxyDb,
    id: i64,
    name: &str,
    remote_url: &str,
    local_path: &str,
    local_checksum: &str,
    remote_checksum: &str,
    local_content_hash: &str,
) {
    fdb.execute(
        "INSERT INTO repositories \
         (id, name, remote_url, local_path, image, local_checksum, remote_checksum, local_content_hash, foxy_mode) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![id, name, remote_url, local_path, "", local_checksum, remote_checksum, local_content_hash, ""],
    )
    .await
    .expect("seed repository");
}

#[allow(clippy::too_many_arguments)]
async fn seed_addon(
    fdb: &FoxyDb,
    id: i64,
    name: &str,
    remote_path: &str,
    local_path: &str,
    local_checksum: &str,
    remote_checksum: &str,
    local_content_hash: &str,
    required: bool,
) {
    fdb.execute(
        "INSERT INTO addons \
         (id, name, display_name, remote_path, local_path, client_side, enabled, \
          local_checksum, remote_checksum, local_content_hash, required, data_order) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            name,
            "",
            remote_path,
            local_path,
            false,
            true,
            local_checksum,
            remote_checksum,
            local_content_hash,
            required,
            0i64
        ],
    )
    .await
    .expect("seed addon");
}

#[allow(clippy::too_many_arguments)]
async fn seed_file(
    fdb: &FoxyDb,
    id: i64,
    name: &str,
    remote_path: &str,
    local_path: &str,
    local_checksum: &str,
    remote_checksum: &str,
    local_content_hash: &str,
    length: i64,
    data_order: i64,
) {
    fdb.execute(
        "INSERT INTO files \
         (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash, length, data_order) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            name,
            remote_path,
            local_path,
            local_checksum,
            remote_checksum,
            local_content_hash,
            length,
            data_order
        ],
    )
    .await
    .expect("seed file");
}

async fn seed_repository_addon(fdb: &FoxyDb, repository_id: i64, addon_id: i64) {
    fdb.execute(
        "INSERT INTO repository_addons (repository_id, addon_id) VALUES (?, ?)",
        params![repository_id, addon_id],
    )
    .await
    .expect("seed repository/addon link");
}

async fn seed_addon_file(fdb: &FoxyDb, addon_id: i64, file_id: i64) {
    fdb.execute(
        "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
        params![addon_id, file_id],
    )
    .await
    .expect("seed addon/file link");
}

async fn seed_subfile(
    fdb: &FoxyDb,
    id: i64,
    file_id: i64,
    local_checksum: &str,
    remote_checksum: &str,
) {
    fdb.execute(
        "INSERT INTO subfiles \
         (id, file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) \
         VALUES (?, ?, '', 0, 0, 1024, 0, ?, ?, 0)",
        params![id, file_id, local_checksum, remote_checksum],
    )
    .await
    .expect("seed subfile");
}

/// Read `(local_checksum, remote_checksum, local_content_hash)` for a row by id.
async fn checksums(fdb: &FoxyDb, table: &str, id: i64) -> (String, String, String) {
    let row = fdb
        .query_one(
            &format!(
                "SELECT local_checksum, remote_checksum, local_content_hash FROM {table} WHERE id = ?"
            ),
            params![id],
        )
        .await
        .expect("query checksums")
        .expect("row present");
    (
        row.get_string("local_checksum").unwrap(),
        row.get_string("remote_checksum").unwrap(),
        row.get_string("local_content_hash").unwrap(),
    )
}

async fn pending_update_exists(fdb: &FoxyDb, repository_url: &str, local_path: &str) -> bool {
    fdb.query_one(
        "SELECT 1 AS present FROM pending_updates WHERE repository_url = ? AND local_path = ?",
        params![repository_url, local_path],
    )
    .await
    .expect("query pending update")
    .is_some()
}

#[tokio::test]
async fn quick_scan_separates_same_url_instances_by_local_path() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let repo_url = "https://example.invalid/same-repo/";
    let shared_root = temp.path().join("a-shared");
    let target_root = temp.path().join("z-standalone");
    let shared_addon = shared_root.join("@addon");
    let target_addon = target_root.join("@addon");
    let shared_file = shared_addon.join("data.pbo");
    let target_file = target_addon.join("data.pbo");
    std::fs::create_dir_all(&shared_addon).expect("create shared addon");
    std::fs::create_dir_all(&target_root).expect("create empty standalone root");
    std::fs::write(&shared_file, b"existing repository file").expect("write shared file");
    let file_length = std::fs::metadata(&shared_file)
        .expect("shared file metadata")
        .len() as i64;

    seed_repository(
        &fdb,
        1,
        "Shared instance",
        repo_url,
        &shared_root.to_string_lossy(),
        "REPO_HASH",
        "REPO_HASH",
        "",
    )
    .await;
    seed_repository(
        &fdb,
        2,
        "Standalone instance",
        repo_url,
        &target_root.to_string_lossy(),
        "REPO_HASH",
        "REPO_HASH",
        "",
    )
    .await;

    seed_addon(
        &fdb,
        11,
        "@addon",
        "https://example.invalid/same-repo/@addon/",
        &shared_addon.to_string_lossy(),
        "ADDON_HASH",
        "ADDON_HASH",
        "",
        true,
    )
    .await;
    seed_addon(
        &fdb,
        12,
        "@addon",
        "https://example.invalid/same-repo/@addon/",
        &target_addon.to_string_lossy(),
        "ADDON_HASH",
        "ADDON_HASH",
        "",
        true,
    )
    .await;

    seed_repository_addon(&fdb, 1, 11).await;
    seed_repository_addon(&fdb, 2, 12).await;

    seed_file(
        &fdb,
        21,
        "data.pbo",
        "https://example.invalid/same-repo/@addon/data.pbo",
        &shared_file.to_string_lossy(),
        "FILE_HASH",
        "FILE_HASH",
        "",
        file_length,
        0,
    )
    .await;
    seed_file(
        &fdb,
        22,
        "data.pbo",
        "https://example.invalid/same-repo/@addon/data.pbo",
        &target_file.to_string_lossy(),
        "FILE_HASH",
        "FILE_HASH",
        "",
        file_length,
        0,
    )
    .await;

    seed_addon_file(&fdb, 11, 21).await;
    seed_addon_file(&fdb, 12, 22).await;

    let shared_context = Arc::new(
        FoxyContext::new(db.clone(), reqwest::Client::new())
            .with_target_local_path(shared_root.to_string_lossy()),
    );
    assert!(
        refresh_content_hashes_for_repository(shared_context, repo_url, None).await,
        "shared instance should establish a clean content baseline"
    );

    let target_context = Arc::new(
        FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(target_root.to_string_lossy()),
    );
    let diff = quick_local_change_diff(
        target_context,
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

    assert_eq!(diff.len(), 1);
    assert!(diff[0].needs_update);
    assert_eq!(diff[0].files.len(), 1);
    assert!(diff[0].files[0].needs_update);
}

#[tokio::test]
async fn shared_addon_propagation_keeps_sibling_quick_scan_clean() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));

    let shared_root = temp.path().join("shared");
    let addon_dir = shared_root.join("@shared_addon");
    std::fs::create_dir_all(&addon_dir).expect("create addon dir");
    let shared_file = addon_dir.join("data.pbo");
    std::fs::write(&shared_file, b"shared-addon-content-v1").expect("write shared file");

    let repo_a_url = "https://example.invalid/repo-a/";
    let repo_b_url = "https://example.invalid/repo-b/";
    let addon_local_path = addon_dir.to_string_lossy().to_string();
    let shared_file_path = shared_file.to_string_lossy().to_string();
    let file_length = std::fs::metadata(&shared_file)
        .expect("file metadata")
        .len() as i64;

    seed_repository(
        &fdb,
        1,
        "Repo A",
        repo_a_url,
        &shared_root.to_string_lossy(),
        "REPO_REMOTE",
        "REPO_REMOTE",
        "",
    )
    .await;
    seed_repository(
        &fdb,
        2,
        "Repo B",
        repo_b_url,
        &shared_root.to_string_lossy(),
        "REPO_STALE",
        "REPO_REMOTE",
        "REPO_STALE_CONTENT",
    )
    .await;

    seed_addon(
        &fdb,
        11,
        "@shared_addon",
        "https://example.invalid/repo-a/@shared_addon/",
        &addon_local_path,
        "MOD_REMOTE",
        "MOD_REMOTE",
        "",
        true,
    )
    .await;
    seed_addon(
        &fdb,
        12,
        "@shared_addon",
        "https://example.invalid/repo-b/@shared_addon/",
        &addon_local_path,
        "MOD_STALE",
        "MOD_REMOTE",
        "MOD_STALE_CONTENT",
        true,
    )
    .await;

    seed_repository_addon(&fdb, 1, 11).await;
    seed_repository_addon(&fdb, 2, 12).await;

    seed_file(
        &fdb,
        21,
        "data.pbo",
        "https://example.invalid/repo-a/@shared_addon/data.pbo",
        &shared_file_path,
        "FILE_REMOTE",
        "FILE_REMOTE",
        "",
        file_length,
        0,
    )
    .await;
    seed_file(
        &fdb,
        22,
        "data.pbo",
        "https://example.invalid/repo-b/@shared_addon/data.pbo",
        &shared_file_path,
        "FILE_STALE",
        "FILE_REMOTE",
        "FILE_STALE_CONTENT",
        file_length,
        0,
    )
    .await;

    seed_addon_file(&fdb, 11, 21).await;
    seed_addon_file(&fdb, 12, 22).await;

    fdb.execute(
        "INSERT INTO pending_updates (repository_url, local_path, diff_json, updated_at) VALUES (?, ?, ?, ?)",
        params![repo_b_url, "", "[]", 1i64],
    )
    .await
    .expect("insert sibling pending update");

    assert!(
        refresh_content_hashes_for_repository(context.clone(), repo_a_url, None).await,
        "repo A should refresh content-hash baseline from shared files"
    );

    let before = quick_local_change_diff(
        context.clone(),
        repo_b_url,
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
        before.iter().any(|m| m.needs_update),
        "repo B should report updates before sibling propagation"
    );

    let propagated_sibling_urls =
        propagate_checksums_to_siblings(context.clone(), repo_a_url).await;
    assert_eq!(propagated_sibling_urls, vec![repo_b_url.to_string()]);

    let after = quick_local_change_diff(
        context.clone(),
        repo_b_url,
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
        !after.iter().any(|m| m.needs_update),
        "repo B should be clean after sibling propagation"
    );

    let (_, _, addon_a_content) = checksums(&fdb, "addons", 11).await;
    let (addon_b_local, addon_b_remote, addon_b_content) = checksums(&fdb, "addons", 12).await;
    assert_eq!(addon_b_local, addon_b_remote);
    assert_eq!(addon_b_content, addon_a_content);
    assert!(!addon_b_content.is_empty());

    let (_, _, file_a_content) = checksums(&fdb, "files", 21).await;
    let (file_b_local, file_b_remote, file_b_content) = checksums(&fdb, "files", 22).await;
    assert_eq!(file_b_local, file_b_remote);
    assert_eq!(file_b_content, file_a_content);
    assert!(!file_b_content.is_empty());

    let (repo_b_local, repo_b_remote, repo_b_content) = checksums(&fdb, "repositories", 2).await;
    assert_eq!(repo_b_local, repo_b_remote);
    assert!(!repo_b_content.is_empty());

    assert!(
        !pending_update_exists(&fdb, repo_b_url, "").await,
        "sibling pending update should be cleared after propagation"
    );
}

#[tokio::test]
async fn content_hash_refresh_does_not_bless_addon_with_missing_manifest_file() {
    let temp = tempfile::tempdir().expect("temp dir");
    let db = build_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));

    let repo_url = "https://example.invalid/repo/";
    let repo_root = temp.path().join("repo");
    let addon_dir = repo_root.join("@addon");
    std::fs::create_dir_all(&addon_dir).expect("create addon dir");
    let present_file = addon_dir.join("present.pbo");
    let missing_file = addon_dir.join("missing.pbo");
    std::fs::write(&present_file, b"present file").expect("write present file");

    seed_repository(
        &fdb,
        1,
        "Repo",
        repo_url,
        &repo_root.to_string_lossy(),
        "REPO_REMOTE",
        "REPO_REMOTE",
        "STALE_REPO_CONTENT",
    )
    .await;

    seed_addon(
        &fdb,
        11,
        "@addon",
        "https://example.invalid/repo/@addon/",
        &addon_dir.to_string_lossy(),
        "MOD_REMOTE",
        "MOD_REMOTE",
        "STALE_ADDON_CONTENT",
        true,
    )
    .await;

    seed_repository_addon(&fdb, 1, 11).await;

    seed_file(
        &fdb,
        21,
        "present.pbo",
        "https://example.invalid/repo/@addon/present.pbo",
        &present_file.to_string_lossy(),
        "PRESENT_REMOTE",
        "PRESENT_REMOTE",
        "",
        12,
        0,
    )
    .await;
    seed_file(
        &fdb,
        22,
        "missing.pbo",
        "https://example.invalid/repo/@addon/missing.pbo",
        &missing_file.to_string_lossy(),
        "MISSING_REMOTE",
        "MISSING_REMOTE",
        "STALE_FILE_CONTENT",
        12,
        1,
    )
    .await;

    seed_addon_file(&fdb, 11, 21).await;
    seed_addon_file(&fdb, 11, 22).await;

    assert!(
        refresh_content_hashes_for_repository(context, repo_url, None).await,
        "content hash refresh should complete"
    );

    let (_, _, present_content) = checksums(&fdb, "files", 21).await;
    assert!(!present_content.is_empty());

    let (_, _, missing_content) = checksums(&fdb, "files", 22).await;
    assert!(missing_content.is_empty());

    let (_, _, addon_content) = checksums(&fdb, "addons", 11).await;
    assert!(
        addon_content.is_empty(),
        "addon baseline must stay empty so quick scan deep-scans missing files"
    );

    let (_, _, repo_content) = checksums(&fdb, "repositories", 1).await;
    assert!(
        repo_content.is_empty(),
        "repo baseline must stay empty when any addon baseline is withheld"
    );
}

// ---------------------------------------------------------------------------
// Quick scan eligibility tests
// ---------------------------------------------------------------------------

async fn create_test_db() -> std::sync::Arc<turso::Database> {
    build_db().await
}

#[tokio::test]
async fn batch_eligible_repos_empty_input() {
    let db = create_test_db().await;
    let result = batch_eligible_repos(&FoxyDb::from_turso(db.clone()), &[]).await;
    assert!(result.candidates.is_empty());
    assert_eq!(result.fast_rejected, 0);
}

#[tokio::test]
async fn batch_eligible_repos_rejects_empty_remote_checksum_only() {
    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let good_url = "https://example.invalid/good/";
    let bad_remote_url = "https://example.invalid/bad-remote/";
    let bad_content_url = "https://example.invalid/bad-content/";

    seed_repository(
        &fdb,
        1,
        "Good",
        good_url,
        "",
        "",
        "REMOTE_HASH",
        "CONTENT_HASH",
    )
    .await;
    // empty remote_checksum → rejected
    seed_repository(
        &fdb,
        2,
        "Bad Remote",
        bad_remote_url,
        "",
        "",
        "",
        "CONTENT_HASH",
    )
    .await;
    // empty content hash → still a candidate (only the remote checksum gate is "fast")
    seed_repository(
        &fdb,
        3,
        "Bad Content",
        bad_content_url,
        "",
        "",
        "REMOTE_HASH",
        "",
    )
    .await;

    let urls = vec![
        good_url.to_string(),
        bad_remote_url.to_string(),
        bad_content_url.to_string(),
    ];
    let result = batch_eligible_repos(&fdb, &urls).await;
    assert_eq!(result.fast_rejected, 1);
    let candidate_urls = result
        .candidates
        .into_iter()
        .map(|(_, url)| url)
        .collect::<HashSet<_>>();
    assert_eq!(
        candidate_urls,
        HashSet::from([good_url.to_string(), bad_content_url.to_string()])
    );
}

#[tokio::test]
async fn batch_eligible_repos_all_eligible() {
    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let url_a = "https://example.invalid/repo-a/";
    let url_b = "https://example.invalid/repo-b/";

    seed_repository(&fdb, 1, "Repo A", url_a, "", "", "HASH_A", "CONTENT_A").await;
    seed_repository(&fdb, 2, "Repo B", url_b, "", "", "HASH_B", "CONTENT_B").await;

    let urls = vec![url_a.to_string(), url_b.to_string()];
    let result = batch_eligible_repos(&fdb, &urls).await;
    assert_eq!(result.fast_rejected, 0);
    assert_eq!(result.candidates.len(), 2);
}

#[tokio::test]
async fn batch_eligible_repos_unknown_url_ignored() {
    let db = create_test_db().await;

    let urls = vec!["https://not-in-db.invalid/".to_string()];
    let result = batch_eligible_repos(&FoxyDb::from_turso(db.clone()), &urls).await;
    assert!(result.candidates.is_empty());
    assert_eq!(result.fast_rejected, 0);
}

#[tokio::test]
async fn eligible_joined_full_tree_ready() {
    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let repo_url = "https://example.invalid/full-tree/";

    // Insert a complete tree: repo → addon → file, all with checksums populated
    seed_repository(
        &fdb,
        1,
        "Full Tree",
        repo_url,
        "",
        "REPO_LOCAL",
        "REPO_REMOTE",
        "REPO_CONTENT",
    )
    .await;
    seed_addon(
        &fdb,
        1,
        "@test_addon",
        "",
        "",
        "MOD_LOCAL",
        "MOD_REMOTE",
        "MOD_CONTENT",
        false,
    )
    .await;
    seed_file(
        &fdb,
        1,
        "data.pbo",
        "",
        "",
        "FILE_LOCAL",
        "FILE_REMOTE",
        "FILE_CONTENT",
        1024,
        0,
    )
    .await;

    seed_repository_addon(&fdb, 1, 1).await;
    seed_addon_file(&fdb, 1, 1).await;
    seed_subfile(&fdb, 1, 1, "PART_LOCAL", "PART_REMOTE").await;

    // Full tree with all checksums → should be eligible
    assert!(launch_quick_scan_repo_eligible_joined(&fdb, 1, repo_url).await);
    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context.clone(), repo_url).await,
        StartupQuickScanEligibility::Prevalidated
    );

    // Also verify the individual joined helpers directly
    assert_eq!(
        remote_checksum_state_ready_joined(&fdb, 1, "test").await,
        Some(true)
    );
    assert_eq!(
        content_hash_baseline_ready_joined(&fdb, 1, "test").await,
        Some(true)
    );

    // Clear the addon's remote_checksum → should become ineligible
    fdb.execute(
        "UPDATE addons SET remote_checksum = '' WHERE id = 1",
        params![],
    )
    .await
    .expect("clear addon remote_checksum");

    assert!(!launch_quick_scan_repo_eligible_joined(&fdb, 1, repo_url).await);
    assert_eq!(
        remote_checksum_state_ready_joined(&fdb, 1, "test").await,
        Some(false)
    );

    // Startup eligibility now trusts complete repo/addon rollups.
    fdb.execute(
        "UPDATE addons SET remote_checksum = 'MOD_REMOTE' WHERE id = 1",
        params![],
    )
    .await
    .expect("restore addon remote_checksum");
    fdb.execute(
        "UPDATE files SET local_content_hash = '' WHERE id = 1",
        params![],
    )
    .await
    .expect("clear file content hash");

    assert!(!launch_quick_scan_repo_eligible_joined(&fdb, 1, repo_url).await);
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context.clone(), repo_url).await,
        StartupQuickScanEligibility::Prevalidated
    );
    assert_eq!(
        content_hash_baseline_ready_joined(&fdb, 1, "test").await,
        Some(false)
    );

    // Clearing the addon-level rollup breaks the fast path.
    fdb.execute(
        "UPDATE addons SET local_content_hash = '' WHERE id = 1",
        params![],
    )
    .await
    .expect("clear addon content hash");

    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context, repo_url).await,
        StartupQuickScanEligibility::NeedsBootstrap
    );
}

#[tokio::test]
async fn startup_eligibility_requires_part_metadata() {
    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let repo_url = "https://example.invalid/partless/";

    seed_repository(
        &fdb,
        1,
        "Partless",
        repo_url,
        "",
        "REPO_LOCAL",
        "REPO_REMOTE",
        "REPO_CONTENT",
    )
    .await;
    seed_addon(
        &fdb,
        1,
        "@partless",
        "",
        "",
        "MOD_LOCAL",
        "MOD_REMOTE",
        "",
        false,
    )
    .await;
    seed_file(
        &fdb,
        1,
        "data.pbo",
        "",
        "",
        "FILE_LOCAL",
        "FILE_REMOTE",
        "FILE_CONTENT",
        1024,
        0,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    seed_addon_file(&fdb, 1, 1).await;

    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context.clone(), repo_url).await,
        StartupQuickScanEligibility::Ineligible
    );

    seed_subfile(&fdb, 1, 1, "PART_LOCAL", "PART_REMOTE").await;
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context, repo_url).await,
        StartupQuickScanEligibility::NeedsBootstrap
    );
}

/// The preflight decides two things from part rows - "is any remote part
/// checksum missing" and "is any part still unhashed" - and answers both with
/// `LIMIT 1` probes rather than an aggregate over every part in the repository.
/// A part whose file is not already proven clean must still hold the repository
/// out of the fast path.
#[tokio::test]
async fn startup_eligibility_detects_a_part_missing_its_local_checksum() {
    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let repo_url = "https://example.invalid/unhashed-part/";

    seed_repository(
        &fdb,
        1,
        "Unhashed part",
        repo_url,
        "",
        "REPO_LOCAL",
        "REPO_REMOTE",
        "REPO_CONTENT",
    )
    .await;
    // Blank addon content hash keeps the addon fast path from short-circuiting
    // before the file and part levels are consulted.
    seed_addon(
        &fdb,
        1,
        "@unhashed",
        "",
        "",
        "MOD_LOCAL",
        "MOD_REMOTE",
        "",
        false,
    )
    .await;
    seed_file(
        &fdb,
        1,
        "data.pbo",
        "",
        "",
        "FILE_LOCAL",
        "FILE_REMOTE",
        "FILE_CONTENT",
        1024,
        0,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    seed_addon_file(&fdb, 1, 1).await;
    seed_subfile(&fdb, 1, 1, "", "PART_REMOTE").await;

    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context.clone(), repo_url).await,
        StartupQuickScanEligibility::Ineligible
    );

    // A part with no remote checksum is a metadata gap, not a local one, and
    // must also keep the repository out of the local-only path.
    fdb.execute(
        "UPDATE subfiles SET local_checksum = 'PART_LOCAL', remote_checksum = '' WHERE id = 1",
        params![],
    )
    .await
    .expect("clear part remote checksum");
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context.clone(), repo_url).await,
        StartupQuickScanEligibility::Ineligible
    );

    // Both part checksums present: only the blank addon content hash is left,
    // which is a baseline refresh rather than a tree repair.
    fdb.execute(
        "UPDATE subfiles SET remote_checksum = 'PART_REMOTE' WHERE id = 1",
        params![],
    )
    .await
    .expect("restore part remote checksum");
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context, repo_url).await,
        StartupQuickScanEligibility::NeedsBootstrap
    );
}

/// A part probe that cannot run answers nothing, not "nothing is missing". The
/// preflight has no evidence either way, so the repository must fall back to the
/// conservative verdict instead of being reported ready from a failed query.
#[tokio::test]
async fn startup_eligibility_is_conservative_when_a_part_probe_fails() {
    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let repo_url = "https://example.invalid/broken-parts/";

    seed_repository(
        &fdb,
        1,
        "Broken parts",
        repo_url,
        "",
        "REPO_LOCAL",
        "REPO_REMOTE",
        "REPO_CONTENT",
    )
    .await;
    // Blank addon content hash keeps the addon fast path from short-circuiting
    // before the part level is consulted.
    seed_addon(
        &fdb,
        1,
        "@broken",
        "",
        "",
        "MOD_LOCAL",
        "MOD_REMOTE",
        "",
        false,
    )
    .await;
    seed_file(
        &fdb,
        1,
        "data.pbo",
        "",
        "",
        "FILE_LOCAL",
        "FILE_REMOTE",
        "FILE_CONTENT",
        1024,
        0,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    seed_addon_file(&fdb, 1, 1).await;
    seed_subfile(&fdb, 1, 1, "PART_LOCAL", "PART_REMOTE").await;

    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context.clone(), repo_url).await,
        StartupQuickScanEligibility::NeedsBootstrap
    );

    fdb.execute("DROP TABLE subfiles", params![])
        .await
        .expect("drop subfiles");

    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context, repo_url).await,
        StartupQuickScanEligibility::Ineligible,
        "a failed part probe must not be read as a clean repository"
    );
}

/// A deselected optional addon is never downloaded, so its files can never earn
/// local tree checksums, part checksums or content hashes. Counting it in the
/// startup preflight left the repository `unknown` on every launch and forced a
/// manual recheck that never stuck.
#[tokio::test]
async fn startup_eligibility_ignores_disabled_addon_without_local_state() {
    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let repo_url = "https://example.invalid/optional/";

    seed_repository(
        &fdb,
        1,
        "Optional",
        repo_url,
        "",
        "REPO_LOCAL",
        "REPO_REMOTE",
        "REPO_CONTENT",
    )
    .await;
    seed_addon(
        &fdb,
        1,
        "@required",
        "",
        "",
        "MOD_LOCAL",
        "MOD_REMOTE",
        "MOD_CONTENT",
        true,
    )
    .await;
    seed_file(
        &fdb,
        1,
        "data.pbo",
        "",
        "",
        "FILE_LOCAL",
        "FILE_REMOTE",
        "FILE_CONTENT",
        1024,
        0,
    )
    .await;
    seed_repository_addon(&fdb, 1, 1).await;
    seed_addon_file(&fdb, 1, 1).await;
    seed_subfile(&fdb, 1, 1, "PART_LOCAL", "PART_REMOTE").await;

    // Never-downloaded optional addon: no local content hash anywhere and no
    // local part checksum, mirroring a folder that does not exist on disk.
    seed_addon(
        &fdb,
        2,
        "@optional_not_installed",
        "",
        "",
        "EMPTY_LOCAL",
        "MOD_REMOTE_2",
        "",
        false,
    )
    .await;
    seed_file(
        &fdb,
        2,
        "missing.pbo",
        "",
        "",
        "EMPTY_LOCAL",
        "FILE_REMOTE_2",
        "",
        2048,
        0,
    )
    .await;
    seed_repository_addon(&fdb, 1, 2).await;
    seed_addon_file(&fdb, 2, 2).await;
    seed_subfile(&fdb, 2, 2, "", "PART_REMOTE_2").await;

    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));

    // While the addon still looks enabled it blocks the fast path.
    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context.clone(), repo_url).await,
        StartupQuickScanEligibility::Ineligible
    );

    fdb.execute("UPDATE addons SET enabled = 0 WHERE id = 2", params![])
        .await
        .expect("disable optional addon");

    assert_eq!(
        launch_quick_scan_repo_startup_eligibility(context, repo_url).await,
        StartupQuickScanEligibility::Prevalidated
    );
    assert_eq!(
        content_hash_baseline_ready_joined(&fdb, 1, "test").await,
        Some(true)
    );
}

#[tokio::test]
async fn persisting_addon_selection_updates_only_changed_rows() {
    use crate::core::tasks::addon_enabled_state::persist_repository_addon_enabled_states;
    use std::collections::HashMap;

    let db = create_test_db().await;
    let fdb = FoxyDb::from_turso(db.clone());

    let repo_url = "https://example.invalid/selection/";
    let local_path = "D:/games/selection";

    seed_repository(&fdb, 1, "Selection", repo_url, local_path, "", "", "").await;
    seed_addon(&fdb, 1, "@keep", "", "", "", "", "", true).await;
    seed_addon(&fdb, 2, "@Drop", "", "", "", "", "", false).await;
    seed_repository_addon(&fdb, 1, 1).await;
    seed_repository_addon(&fdb, 1, 2).await;

    let context = Arc::new(FoxyContext::new(db.clone(), reqwest::Client::new()));
    let mut overrides = HashMap::new();
    overrides.insert("@keep".to_string(), true);
    overrides.insert("@drop".to_string(), false);

    let changed =
        persist_repository_addon_enabled_states(context.clone(), repo_url, local_path, &overrides)
            .await;
    assert_eq!(changed, 1, "only the deselected addon should change");

    let enabled_after = |id: i64| {
        let fdb = fdb.clone();
        async move {
            fdb.query_one("SELECT enabled FROM addons WHERE id = ?", params![id])
                .await
                .expect("query addon")
                .expect("addon row")
                .get_bool("enabled")
                .expect("enabled column")
        }
    };
    assert!(enabled_after(1).await);
    assert!(!enabled_after(2).await);

    // Re-running is a no-op once the stored state already matches.
    let changed_again =
        persist_repository_addon_enabled_states(context, repo_url, local_path, &overrides).await;
    assert_eq!(changed_again, 0);
}
