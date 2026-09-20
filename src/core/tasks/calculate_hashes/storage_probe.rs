use super::scheduling::{FileHashJob, HashStorageClass, storage_path_starts_with_mount};
use crate::core::db::{FoxyDb, params};
use crate::core::models::context::FoxyContext;
use crate::core::utils::speed_of_light::{SolLight, op_id_extra, sol_line};
use crate::ui::types::HashIoProfilePreference;
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use log::{info, warn};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use sysinfo::Disks;
use tokio::sync::watch;

const METHOD: &str = "windows_unbuffered_existing_files_v1";
const MAX_PROBE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MIN_PROBE_BYTES: u64 = 64 * 1024 * 1024;
const BLOCK_BYTES: usize = 8 * 1024 * 1024;
const ALIGNMENT: u64 = 4096;
const MAX_FILES: usize = 16;

pub(crate) const STORAGE_READ_MEASUREMENT_UPSERT_SQL: &str = "INSERT INTO storage_read_measurements \
    (volume_key, method, read_bytes, elapsed_ns, read_bps, measured_at_utc) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
    ON CONFLICT(volume_key) DO UPDATE SET \
    method=excluded.method, read_bytes=excluded.read_bytes, elapsed_ns=excluded.elapsed_ns, \
    read_bps=excluded.read_bps, measured_at_utc=excluded.measured_at_utc";

#[derive(Clone)]
struct ReadMeasurement {
    read_bytes: u64,
    elapsed_ns: u64,
    read_bps: u64,
    measured_at_utc: String,
    method: String,
}

impl ReadMeasurement {
    fn is_fresh_at(&self, now: DateTime<Utc>) -> bool {
        self.method == METHOD
            && DateTime::parse_from_rfc3339(&self.measured_at_utc)
                .ok()
                .is_some_and(|measured| {
                    let age = now.signed_duration_since(measured);
                    age >= ChronoDuration::zero() && age < ChronoDuration::days(30)
                })
            && self.read_bytes >= MIN_PROBE_BYTES
            && self.elapsed_ns > 0
            && self.read_bps > 0
    }
}

