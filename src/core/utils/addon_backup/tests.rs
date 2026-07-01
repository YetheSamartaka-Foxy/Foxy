use super::*;
use std::path::{Path, PathBuf};

// ── sanitize_backup_component ───────────────────────────────────────

#[test]
fn sanitize_normal_name() {
    assert_eq!(helpers::sanitize_backup_component("@ace"), "@ace");
}

#[test]
fn sanitize_with_forbidden_chars() {
    assert_eq!(helpers::sanitize_backup_component("mod<>name"), "mod__name");
}

#[test]
fn sanitize_with_slashes() {
    assert_eq!(
        helpers::sanitize_backup_component("path/to\\mod"),
        "path_to_mod"
    );
}

#[test]
fn sanitize_dots_only() {
    assert_eq!(helpers::sanitize_backup_component("..."), "addon");
}

#[test]
fn sanitize_empty() {
    assert_eq!(helpers::sanitize_backup_component(""), "addon");
}

#[test]
fn sanitize_whitespace_only() {
    assert_eq!(helpers::sanitize_backup_component("   "), "addon");
}

#[test]
fn sanitize_control_chars() {
    assert_eq!(
        helpers::sanitize_backup_component("mod\x00name"),
        "mod_name"
    );
}

// ── backup_folder_name ──────────────────────────────────────────────

#[test]
fn backup_folder_name_format() {
    let name = helpers::backup_folder_name("@ace", "abc123");
    assert_eq!(name, "ABC123_@ace");
}

#[test]
fn backup_folder_name_trims_hash() {
    let name = helpers::backup_folder_name("mod", "  hash  ");
    assert_eq!(name, "HASH_mod");
}

// ── parse_backup_folder_name ────────────────────────────────────────

#[test]
fn parse_valid_folder_name() {
    let result = helpers::parse_backup_folder_name("ABC123_@ace");
    assert_eq!(result, Some(("ABC123".to_string(), "@ace".to_string())));
}

#[test]
fn parse_folder_name_no_underscore() {
    assert_eq!(helpers::parse_backup_folder_name("nounderscore"), None);
}

#[test]
fn parse_folder_name_empty_hash() {
    assert_eq!(helpers::parse_backup_folder_name("_addon"), None);
}

#[test]
fn parse_folder_name_empty_addon() {
    assert_eq!(helpers::parse_backup_folder_name("HASH_"), None);
}

#[test]
fn parse_folder_name_multiple_underscores() {
    let result = helpers::parse_backup_folder_name("HASH_mod_with_underscores");
    assert_eq!(
        result,
        Some(("HASH".to_string(), "mod_with_underscores".to_string()))
    );
}

// ── addon_directory_name ────────────────────────────────────────────

#[test]
fn addon_directory_name_normal() {
    let name = helpers::addon_directory_name(Path::new("/mods/@ace")).unwrap();
    assert_eq!(name, "@ace");
}

#[test]
fn addon_directory_name_root_fails() {
    assert!(helpers::addon_directory_name(Path::new("/")).is_err());
}

// ── backup_addon (full round-trip) ──────────────────────────────────

#[test]
fn backup_and_list_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let backup_root = dir.path().join("backups");
    let addon_path = dir.path().join("@test_mod");

    // Create a fake addon
    std::fs::create_dir(&addon_path).unwrap();
    std::fs::write(addon_path.join("config.cpp"), b"class CfgMods {};").unwrap();
    std::fs::write(addon_path.join("data.pbo"), b"pbo content").unwrap();

    // Back it up
    let record = backup_addon(&backup_root, &addon_path).unwrap();
    assert_eq!(record.addon_name, "@test_mod");
    assert!(!record.content_hash.is_empty());
    assert!(record.path.exists());
    assert!(record.size_bytes > 0);

    // List backups
    let backups = list_addon_backups(&backup_root, "@test_mod").unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].addon_name, "@test_mod");

    // Idempotent: backing up the same content again reuses existing backup
    let record2 = backup_addon(&backup_root, &addon_path).unwrap();
    assert_eq!(record.content_hash, record2.content_hash);
    let backups2 = list_addon_backups(&backup_root, "@test_mod").unwrap();
    assert_eq!(
        backups2.len(),
        1,
        "same content should not create duplicate"
    );
}

#[test]
fn backup_addon_non_directory_fails() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not_a_dir.txt");
    std::fs::write(&file, b"hello").unwrap();
    let result = backup_addon(dir.path(), &file);
    assert!(result.is_err());
}

// ── restore_addon_backup ────────────────────────────────────────────

#[test]
fn restore_overwrites_existing_addon() {
    let dir = tempfile::tempdir().unwrap();
    let backup_root = dir.path().join("backups");
    let addon_path = dir.path().join("@restore_test");

    // Create and backup v1
    std::fs::create_dir(&addon_path).unwrap();
    std::fs::write(addon_path.join("file.txt"), b"version1").unwrap();
    let backup_v1 = backup_addon(&backup_root, &addon_path).unwrap();

    // Modify addon to v2
    std::fs::write(addon_path.join("file.txt"), b"version2_longer").unwrap();

    // Restore v1
    restore_addon_backup(&backup_v1, &addon_path).unwrap();

    let content = std::fs::read_to_string(addon_path.join("file.txt")).unwrap();
    assert_eq!(content, "version1");
}

