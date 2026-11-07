use super::{
    paths,
    store::{
        self, BackupEntry, BackupPolicy, FileRecord, MANIFEST, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
        Manifest, RETAIN,
    },
};
use crate::core::arma3_profiles::{self, Arma3Profile};
use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Local};
use rand::{RngExt, distr::Alphanumeric};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, write::SimpleFileOptions};

pub fn run_automatic(
    root: &Path,
    profiles: &[Arma3Profile],
    policy: &BackupPolicy,
    now: DateTime<Local>,
) -> Result<Vec<BackupEntry>> {
    if !policy.enabled {
        return Ok(Vec::new());
    }
    let old = store::list_backups(root)?;
    let due: Vec<_> = profiles
        .iter()
        .filter(|p| {
            policy.interval_days == 0
                || old
                    .iter()
                    .filter(|b| b.profile_name == p.name && same_path(&b.original_path, &p.path))
                    .filter(|b| b.created_at <= now.timestamp())
                    .max_by_key(|b| b.created_at)
                    .is_none_or(|b| is_due(b.created_at, now, policy.interval_days))
        })
        .cloned()
        .collect();
    if due.is_empty() {
        return Ok(Vec::new());
    }
    create_backups_inner(root, &due, now, true)
}
pub fn create_backups(
    root: &Path,
    profiles: &[Arma3Profile],
    now: DateTime<Local>,
) -> Result<Vec<BackupEntry>> {
    create_backups_inner(root, profiles, now, true)
}
fn create_backups_inner(
    root: &Path,
    profiles: &[Arma3Profile],
    now: DateTime<Local>,
    prune: bool,
) -> Result<Vec<BackupEntry>> {
    if profiles.is_empty() {
        return Ok(Vec::new());
    }
    if arma3_profiles::is_arma3_running() {
        bail!("Arma 3 is running; profile backup is unavailable")
    }
    paths::check_path(root, false)?;
    fs::create_dir_all(root)?;
    paths::check_path(root, true)?;
    let mut entries = Vec::new();
    let mut failure = None;
    for p in profiles {
        let captured = (|| -> Result<Option<BackupEntry>> {
            let files = profile_files(p)?;
            if files.is_empty() || fs::metadata(&files[0])?.len() == 0 {
                return Ok(None);
            }
            let id = format!(
                "{}-{:09}-{}",
                now.timestamp(),
                now.timestamp_subsec_nanos(),
                rand::rng()
                    .sample_iter(&Alphanumeric)
                    .take(12)
                    .map(char::from)
                    .collect::<String>()
            );
            let stage = root.join(format!(".{id}.staging"));
            fs::create_dir(&stage)?;
            let mut records = Vec::new();
            let mut total = 0u64;
            let result = (|| -> Result<()> {
                for source in files {
                    let name = source
                        .file_name()
                        .ok_or_else(|| anyhow!("invalid profile file"))?
                        .to_string_lossy()
                        .into_owned();
                    let before = fs::symlink_metadata(&source)?;
                    if !before.is_file() || before.len() > MAX_FILE_BYTES {
                        bail!("profile source changed during capture");
                    }
                    let mut input = fs::File::open(&source)?;
                    let output_path = stage.join(&name);
                    let mut output = fs::File::create(output_path)?;
                    let mut hasher = blake3::Hasher::new();
                    let mut count = 0u64;
                    let mut buf = [0u8; 64 * 1024];
                    loop {
                        let n = input.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        count = count
                            .checked_add(n as u64)
                            .ok_or_else(|| anyhow!("profile size overflow"))?;
                        total = total
                            .checked_add(n as u64)
                            .ok_or_else(|| anyhow!("profile size overflow"))?;
                        if count > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
                            bail!("profile backup exceeds size limit")
                        }
                        hasher.update(&buf[..n]);
                        output.write_all(&buf[..n])?;
                    }
                    let after = fs::symlink_metadata(&source)?;
                    if !after.is_file()
                        || before.len() != after.len()
                        || before.modified().ok() != after.modified().ok()
                        || count != after.len()
                    {
                        bail!("profile source changed during capture");
                    }
                    output.sync_all()?;
                    records.push(FileRecord {
                        name,
                        bytes: count,
                        blake3: hasher.finalize().to_hex().to_string(),
                    });
                }
                if records.is_empty() {
                    bail!("profile has no supported files")
                }
                let m = Manifest {
                    id: id.clone(),
                    profile_name: p.name.clone(),
                    original_path: p.path.clone(),
                    created_at: now.timestamp(),
                    files: records.clone(),
                };
                store::validate_manifest(&m)?;
                let mut manifest_file = fs::File::create(stage.join(MANIFEST))?;
                manifest_file.write_all(&serde_json::to_vec_pretty(&m)?)?;
                manifest_file.sync_all()?;
                drop(manifest_file);
                if arma3_profiles::is_arma3_running() {
                    bail!("Arma 3 is running; profile backup is unavailable");
                }
                fs::rename(&stage, root.join(&id))?;
                Ok(())
            })();
            if let Err(e) = result {
                let _ = fs::remove_dir_all(&stage);
                return Err(e);
            }
            Ok(Some(store::to_entry(
                root.join(&id),
                Manifest {
                    id,
                    profile_name: p.name.clone(),
                    original_path: p.path.clone(),
                    created_at: now.timestamp(),
                    files: records,
                },
            )))
        })();
        match captured {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    if prune {
        for entry in &entries {
            prune_profile(root, &entry.profile_name, &entry.original_path, &entry.id)?;
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(entries),
    }
}

pub fn restore_backup(root: &Path, id: &str, allowed_roots: &[PathBuf]) -> Result<PathBuf> {
    if arma3_profiles::is_arma3_running() {
        bail!("Arma 3 is running; profile restore is unavailable")
    }
    let (dir, m) = store::read_manifest(root, id)?;
    paths::validate_restore_target(&m.original_path, allowed_roots)?;
    let mut total = 0u64;
    for f in &m.files {
        let p = dir.join(&f.name);
        paths::check_path(&p, false)?;
        let md = fs::symlink_metadata(&p)?;
        if !md.is_file() || md.len() != f.bytes {
            bail!("backup validation failed")
        }
        total = total
            .checked_add(md.len())
            .ok_or_else(|| anyhow!("backup size overflow"))?;
        if total > MAX_TOTAL_BYTES {
            bail!("backup exceeds size limit")
        }
        verify(&p, f)?;
    }
    let target = &m.original_path;
    fs::create_dir_all(target)?;
    paths::validate_restore_target(target, allowed_roots)?;
    let suffix: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(10)
        .map(char::from)
        .collect();
    let recovery = root.join(format!("recovery-{id}-{suffix}"));
    fs::create_dir(&recovery)?;
    let mut journal = RecoveryJournal {
        target: target.clone(),
        files: Vec::new(),
        complete: false,
    };
    let outcome = (|| -> Result<()> {
        for f in &m.files {
            let dest = target.join(&f.name);
            paths::check_path(&dest, false)?;
            let existed = match fs::symlink_metadata(&dest) {
                Ok(md) if md.is_file() => true,
                Ok(_) => bail!("unsafe profile restore target"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.into()),
            };
            if existed {
                copy_durable(&dest, &recovery.join(&f.name))?;
            }
            let temp_name = format!(".foxy-restore-{suffix}-{}", f.name);
            journal.files.push(RecoveryFile {
                name: f.name.clone(),
                existed,
                temp_name,
            });
        }
        write_journal(&recovery, &journal)?;
        promote_files(target, &dir, &m.files, &journal)?;
        journal.complete = true;
        write_journal(&recovery, &journal)?;
        Ok(())
    })();
    if let Err(e) = outcome {
        let rollback_failed = rollback_recovery(&recovery, allowed_roots).is_err();
        if !rollback_failed {
            fs::remove_dir_all(&recovery)?;
        }
        return Err(e.context(if rollback_failed {
            "restore failed and rollback is incomplete; recovery files were preserved"
        } else {
            "restore failed; original files were restored"
        }));
    }
    Ok(recovery)
}

pub fn recover_interrupted_restores(root: &Path, allowed_roots: &[PathBuf]) -> Result<()> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    paths::check_path(root, true)?;
    for item in fs::read_dir(root)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if !item.file_type()?.is_dir() || !name.starts_with("recovery-") {
            continue;
        }
        let dir = item.path();
        paths::check_path(&dir, true)?;
        let journal_path = dir.join("restore-journal.json");
        match fs::symlink_metadata(&journal_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => paths::check_path(&journal_path, false)?,
        }
        let bytes = read_bounded(&journal_path, 64 * 1024)?;
        let journal: RecoveryJournal = serde_json::from_slice(&bytes)?;
        if journal.complete {
            continue;
        }
        if arma3_profiles::is_arma3_running() {
            bail!("Arma 3 is running; interrupted profile restore recovery is unavailable")
        }
        rollback_recovery(&dir, allowed_roots)?;
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct RecoveryJournal {
    pub target: PathBuf,
    pub files: Vec<RecoveryFile>,
    pub complete: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct RecoveryFile {
    pub name: String,
    pub existed: bool,
    pub temp_name: String,
}
fn write_journal(dir: &Path, journal: &RecoveryJournal) -> Result<()> {
    use std::io::Write;
    let path = dir.join("restore-journal.json");
    let temp = dir.join("restore-journal.tmp");
    let mut file = fs::File::create(&temp)?;
    file.write_all(&serde_json::to_vec(journal)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)?;
    Ok(())
}
fn copy_durable(source: &Path, destination: &Path) -> Result<()> {
    let mut input = fs::File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}
pub(super) fn rollback_recovery(dir: &Path, allowed_roots: &[PathBuf]) -> Result<()> {
    let bytes = read_bounded(&dir.join("restore-journal.json"), 64 * 1024)?;
    let journal: RecoveryJournal = serde_json::from_slice(&bytes)?;
    paths::validate_restore_target(&journal.target, allowed_roots)?;
    if journal.files.len() > super::store::MAX_FILES {
        bail!("invalid recovery journal file count");
    }
    let mut names = std::collections::HashSet::new();
    for entry in &journal.files {
        let suffix = format!("-{}", entry.name);
        let owns_temp = entry
            .temp_name
            .strip_prefix(".foxy-restore-")
            .and_then(|value| value.strip_suffix(&suffix))
            .is_some_and(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            });
        if !store::safe_filename(&entry.name)
            || !store::allowed_profile_file(&entry.name)
            || !store::safe_filename(&entry.temp_name)
            || !owns_temp
            || !names.insert(entry.name.to_ascii_lowercase())
        {
            bail!("invalid recovery journal filename");
        }
    }
    for entry in journal.files.iter().rev() {
        if !store::safe_filename(&entry.name) || !store::allowed_profile_file(&entry.name) {
            bail!("invalid recovery journal filename");
        }
        let target = journal.target.join(&entry.name);
        paths::check_path(&target, false)?;
        if !store::safe_filename(&entry.temp_name) {
            bail!("invalid recovery temporary filename");
        }
        let _ = fs::remove_file(journal.target.join(&entry.temp_name));
        if entry.existed {
            let backup = dir.join(&entry.name);
            paths::check_path(&backup, false)?;
            fs::copy(backup, target)?;
        } else {
            match fs::remove_file(target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
pub(super) fn promote_files(
    target: &Path,
    source_dir: &Path,
    files: &[FileRecord],
    journal: &RecoveryJournal,
) -> Result<()> {
    for f in files {
        let src = source_dir.join(&f.name);
        let dest = target.join(&f.name);
        paths::check_path(&dest, false)?;
        let temp_name = journal
            .files
            .iter()
            .find(|entry| entry.name == f.name)
            .ok_or_else(|| anyhow!("missing recovery journal entry"))?
            .temp_name
            .clone();
        if !store::safe_filename(&temp_name) {
            bail!("invalid restore temporary filename")
        }
        let tmp = target.join(temp_name);
        paths::check_path(&tmp, false)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        copy_verified(&src, f, &mut output)?;
        output.sync_all()?;
        drop(output);
        fs::rename(tmp, dest)?;
    }
    Ok(())
}

pub fn delete_all_backups(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    paths::check_path(root, true)?;
    for item in fs::read_dir(root)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if !item.file_type()?.is_dir() {
            continue;
        }
        let is_backup = backup_id_format(&name);
        let is_stage = name
            .strip_prefix('.')
            .and_then(|v| v.strip_suffix(".staging"))
            .is_some_and(backup_id_format);
        let is_recovery = recovery_id_format(&name);
        if !(is_backup || is_stage || is_recovery) {
            continue;
        }
        paths::check_path(&item.path(), true)?;
        if is_recovery {
            let journal_path = item.path().join("restore-journal.json");
            match fs::symlink_metadata(&journal_path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
                Ok(_) => {
                    paths::check_path(&journal_path, false)?;
                    let bytes = read_bounded(&journal_path, 64 * 1024)?;
                    let journal: RecoveryJournal = serde_json::from_slice(&bytes)?;
                    if !journal.complete {
                        continue;
                    }
                }
            }
        }
        fs::remove_dir_all(item.path())?;
    }
    Ok(())
}

pub fn export_backup(root: &Path, id: &str, destination: &Path) -> Result<()> {
    let (dir, m) = store::read_manifest(root, id)?;
    let dest_abs = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        std::env::current_dir()?.join(destination)
    };
    let root_abs = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    if paths::path_starts_with(&dest_abs, &root_abs) {
        bail!("export destination is inside backup storage")
    };
    paths::check_path(destination, false)?;
    for f in &m.files {
        verify(&dir.join(&f.name), f)?
    }
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("export destination has no parent"))?;
    let stage = parent.join(format!(
        ".foxy-export-{}.tmp",
        rand::rng()
            .sample_iter(&Alphanumeric)
            .take(12)
            .map(char::from)
            .collect::<String>()
    ));
    let result = (|| -> Result<()> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file(MANIFEST, options)?;
        zip.write_all(&serde_json::to_vec_pretty(&m)?)?;
        for f in &m.files {
            zip.start_file(&f.name, options)?;
            copy_verified(&dir.join(&f.name), f, &mut zip)?;
        }
        zip.finish()?.sync_all()?;
        fs::rename(&stage, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(stage);
    }
    result
}

fn profile_files(p: &Arma3Profile) -> Result<Vec<PathBuf>> {
    match fs::symlink_metadata(&p.path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    paths::check_path(&p.path, true)?;
    let mut primaries = Vec::new();
    for e in fs::read_dir(&p.path)? {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        let n = e.file_name().to_string_lossy().into_owned();
        if n.to_ascii_lowercase().ends_with(".arma3profile") {
            let lower = n.to_ascii_lowercase();
            if !lower.ends_with(".vars.arma3profile") && !lower.ends_with(".3den.arma3profile") {
                primaries.push(n);
            }
        }
    }
    let preferred = format!("{}.Arma3Profile", p.name);
    let primary = primaries
        .iter()
        .find(|n| n.eq_ignore_ascii_case(&preferred))
        .cloned()
        .or_else(|| (primaries.len() == 1).then(|| primaries[0].clone()));
    let Some(primary) = primary else {
        return Ok(Vec::new());
    };
    let stem = primary[..primary.len() - "Arma3Profile".len()].to_owned();
    let expected = [
        primary,
        format!("{stem}vars.Arma3Profile"),
        format!("{stem}3den.Arma3Profile"),
    ];
    let mut out = Vec::new();
    for n in expected {
        let path = p.path.join(&n);
        match fs::symlink_metadata(&path) {
            Ok(md) if md.is_file() => {
                paths::check_path(&path, false)?;
                out.push(path)
            }
            Ok(_) => bail!("profile file is not regular"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}
fn verify(path: &Path, f: &FileRecord) -> Result<()> {
    copy_verified(path, f, &mut std::io::sink())
}

fn copy_verified(path: &Path, f: &FileRecord, output: &mut impl Write) -> Result<()> {
    paths::check_path(path, false)?;
    let mut file = fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut n = 0u64;
    let mut b = [0u8; 64 * 1024];
    loop {
        let remaining = f
            .bytes
            .saturating_sub(n)
            .saturating_add(1)
            .min(b.len() as u64) as usize;
        if remaining == 0 {
            bail!("backup validation failed")
        }
        let r = file.read(&mut b[..remaining])?;
        if r == 0 {
            break;
        }
        n += r as u64;
        h.update(&b[..r]);
        output.write_all(&b[..r])?;
    }
    if n != f.bytes || h.finalize().to_hex().as_str() != f.blake3 {
        bail!("backup validation failed")
    }
    Ok(())
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("recovery journal exceeds size limit");
    }
    Ok(bytes)
}
fn prune_profile(root: &Path, name: &str, path: &Path, newest_id: &str) -> Result<()> {
    let mut entries = store::list_backups(root)?
        .into_iter()
        .filter(|b| b.profile_name == name && same_path(&b.original_path, path))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        (b.id == newest_id).cmp(&(a.id == newest_id)).then_with(|| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        })
    });
    for e in entries.into_iter().skip(RETAIN) {
        let (d, _) = store::read_manifest(root, &e.id)?;
        paths::check_path(&d, true)?;
        fs::remove_dir_all(d)?;
    }
    Ok(())
}
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}
fn backup_id_format(id: &str) -> bool {
    let Some((head, random)) = id.rsplit_once('-') else {
        return false;
    };
    if random.len() != 12 || !random.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    let Some((timestamp, nanos)) = head.rsplit_once('-') else {
        return false;
    };
    timestamp.parse::<i64>().is_ok()
        && nanos.len() == 9
        && nanos.bytes().all(|b| b.is_ascii_digit())
}
fn recovery_id_format(id: &str) -> bool {
    let Some(value) = id.strip_prefix("recovery-") else {
        return false;
    };
    let Some((backup, suffix)) = value.rsplit_once('-') else {
        return false;
    };
    backup_id_format(backup)
        && suffix.len() == 10
        && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
}
fn is_due(created: i64, now: DateTime<Local>, days: u16) -> bool {
    let Some(previous) = DateTime::from_timestamp(created, 0).map(|d| d.with_timezone(&Local))
    else {
        return true;
    };
    if previous > now {
        return true;
    }
    let elapsed = now
        .date_naive()
        .signed_duration_since(previous.date_naive())
        .num_days();
    elapsed >= i64::from(days)
}