fn volume_key(path: &Path) -> Option<String> {
    let disks = Disks::new_with_refreshed_list();
    let disk = disks
        .list()
        .iter()
        .filter(|disk| storage_path_starts_with_mount(path, disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(
        disk.mount_point()
            .to_string_lossy()
            .to_ascii_lowercase()
            .as_bytes(),
    );
    hasher.update(&[0]);
    hasher.update(disk.name().to_string_lossy().as_bytes());
    hasher.update(&[0]);
    hasher.update(&disk.total_space().to_le_bytes());
    Some(hasher.finalize().to_hex().to_string())
}

async fn load(db: &FoxyDb, key: &str) -> Result<Option<ReadMeasurement>, String> {
    let row = db
        .query_one(
            "SELECT method, read_bytes, elapsed_ns, read_bps, measured_at_utc \
             FROM storage_read_measurements WHERE volume_key = ?1",
            params![key],
        )
        .await
        .map_err(|err| err.to_string())?;
    row.map(|row| {
        Ok(ReadMeasurement {
            method: row.get_string("method").map_err(|err| err.to_string())?,
            read_bytes: u64::try_from(row.get_i64("read_bytes").map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())?,
            elapsed_ns: u64::try_from(row.get_i64("elapsed_ns").map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())?,
            read_bps: u64::try_from(row.get_i64("read_bps").map_err(|err| err.to_string())?)
                .map_err(|err| err.to_string())?,
            measured_at_utc: row
                .get_string("measured_at_utc")
                .map_err(|err| err.to_string())?,
        })
    })
    .transpose()
}

async fn save(db: &FoxyDb, key: &str, value: &ReadMeasurement) -> Result<(), String> {
    db.execute_retry(
        "storage_read_measurement",
        STORAGE_READ_MEASUREMENT_UPSERT_SQL,
        params![
            key,
            value.method.as_str(),
            value.read_bytes,
            value.elapsed_ns,
            value.read_bps,
            value.measured_at_utc.as_str()
        ],
    )
    .await
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn sample_files(jobs: &[FileHashJob]) -> Vec<(PathBuf, u64)> {
    let mut files: Vec<_> = jobs
        .iter()
        .map(|job| (PathBuf::from(&job.file_path), job.file_length))
        .collect();
    files.sort_by_key(|(_, length)| std::cmp::Reverse(*length));
    let mut remaining = MAX_PROBE_BYTES;
    files
        .into_iter()
        .take(MAX_FILES)
        .filter_map(|(path, expected_length)| {
            let actual_length = std::fs::metadata(&path).ok()?.len();
            let bytes = actual_length
                .min(expected_length)
                .min(MAX_FILE_BYTES)
                .min(remaining)
                / ALIGNMENT
                * ALIGNMENT;
            if bytes == 0 {
                return None;
            }
            remaining -= bytes;
            Some((path, bytes))
        })
        .collect()
}

#[cfg(windows)]
fn probe_unbuffered(
    files: Vec<(PathBuf, u64)>,
    cancel: Option<watch::Receiver<bool>>,
) -> std::io::Result<Option<ReadMeasurement>> {
    use std::fs::OpenOptions;
    use std::io::{Error, ErrorKind};
    use std::os::windows::fs::{FileExt, OpenOptionsExt};

    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    if files.iter().map(|(_, bytes)| bytes).sum::<u64>() < MIN_PROBE_BYTES {
        return Ok(None);
    }
    let mut raw = vec![0u8; BLOCK_BYTES + ALIGNMENT as usize];
    let offset = raw.as_ptr().align_offset(ALIGNMENT as usize);
    let buffer = &mut raw[offset..offset + BLOCK_BYTES];
    let started = Instant::now();
    let mut read_bytes = 0u64;
    for (path, bytes) in files {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
            .open(path)?;
        let mut file_offset = 0u64;
        while file_offset < bytes {
            if cancel.as_ref().is_some_and(|rx| *rx.borrow()) {
                return Err(Error::new(
                    ErrorKind::Interrupted,
                    "hash read probe cancelled",
                ));
            }
            let len = (bytes - file_offset).min(BLOCK_BYTES as u64) as usize;
            let read = file.seek_read(&mut buffer[..len], file_offset)?;
            if read != len {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "hash read probe short read",
                ));
            }
            file_offset += read as u64;
            read_bytes += read as u64;
        }
    }
    let elapsed = started.elapsed().max(Duration::from_nanos(1));
    let elapsed_ns = elapsed.as_nanos() as u64;
    Ok(Some(ReadMeasurement {
        read_bytes,
        elapsed_ns,
        read_bps: (read_bytes as f64 / elapsed.as_secs_f64()) as u64,
        measured_at_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        method: METHOD.to_string(),
    }))
}

#[cfg(not(windows))]
fn probe_unbuffered(
    _files: Vec<(PathBuf, u64)>,
    _cancel: Option<watch::Receiver<bool>>,
) -> std::io::Result<Option<ReadMeasurement>> {
    Ok(None)
}

pub(super) async fn log_storage_read_measurement(
    context: &FoxyContext,
    jobs: &[FileHashJob],
    storage_class: HashStorageClass,
    requested_profile: HashIoProfilePreference,
    cancel: Option<&watch::Receiver<bool>>,
) {
    if storage_class != HashStorageClass::Ssd {
        info!(
            "Hash storage read measurement: storage={} status=not_applicable read_bps=na measured_at_utc=na",
            storage_class
        );
        return;
    }
    let Some(first_path) = jobs.first().map(|job| Path::new(&job.file_path)) else {
        return;
    };
    let Some(key) = volume_key(first_path) else {
        info!(
            "Hash storage read measurement: storage=ssd status=volume_unknown read_bps=na measured_at_utc=na"
        );
        return;
    };
    let db = context.db();
    let previous = match load(&db, &key).await {
        Ok(value) => value,
        Err(_) => {
            warn!("Hash storage read measurement load failed");
            None
        }
    };
    let now = Utc::now();
    let fresh = previous
        .as_ref()
        .is_some_and(|value| value.is_fresh_at(now));
    let mut value = previous;
    let mut status = if fresh {
        "cached"
    } else if value.is_some() {
        "stale"
    } else {
        "missing"
    };
    if !fresh && requested_profile == HashIoProfilePreference::Auto {
        let files = sample_files(jobs);
        let probe_started = Instant::now();
        match tokio::task::spawn_blocking({
            let cancel = cancel.cloned();
            move || probe_unbuffered(files, cancel)
        })
        .await
        {
            Ok(Ok(Some(measured))) => {
                if save(&db, &key, &measured).await.is_err() {
                    warn!("Hash storage read measurement save failed");
                }
                value = Some(measured);
                status = "measured";
            }
            Ok(Ok(None)) => status = "insufficient_sample",
            Ok(Err(err)) if err.kind() == std::io::ErrorKind::Interrupted => status = "cancelled",
            Ok(Err(err)) => {
                warn!(
                    "Hash storage read measurement failed: kind={:?}",
                    err.kind()
                );
                status = "failed";
            }
            Err(_) => {
                warn!("Hash storage read measurement task failed");
                status = "failed";
            }
        }
        if status == "measured" || status == "failed" || status == "cancelled" {
            let mut extras = vec![
                ("storage", "ssd".to_string()),
                ("volume_id", key[..12].to_string()),
                ("method", METHOD.to_string()),
                ("timer_scope", "sample_wall".to_string()),
                ("outcome", status.to_string()),
            ];
            extras.extend(op_id_extra(context.operation_id()));
            let (bytes, elapsed) = if status == "measured" {
                let measured = value.as_ref().expect("measured value");
                extras.push(("measured_at_utc", measured.measured_at_utc.clone()));
                (
                    measured.read_bytes,
                    Duration::from_nanos(measured.elapsed_ns),
                )
            } else {
                extras.push(("measured_at_utc", "na".to_string()));
                (0, probe_started.elapsed())
            };
            info!(
                "{}",
                sol_line(
                    "storage_read_probe",
                    bytes,
                    elapsed,
                    &SolLight::SelfBaseline,
                    &extras
                )
            );
        }
    }
    if let Some(value) = value {
        info!(
            "Hash storage read measurement: storage=ssd volume_id={} status={} method={} read_bps={} read_bytes={} elapsed_ns={} measured_at_utc={} fresh={}",
            &key[..12],
            status,
            value.method,
            value.read_bps,
            value.read_bytes,
            value.elapsed_ns,
            value.measured_at_utc,
            value.is_fresh_at(Utc::now())
        );
    } else {
        info!(
            "Hash storage read measurement: storage=ssd volume_id={} status={} read_bps=na measured_at_utc=na",
            &key[..12],
            status
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_expires_after_thirty_days() {
        let now = Utc::now();
        let mut value = ReadMeasurement {
            read_bytes: MIN_PROBE_BYTES,
            elapsed_ns: 1_000_000_000,
            read_bps: MIN_PROBE_BYTES,
            measured_at_utc: now.to_rfc3339_opts(SecondsFormat::Secs, true),
            method: METHOD.to_string(),
        };
        assert!(value.is_fresh_at(now));
        value.measured_at_utc =
            (now - ChronoDuration::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
        assert!(!value.is_fresh_at(now));
        value.measured_at_utc =
            (now + ChronoDuration::days(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
        assert!(!value.is_fresh_at(now));
    }

    #[tokio::test]
    async fn measurement_round_trips_through_game_space_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let database = crate::core::tasks::db_turso::build_and_bootstrap(path.to_str().unwrap())
            .await
            .unwrap();
        let db = FoxyDb::from_turso(std::sync::Arc::new(database));
        let value = ReadMeasurement {
            read_bytes: 128 * 1024 * 1024,
            elapsed_ns: 200_000_000,
            read_bps: 671_088_640,
            measured_at_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            method: METHOD.to_string(),
        };
        save(&db, "test-volume", &value).await.unwrap();
        let loaded = load(&db, "test-volume").await.unwrap().unwrap();
        assert_eq!(loaded.read_bps, value.read_bps);
        assert_eq!(loaded.measured_at_utc, value.measured_at_utc);
        assert!(loaded.is_fresh_at(Utc::now()));

        let mut refreshed = value.clone();
        refreshed.read_bps /= 2;
        save(&db, "test-volume", &refreshed).await.unwrap();
        assert_eq!(
            load(&db, "test-volume").await.unwrap().unwrap().read_bps,
            refreshed.read_bps
        );
    }
}
