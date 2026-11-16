use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::core::server::monitoring::open_regular_log;

/// Both the Windows profile report and Linux stdout of an Arma client log this after it joined.
const MARKER: &[u8] = b"Starting mission:";
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_READ_PER_POLL: u64 = 512 * 1024;
const MAX_FILES: usize = 4;
const MAX_DIRECTORY_ENTRIES: usize = 256;

struct Cursor {
    path: PathBuf,
    offset: u64,
    carry: Vec<u8>,
}

/// Scans only logs written by one spawned process: its own fresh stdout file and report files
/// that did not exist when it was spawned.
pub(in crate::core::server) struct JoinEvidence {
    profiles: PathBuf,
    spawned_unix_ms: u64,
    baseline: HashSet<PathBuf>,
    files: Vec<Cursor>,
    scanned: u64,
    found: bool,
}

impl JoinEvidence {
    pub(in crate::core::server) fn before_spawn(
        stdout: PathBuf,
        profiles: PathBuf,
        spawned_unix_ms: u64,
    ) -> Self {
        let baseline = report_files(&profiles).into_iter().collect();
        Self {
            profiles,
            spawned_unix_ms,
            baseline,
            files: vec![Cursor {
                path: stdout,
                offset: 0,
                carry: Vec::new(),
            }],
            scanned: 0,
            found: false,
        }
    }

    pub(in crate::core::server) fn poll(&mut self) -> bool {
        if self.found || self.scanned >= MAX_SCAN_BYTES {
            return self.found;
        }
        for path in report_files(&self.profiles) {
            if self.files.len() >= MAX_FILES {
                break;
            }
            if self.baseline.contains(&path) || self.files.iter().any(|file| file.path == path) {
                continue;
            }
            let fresh = open_regular_log(&path)
                .and_then(|(_, metadata)| modified_ms(&metadata))
                .is_some_and(|modified| modified >= self.spawned_unix_ms);
            if fresh {
                self.files.push(Cursor {
                    path,
                    offset: 0,
                    carry: Vec::new(),
                });
            } else {
                self.baseline.insert(path);
            }
        }
        for index in 0..self.files.len() {
            if self.scanned >= MAX_SCAN_BYTES {
                break;
            }
            let budget = MAX_READ_PER_POLL.min(MAX_SCAN_BYTES - self.scanned);
            if let Some(read) = scan(&mut self.files[index], budget) {
                self.scanned = self.scanned.saturating_add(read.bytes);
                if read.found {
                    self.found = true;
                    break;
                }
            }
        }
        self.found
    }
}

struct Scan {
    bytes: u64,
    found: bool,
}

fn scan(cursor: &mut Cursor, budget: u64) -> Option<Scan> {
    let (mut file, metadata) = open_regular_log(&cursor.path)?;
    if metadata.len() < cursor.offset {
        cursor.offset = 0;
        cursor.carry.clear();
    }
    let available = metadata.len().saturating_sub(cursor.offset).min(budget);
    if available == 0 {
        return Some(Scan {
            bytes: 0,
            found: false,
        });
    }
    file.seek(SeekFrom::Start(cursor.offset)).ok()?;
    let mut bytes = Vec::with_capacity(cursor.carry.len() + available as usize);
    bytes.extend_from_slice(&cursor.carry);
    let read = (&mut file).take(available).read_to_end(&mut bytes).ok()? as u64;
    cursor.offset = cursor.offset.saturating_add(read);
    let found = bytes.windows(MARKER.len()).any(|window| window == MARKER);
    let keep = bytes.len().min(MARKER.len() - 1);
    cursor.carry = bytes[bytes.len() - keep..].to_vec();
    Some(Scan { bytes: read, found })
}

fn report_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .take(MAX_DIRECTORY_ENTRIES)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("rpt"))
        })
        .collect();
    files.sort();
    files
}

fn modified_ms(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "foxy-hc-evidence-{label}-{}",
            rand::random::<u64>()
        ));
        fs::create_dir_all(path.join("profiles")).unwrap();
        path
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[test]
    fn marker_split_across_polls_in_fresh_stdout_is_found() {
        let root = temp("split");
        let stdout = root.join("run.log");
        fs::write(&stdout, b"").unwrap();
        let mut evidence =
            JoinEvidence::before_spawn(stdout.clone(), root.join("profiles"), now_ms());
        assert!(!evidence.poll());
        let mut file = fs::OpenOptions::new().append(true).open(&stdout).unwrap();
        file.write_all(b" 0:23:42 Client connected\n 0:23:43 Starting mi")
            .unwrap();
        assert!(!evidence.poll());
        file.write_all(b"ssion:\n").unwrap();
        assert!(evidence.poll());
        assert!(evidence.poll());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_present_before_spawn_never_count_but_new_reports_do() {
        let root = temp("reports");
        let profiles = root.join("profiles");
        fs::write(profiles.join("old.rpt"), b"Starting mission:\n").unwrap();
        let stdout = root.join("run.log");
        fs::write(&stdout, b"Setting breakpad minidump AppID\n").unwrap();
        let mut evidence = JoinEvidence::before_spawn(stdout, profiles.clone(), now_ms());
        assert!(!evidence.poll());
        fs::write(profiles.join("new.RPT"), b"sessionID: abc\n").unwrap();
        assert!(!evidence.poll());
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(profiles.join("new.RPT"))
            .unwrap();
        file.write_all(b" 0:21:03 Starting mission:\n").unwrap();
        assert!(evidence.poll());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_report_written_before_spawn_time_is_ignored() {
        let root = temp("stale");
        let profiles = root.join("profiles");
        let stdout = root.join("run.log");
        fs::write(&stdout, b"").unwrap();
        let mut evidence = JoinEvidence::before_spawn(stdout, profiles.clone(), now_ms() + 60_000);
        fs::write(profiles.join("late.rpt"), b"Starting mission:\n").unwrap();
        assert!(!evidence.poll());
        assert!(!evidence.poll());
        fs::remove_dir_all(root).unwrap();
    }
}
