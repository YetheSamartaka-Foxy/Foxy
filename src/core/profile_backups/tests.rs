use super::*;
use chrono::{DateTime, Local};
use std::{fs, path::PathBuf};
use tempfile::tempdir;

fn profile(path: PathBuf, name: &str) -> crate::core::arma3_profiles::Arma3Profile {
    crate::core::arma3_profiles::Arma3Profile {
        name: name.into(),
        path,
        is_default: true,
    }
}
fn at(day: &str) -> DateTime<Local> {
    Local::now()
        .date_naive()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .unwrap()
        + chrono::Duration::days(day.parse().unwrap())
}

#[test]
fn captures_only_primary_and_companions_then_recovers_missing_profile() {
    let t = tempdir().unwrap();
    let live = t.path().join("docs/Arma 3");
    fs::create_dir_all(&live).unwrap();
    fs::write(live.join("pilot.Arma3Profile"), b"settings").unwrap();
    fs::write(live.join("pilot.vars.Arma3Profile"), b"vars").unwrap();
    fs::write(live.join("other.vars.Arma3Profile"), b"other").unwrap();
    fs::write(live.join("mission.sqm"), b"mission").unwrap();
    let root = t.path().join("backups");
    let p = profile(live.clone(), "pilot");
    let e = operations::create_backups(&root, &[p], at("0"))
        .unwrap()
        .remove(0);
    assert_eq!(e.file_count, 2);
    fs::remove_dir_all(&live).unwrap();
    let roots = vec![t.path().join("docs")];
    let recovery = operations::restore_backup(&root, &e.id, &roots).unwrap();
    assert!(recovery.exists());
    assert_eq!(
        fs::read(live.join("pilot.vars.Arma3Profile")).unwrap(),
        b"vars"
    );
    assert!(!live.join("mission.sqm").exists());
}

#[test]
fn separates_profiles_and_prunes_only_profile_with_new_capture() {
    let t = tempdir().unwrap();
    let root = t.path().join("backups");
    let a = t.path().join("a");
    let b = t.path().join("b");
    for (p, n) in [(&a, "one"), (&b, "two")] {
        fs::create_dir(p).unwrap();
        fs::write(p.join(format!("{n}.Arma3Profile")), n).unwrap();
    }
    let pa = profile(a, "one");
    let pb = profile(b, "two");
    for d in 0..4 {
        operations::create_backups(&root, std::slice::from_ref(&pa), at(&d.to_string())).unwrap();
    }
    assert_eq!(store::list_backups(&root).unwrap().len(), 3);
    operations::create_backups(&root, std::slice::from_ref(&pb), at("5")).unwrap();
    assert_eq!(store::list_backups(&root).unwrap().len(), 4);
}

