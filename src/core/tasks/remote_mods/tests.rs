use super::helpers::*;
use crate::core::models::repository::FoxyRepository;
use serde_json::json;
use std::collections::HashSet;

// ── join_path ───────────────────────────────────────────────────────

#[test]
fn join_path_with_trailing_slash() {
    assert_eq!(
        join_path("https://repo.com/", "@mod"),
        "https://repo.com/@mod"
    );
}

#[test]
fn join_path_without_trailing_slash() {
    assert_eq!(
        join_path("https://repo.com", "@mod"),
        "https://repo.com/@mod"
    );
}

#[test]
fn join_path_with_trailing_backslash() {
    assert_eq!(join_path("C:\\mods\\", "@mod"), "C:\\mods\\@mod");
}

#[test]
fn join_path_no_trailing_separator() {
    assert_eq!(join_path("C:\\mods", "@mod"), "C:\\mods/@mod");
}

// ── mod_task_limit ──────────────────────────────────────────────────

#[test]
fn mod_task_limit_within_bounds() {
    let limit = mod_task_limit();
    assert!(limit >= 4, "limit should be at least 4, got {}", limit);
}

// ── collect_desired_mod_pairs ───────────────────────────────────────

fn make_parent(remote_url: &str, local_path: &str) -> FoxyRepository {
    FoxyRepository {
        id: 1,
        name: "test".to_string(),
        remote_url: remote_url.to_string(),
        local_path: local_path.to_string(),
        image: "".to_string(),
        local_checksum: "".to_string(),
        local_content_hash: "".to_string(),
        remote_checksum: "".to_string(),
        foxy_mode: crate::core::models::repository::FoxyMode::None,
    }
}

#[test]
fn collect_mods_basic() {
    let parent = make_parent("https://repo.com/", "/mods/");
    let data = json!([
        {"modName": "@ace", "checkSum": "ABC"},
        {"modName": "@cba", "checkSum": "DEF"}
    ]);
    let mut dedupe = HashSet::new();
    let mut out = Vec::new();
    collect_desired_mod_pairs(&parent, None, &data, &mut dedupe, &mut out);
    assert_eq!(out.len(), 2);
    assert!(out[0].0.contains("@ace"));
    assert!(out[1].0.contains("@cba"));
}

#[test]
fn collect_mods_skips_empty_names() {
    let parent = make_parent("https://repo.com/", "/mods/");
    let data = json!([
        {"modName": "", "checkSum": "ABC"},
        {"modName": "@valid", "checkSum": "DEF"}
    ]);
    let mut dedupe = HashSet::new();
    let mut out = Vec::new();
    collect_desired_mod_pairs(&parent, None, &data, &mut dedupe, &mut out);
    assert_eq!(out.len(), 1);
    assert!(out[0].0.contains("@valid"));
}

#[test]
fn collect_mods_deduplicates() {
    let parent = make_parent("https://repo.com/", "/mods/");
    let data = json!([
        {"modName": "@ace", "checkSum": "ABC"},
        {"modName": "@ace", "checkSum": "DEF"}
    ]);
    let mut dedupe = HashSet::new();
    let mut out = Vec::new();
    collect_desired_mod_pairs(&parent, None, &data, &mut dedupe, &mut out);
    assert_eq!(out.len(), 1, "duplicate mod names should be deduped");
}

#[test]
fn collect_mods_non_array_is_noop() {
    let parent = make_parent("https://repo.com/", "/mods/");
    let data = json!({"not": "an array"});
    let mut dedupe = HashSet::new();
    let mut out = Vec::new();
    collect_desired_mod_pairs(&parent, None, &data, &mut dedupe, &mut out);
    assert!(out.is_empty());
}

#[test]
fn collect_mods_missing_mod_name_field() {
    let parent = make_parent("https://repo.com/", "/mods/");
    let data = json!([
        {"checkSum": "ABC"},
        {"modName": "@valid"}
    ]);
    let mut dedupe = HashSet::new();
    let mut out = Vec::new();
    collect_desired_mod_pairs(&parent, None, &data, &mut dedupe, &mut out);
    assert_eq!(out.len(), 1, "entry without modName should be skipped");
}

#[test]
fn repository_space_resolver_reuses_present_shared_addon_only() {
    let unique = format!(
        "foxy-space-path-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    );
    let root = std::env::temp_dir().join(unique);
    let shared = root.join("shared");
    let target = root.join("target");
    std::fs::create_dir_all(shared.join("@common")).expect("create shared addon");
    std::fs::create_dir_all(&target).expect("create target root");

    let common = resolve_mod_local_path(
        &target.to_string_lossy(),
        Some(&shared.to_string_lossy()),
        "@common",
    );
    let specific = resolve_mod_local_path(
        &target.to_string_lossy(),
        Some(&shared.to_string_lossy()),
        "@specific",
    );

    assert_eq!(
        crate::core::utils::content_hash::normalize_path(&common),
        crate::core::utils::content_hash::normalize_path(&shared.join("@common").to_string_lossy())
    );
    assert_eq!(
        crate::core::utils::content_hash::normalize_path(&specific),
        crate::core::utils::content_hash::normalize_path(
            &target.join("@specific").to_string_lossy()
        )
    );

    std::fs::remove_dir_all(root).expect("remove test tree");
}