// ── delete_addon_backup ─────────────────────────────────────────────

#[test]
fn delete_backup_removes_directory() {
    let dir = tempfile::tempdir().unwrap();
    let backup_root = dir.path().join("backups");
    let addon_path = dir.path().join("@delete_test");

    std::fs::create_dir(&addon_path).unwrap();
    std::fs::write(addon_path.join("f.txt"), b"data").unwrap();
    let record = backup_addon(&backup_root, &addon_path).unwrap();

    assert!(record.path.exists());
    delete_addon_backup(&record).unwrap();
    assert!(!record.path.exists());
}

#[test]
fn delete_nonexistent_backup_is_ok() {
    let record = AddonBackupRecord {
        addon_name: "test".to_string(),
        content_hash: "ABC".to_string(),
        folder_name: "ABC_test".to_string(),
        path: PathBuf::from("/nonexistent/backup"),
        created_at_unix_secs: 0,
        size_bytes: 0,
    };
    assert!(delete_addon_backup(&record).is_ok());
}

// ── list_all_addon_backups ──────────────────────────────────────────

#[test]
fn list_all_backups_empty_root() {
    let dir = tempfile::tempdir().unwrap();
    let records = list_all_addon_backups(dir.path()).unwrap();
    assert!(records.is_empty());
}

#[test]
fn list_all_backups_missing_root() {
    let records = list_all_addon_backups(Path::new("/nonexistent/backups")).unwrap();
    assert!(records.is_empty());
}

#[test]
fn list_all_backups_ignores_non_backup_dirs() {
    let dir = tempfile::tempdir().unwrap();
    // Directory without underscore won't match parse_backup_folder_name
    std::fs::create_dir(dir.path().join("nounderscore")).unwrap();
    // Files (not dirs) are also ignored
    std::fs::write(dir.path().join("HASH_addon"), b"not a dir").unwrap();
    let records = list_all_addon_backups(dir.path()).unwrap();
    assert!(records.is_empty());
}

// ── cleanup_addon_backups ───────────────────────────────────────────

#[test]
fn cleanup_keep_latest_per_addon() {
    let dir = tempfile::tempdir().unwrap();
    let backup_root = dir.path().join("backups");
    let addon_path = dir.path().join("@cleanup_test");

    // Create 3 backups with different content
    for i in 0..3 {
        std::fs::create_dir_all(&addon_path).unwrap();
        std::fs::write(
            addon_path.join("data.txt"),
            format!("version {}", i).as_bytes(),
        )
        .unwrap();
        backup_addon(&backup_root, &addon_path).unwrap();
        std::fs::remove_dir_all(&addon_path).unwrap();
    }

    let before = list_all_addon_backups(&backup_root).unwrap();
    assert_eq!(before.len(), 3);

    let policy = BackupCleanupPolicy {
        keep_latest_per_addon: Some(1),
        max_age_days: None,
    };
    let report = cleanup_addon_backups(&backup_root, policy).unwrap();
    assert_eq!(report.deleted_backups, 2);

    let after = list_all_addon_backups(&backup_root).unwrap();
    assert_eq!(after.len(), 1);
}

// ── directory_total_size ────────────────────────────────────────────

#[test]
fn directory_total_size_sums_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"hello").unwrap(); // 5 bytes
    std::fs::write(dir.path().join("b.txt"), b"world!").unwrap(); // 6 bytes
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("c.txt"), b"!!").unwrap(); // 2 bytes
    let size = helpers::directory_total_size(dir.path()).unwrap();
    assert_eq!(size, 13);
}

#[test]
fn directory_total_size_single_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.bin");
    std::fs::write(&file, b"12345").unwrap();
    let size = helpers::directory_total_size(&file).unwrap();
    assert_eq!(size, 5);
}

// ── copy_directory_recursive ────────────────────────────────────────

#[test]
fn copy_directory_recursive_preserves_structure() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dst_path = dst.path().join("copy");

    std::fs::write(src.path().join("root.txt"), b"root").unwrap();
    let sub = src.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("nested.txt"), b"nested").unwrap();

    helpers::copy_directory_recursive(src.path(), &dst_path).unwrap();

    assert_eq!(
        std::fs::read_to_string(dst_path.join("root.txt")).unwrap(),
        "root"
    );
    assert_eq!(
        std::fs::read_to_string(dst_path.join("sub/nested.txt")).unwrap(),
        "nested"
    );
}

#[test]
fn copy_directory_recursive_non_dir_source_fails() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    std::fs::write(&file, b"data").unwrap();
    let result = helpers::copy_directory_recursive(&file, &dir.path().join("dst"));
    assert!(result.is_err());
}