#[test]
fn daily_schedule_uses_calendar_days_and_future_timestamp_is_due() {
    let t = tempdir().unwrap();
    let root = t.path().join("backups");
    let pth = t.path().join("p");
    fs::create_dir(&pth).unwrap();
    fs::write(pth.join("p.Arma3Profile"), b"x").unwrap();
    let p = profile(pth, "p");
    let today = Local::now()
        .date_naive()
        .and_hms_opt(23, 50, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .unwrap();
    assert_eq!(
        operations::run_automatic(
            &root,
            std::slice::from_ref(&p),
            &BackupPolicy::default(),
            today
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        operations::run_automatic(
            &root,
            std::slice::from_ref(&p),
            &BackupPolicy::default(),
            today + chrono::Duration::minutes(5),
        )
        .unwrap()
        .is_empty()
    );
    let tomorrow = today + chrono::Duration::minutes(20);
    assert_eq!(
        operations::run_automatic(
            &root,
            std::slice::from_ref(&p),
            &BackupPolicy::default(),
            tomorrow
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn interval_two_uses_local_calendar_days_and_future_clock_recovers_once() {
    let t = tempdir().unwrap();
    let root = t.path().join("backups");
    let pth = t.path().join("p");
    fs::create_dir(&pth).unwrap();
    fs::write(pth.join("p.Arma3Profile"), b"x").unwrap();
    let p = profile(pth, "p");
    let today = Local::now()
        .date_naive()
        .and_hms_opt(23, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .unwrap();
    let policy = BackupPolicy {
        enabled: true,
        interval_days: 2,
    };
    assert_eq!(
        operations::run_automatic(&root, std::slice::from_ref(&p), &policy, today)
            .unwrap()
            .len(),
        1
    );
    assert!(
        operations::run_automatic(
            &root,
            std::slice::from_ref(&p),
            &policy,
            today + chrono::Duration::days(1)
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        operations::run_automatic(
            &root,
            std::slice::from_ref(&p),
            &policy,
            today + chrono::Duration::days(2)
        )
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        operations::run_automatic(
            &root,
            std::slice::from_ref(&p),
            &BackupPolicy::default(),
            today - chrono::Duration::days(1)
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        operations::run_automatic(
            &root,
            std::slice::from_ref(&p),
            &BackupPolicy::default(),
            today - chrono::Duration::days(1) + chrono::Duration::minutes(5)
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn zero_length_primary_does_not_create_or_prune_backups() {
    let t = tempdir().unwrap();
    let root = t.path().join("backups");
    let live = t.path().join("p");
    fs::create_dir(&live).unwrap();
    fs::write(live.join("p.Arma3Profile"), b"ok").unwrap();
    let p = profile(live.clone(), "p");
    operations::create_backups(&root, std::slice::from_ref(&p), at("0")).unwrap();
    for d in 1..4 {
        operations::create_backups(&root, std::slice::from_ref(&p), at(&d.to_string())).unwrap();
    }
    assert_eq!(store::list_backups(&root).unwrap().len(), 3);
    fs::write(live.join("p.Arma3Profile"), b"").unwrap();
    assert!(
        operations::create_backups(&root, std::slice::from_ref(&p), at("8"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(store::list_backups(&root).unwrap().len(), 3);
}

#[test]
fn restore_twice_preserves_each_overwritten_version() {
    let t = tempdir().unwrap();
    let root = t.path().join("backups");
    let live = t.path().join("docs/profile");
    fs::create_dir_all(&live).unwrap();
    fs::write(live.join("p.Arma3Profile"), b"backup").unwrap();
    let p = profile(live.clone(), "p");
    let e = operations::create_backups(&root, &[p], at("0"))
        .unwrap()
        .remove(0);
    let roots = vec![t.path().join("docs")];
    fs::write(live.join("p.Arma3Profile"), b"first").unwrap();
    let r1 = operations::restore_backup(&root, &e.id, &roots).unwrap();
    assert_eq!(fs::read(r1.join("p.Arma3Profile")).unwrap(), b"first");
    fs::write(live.join("p.Arma3Profile"), b"second").unwrap();
    let r2 = operations::restore_backup(&root, &e.id, &roots).unwrap();
    assert_eq!(fs::read(r2.join("p.Arma3Profile")).unwrap(), b"second");
}

#[test]
fn validates_payload_and_restore_target_before_writing() {
    let t = tempdir().unwrap();
    let root = t.path().join("backups");
    let live = t.path().join("docs/profile");
    fs::create_dir_all(&live).unwrap();
    fs::write(live.join("p.Arma3Profile"), b"good").unwrap();
    let p = profile(live.clone(), "p");
    let e = operations::create_backups(&root, &[p], at("0"))
        .unwrap()
        .remove(0);
    fs::write(e.directory.join("p.Arma3Profile"), b"bad").unwrap();
    let outside = t.path().join("outside");
    assert!(operations::restore_backup(&root, &e.id, &[t.path().join("docs")]).is_err());
    assert!(!outside.exists());
    let dest = t.path().join("out.zip");
    fs::write(&dest, b"keep").unwrap();
    assert!(operations::export_backup(&root, &e.id, &dest).is_err());
    assert_eq!(fs::read(&dest).unwrap(), b"keep");
    assert!(operations::restore_backup(&root, "../escape", &[t.path().join("docs")]).is_err());
    assert!(operations::restore_backup(&root, &e.id, &[PathBuf::from("/valid/../docs")]).is_err());
}

#[test]
fn interrupted_restore_rolls_back_replacements_and_new_files() {
    let t = tempdir().unwrap();
    let root = t.path().join("backups");
    let live = t.path().join("docs");
    fs::create_dir_all(&live).unwrap();
    let recovery = root.join("recovery-pending");
    fs::create_dir_all(&recovery).unwrap();
    fs::write(live.join("p.Arma3Profile"), b"new").unwrap();
    fs::write(recovery.join("p.Arma3Profile"), b"old").unwrap();
    let journal = operations::RecoveryJournal {
        target: live.clone(),
        files: vec![
            operations::RecoveryFile {
                name: "p.Arma3Profile".into(),
                existed: true,
                temp_name: ".foxy-restore-random-p.Arma3Profile".into(),
            },
            operations::RecoveryFile {
                name: "p.vars.Arma3Profile".into(),
                existed: false,
                temp_name: ".foxy-restore-random-p.vars.Arma3Profile".into(),
            },
        ],
        complete: false,
    };
    fs::write(
        recovery.join("restore-journal.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    let roots = vec![live.clone()];
    operations::recover_interrupted_restores(&root, &roots).unwrap();
    assert_eq!(fs::read(live.join("p.Arma3Profile")).unwrap(), b"old");
    assert!(!live.join("p.vars.Arma3Profile").exists());
    assert!(!recovery.exists());
}

#[test]
fn mid_restore_failure_rolls_back_written_and_new_files() {
    let t = tempdir().unwrap();
    let target = t.path().join("docs");
    let recovery = t.path().join("recovery");
    let source = t.path().join("backup");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir(&recovery).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(target.join("p.Arma3Profile"), b"original").unwrap();
    fs::write(recovery.join("p.Arma3Profile"), b"original").unwrap();
    fs::write(source.join("p.Arma3Profile"), b"restored").unwrap();
    let journal = operations::RecoveryJournal {
        target: target.clone(),
        files: vec![
            operations::RecoveryFile {
                name: "p.Arma3Profile".into(),
                existed: true,
                temp_name: ".foxy-restore-test-p.Arma3Profile".into(),
            },
            operations::RecoveryFile {
                name: "p.vars.Arma3Profile".into(),
                existed: false,
                temp_name: ".foxy-restore-test-p.vars.Arma3Profile".into(),
            },
        ],
        complete: false,
    };
    fs::write(
        recovery.join("restore-journal.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    let records = vec![
        store::FileRecord {
            name: "p.Arma3Profile".into(),
            bytes: 8,
            blake3: blake3::hash(b"restored").to_hex().to_string(),
        },
        store::FileRecord {
            name: "p.vars.Arma3Profile".into(),
            bytes: 1,
            blake3: String::new(),
        },
    ];
    assert!(operations::promote_files(&target, &source, &records, &journal).is_err());
    operations::rollback_recovery(&recovery, std::slice::from_ref(&target)).unwrap();
    assert_eq!(
        fs::read(target.join("p.Arma3Profile")).unwrap(),
        b"original"
    );
    assert!(!target.join("p.vars.Arma3Profile").exists());
}

#[cfg(unix)]
#[test]
fn restore_rejects_symlink_target_ancestor() {
    use std::os::unix::fs::symlink;
    let t = tempdir().unwrap();
    let real = t.path().join("real");
    let link = t.path().join("docs");
    fs::create_dir(&real).unwrap();
    symlink(&real, &link).unwrap();
    assert!(paths::validate_restore_target(&link.join("profile"), &[link.clone()]).is_err());
}

#[test]
fn manifest_rejects_duplicate_and_arbitrary_names() {
    let m = store::Manifest {
        id: "valid".into(),
        profile_name: "x".into(),
        original_path: PathBuf::from("/valid/profile"),
        created_at: 0,
        files: vec![
            store::FileRecord {
                name: "a.Arma3Profile".into(),
                bytes: 1,
                blake3: "0".repeat(64),
            },
            store::FileRecord {
                name: "A.Arma3Profile".into(),
                bytes: 1,
                blake3: "0".repeat(64),
            },
        ],
    };
    assert!(store::validate_manifest(&m).is_err());
}

#[test]
fn an_unreadable_profile_does_not_stop_other_profile_backups() {
    let t = tempdir().unwrap();
    let blocked = t.path().join("blocked");
    let healthy = t.path().join("healthy");
    for path in [&blocked, &healthy] {
        fs::create_dir(path).unwrap();
        fs::write(path.join("p.Arma3Profile"), b"settings").unwrap();
    }
    fs::create_dir(blocked.join("p.vars.Arma3Profile")).unwrap();
    let root = t.path().join("backups");
    let profiles = [profile(blocked, "p"), profile(healthy.clone(), "p")];
    assert!(create_backups(&root, &profiles, at("0")).is_err());
    let entries = list_backups(&root).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].original_path, healthy);
}

#[test]
fn exports_verified_zip_and_refuses_export_into_relative_backup_store() {
    use std::io::Read;
    let cwd = std::env::current_dir().unwrap();
    let t = tempfile::Builder::new()
        .prefix(".profile-backup-test-")
        .tempdir_in(&cwd)
        .unwrap();
    let live = t.path().join("live");
    fs::create_dir(&live).unwrap();
    fs::write(live.join("p.Arma3Profile"), b"settings").unwrap();
    let root = t.path().join("backups");
    let entry = create_backups(&root, &[profile(live, "p")], at("0"))
        .unwrap()
        .remove(0);
    let output = t.path().join("profile.zip");
    export_backup(&root, &entry.id, &output).unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
    let mut payload = String::new();
    archive
        .by_name("p.Arma3Profile")
        .unwrap()
        .read_to_string(&mut payload)
        .unwrap();
    assert_eq!(payload, "settings");
    let relative = root.strip_prefix(&cwd).unwrap();
    let source = entry.directory.join("p.Arma3Profile");
    assert!(export_backup(relative, &entry.id, &source).is_err());
    assert_eq!(fs::read(source).unwrap(), b"settings");
    fs::write(entry.directory.join("manifest.json"), b"corrupt").unwrap();
    delete_all_backups(&root).unwrap();
    assert!(!entry.directory.exists());
}
