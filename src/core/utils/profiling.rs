//! Opt-in whole-operation profiler for the test kit and extended diagnostics.
//!
//! Off unless `FOXY_PROFILE` is set to something other than `0` or the
//! extended diagnostics setting turned it on at runtime. When off every hook
//! below is two atomic loads and a branch, so the shipping binary pays nothing. When on, the process accumulates one report per operation covering
//! four things the existing instrumentation does not:
//!
//! - a gap-free phase timeline, including the time no phase claims
//! - every statement through the database seam, split read/write and attributed
//!   to the phase that issued it
//! - filesystem calls on the download, hash, patch and purge paths
//! - the same numbers grouped per phase, so a slow phase names its own cause
//!
//! The report is emitted as `PROFILE ...` log lines that the kit parses. Nothing
//! here changes behaviour; a profiled run is still a valid correctness run, but
//! it is not comparable with an unprofiled one for timing.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) mod fs;

/// Phase name used for work that runs outside any declared phase.
const UNATTRIBUTED: &str = "unattributed";
/// Single calls at or above these get their own debug line, since the report
/// only carries the per-label maximum.
const SLOW_DB_CALL: Duration = Duration::from_millis(100);
const SLOW_FS_CALL: Duration = Duration::from_millis(250);

static ENV_ENABLED: OnceLock<bool> = OnceLock::new();
static RUNTIME_ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether deep profiling is on for this process.
pub(crate) fn enabled() -> bool {
    RUNTIME_ENABLED.load(Ordering::Relaxed)
        || *ENV_ENABLED.get_or_init(|| {
            std::env::var("FOXY_PROFILE").is_ok_and(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0"
            })
        })
}

/// Turn profiling on or off without the environment variable. Turning it off
/// mid-operation drops the partial report rather than emitting a torn one.
pub(crate) fn set_runtime_enabled(enabled: bool) {
    RUNTIME_ENABLED.store(enabled, Ordering::Relaxed);
    if !self::enabled() {
        let mut guard = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = None;
    }
}

#[derive(Default, Clone, Copy)]
struct Stat {
    calls: u64,
    units: u64,
    nanos: u128,
    max_nanos: u128,
}

impl Stat {
    fn add(&mut self, elapsed: Duration, units: u64) {
        let nanos = elapsed.as_nanos();
        self.calls += 1;
        self.units = self.units.saturating_add(units);
        self.nanos += nanos;
        self.max_nanos = self.max_nanos.max(nanos);
    }

    fn seconds(&self) -> f64 {
        self.nanos as f64 / 1e9
    }

    fn max_ms(&self) -> f64 {
        self.max_nanos as f64 / 1e6
    }
}

#[derive(Default)]
struct State {
    started: Option<Instant>,
    /// Completed phase totals in first-seen order; a repeated phase accumulates.
    phases: Vec<(String, Duration)>,
    current: Option<(String, Instant)>,
    /// (phase, kind, statement label) -> stat. `kind` is `read` or `write`.
    db: BTreeMap<(String, &'static str, String), Stat>,
    /// (phase, op) -> stat.
    fs: BTreeMap<(String, &'static str), Stat>,
}

impl State {
    fn phase_name(&self) -> String {
        self.current
            .as_ref()
            .map_or_else(|| UNATTRIBUTED.to_owned(), |(name, _)| name.clone())
    }

    fn close_current(&mut self) {
        if let Some((name, started)) = self.current.take() {
            let elapsed = started.elapsed();
            if let Some(entry) = self.phases.iter_mut().find(|(known, _)| *known == name) {
                entry.1 += elapsed;
            } else {
                self.phases.push((name, elapsed));
            }
        }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(work: impl FnOnce(&mut State) -> R) -> Option<R> {
    if !enabled() {
        return None;
    }
    let mut guard = STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = guard.get_or_insert_with(|| State {
        started: Some(Instant::now()),
        ..State::default()
    });
    Some(work(state))
}

/// Start `name` and close whatever phase was running. Phase names are free-form
/// but must be stable across runs to stay comparable.
pub(crate) fn phase(name: &str) {
    with_state(|state| {
        state.close_current();
        state.current = Some((name.to_owned(), Instant::now()));
    });
}

/// Record one statement through the database seam. `kind` is `read` or `write`.
pub(crate) fn record_db(kind: &'static str, sql: &str, elapsed: Duration, rows: u64) {
    with_state(|state| {
        let key = (state.phase_name(), kind, statement_label(sql));
        if elapsed >= SLOW_DB_CALL {
            log::debug!(
                "PROFILE slow db phase={} kind={} label={} rows={} elapsed_ms={:.1}",
                key.0,
                kind,
                key.2,
                rows,
                elapsed.as_secs_f64() * 1e3
            );
        }
        state.db.entry(key).or_default().add(elapsed, rows);
    });
}

/// Record one filesystem call. `units` is bytes for byte-moving ops and entries
/// for directory scans, so it is reported as `units` rather than as bytes.
pub(crate) fn record_fs(op: &'static str, units: u64, elapsed: Duration) {
    with_state(|state| {
        let key = (state.phase_name(), op);
        if elapsed >= SLOW_FS_CALL {
            log::debug!(
                "PROFILE slow fs phase={} op={} units={} elapsed_ms={:.1}",
                key.0,
                op,
                units,
                elapsed.as_secs_f64() * 1e3
            );
        }
        state.fs.entry(key).or_default().add(elapsed, units);
    });
}

/// Timer for a filesystem call. Costs one branch when profiling is off.
pub(crate) struct FsTimer(Option<Instant>);

impl FsTimer {
    pub(crate) fn start() -> Self {
        Self(enabled().then(Instant::now))
    }

    pub(crate) fn stop(self, op: &'static str, units: u64) {
        if let Some(started) = self.0 {
            record_fs(op, units, started.elapsed());
        }
    }

    /// Close the timer against the database seam rather than the filesystem.
    pub(crate) fn stop_db(self, kind: &'static str, sql: &str, rows: u64) {
        if let Some(started) = self.0 {
            record_db(kind, sql, started.elapsed(), rows);
        }
    }
}

/// A statement's aggregation key: the verb plus the first table it names, so
/// call sites need no labels of their own and every seam statement is covered.
fn statement_label(sql: &str) -> String {
    const VERBS: [&str; 14] = [
        "INSERT", "UPDATE", "DELETE", "SELECT", "REPLACE", "CREATE", "DROP", "ALTER", "REINDEX",
        "VACUUM", "PRAGMA", "BEGIN", "COMMIT", "ROLLBACK",
    ];
    let verb = first_keyword(sql, &VERBS).unwrap_or("other");
    if matches!(verb, "BEGIN" | "COMMIT" | "ROLLBACK") {
        return verb.to_ascii_lowercase();
    }
    match first_table(sql) {
        Some(table) => format!("{} {}", verb.to_ascii_lowercase(), table),
        None => verb.to_ascii_lowercase(),
    }
}

/// The first of `keywords` appearing as a whole word, scanning left to right.
/// A leading CTE carries no keywords of its own, so `WITH ... UPDATE x` resolves
/// to `UPDATE` rather than to `WITH`.
fn first_keyword<'a>(sql: &str, keywords: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for word in keywords {
        if let Some(at) = find_word(sql, word)
            && best.is_none_or(|(known, _)| at < known)
        {
            best = Some((at, word));
        }
    }
    best.map(|(_, word)| word)
}

/// The identifier after the first `FROM`, `INTO` or `UPDATE`, which for every
/// statement shape in this codebase is the table the statement is about.
fn first_table(sql: &str) -> Option<String> {
    let mut best: Option<usize> = None;
    for keyword in ["FROM", "INTO", "UPDATE"] {
        if let Some(at) = find_word(sql, keyword) {
            let after = at + keyword.len();
            if best.is_none_or(|known| after < known) {
                best = Some(after);
            }
        }
    }
    let rest = sql.get(best?..)?;
    let table: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!table.is_empty()).then_some(table)
}

/// Byte offset of `word` in `sql` as a whole ASCII word, case-insensitively.
fn find_word(sql: &str, word: &str) -> Option<usize> {
    let bytes = sql.as_bytes();
    let needle = word.as_bytes();
    if needle.is_empty() || bytes.len() < needle.len() {
        return None;
    }
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for start in 0..=bytes.len() - needle.len() {
        if start > 0 && is_word_byte(bytes[start - 1]) {
            continue;
        }
        let end = start + needle.len();
        if end < bytes.len() && is_word_byte(bytes[end]) {
            continue;
        }
        if bytes[start..end].eq_ignore_ascii_case(needle) {
            return Some(start);
        }
    }
    None
}

/// Emit the report and reset, so a process that runs several operations reports
/// each one separately.
pub(crate) fn report(context: &str) {
    if !enabled() {
        return;
    }
    let state = {
        let mut guard = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.take() {
            Some(mut state) => {
                state.close_current();
                state
            }
            None => return,
        }
    };

    let wall = state
        .started
        .map_or(Duration::ZERO, |started| started.elapsed());
    let wall_s = wall.as_secs_f64();
    let phase_total: f64 = state
        .phases
        .iter()
        .map(|(_, elapsed)| elapsed.as_secs_f64())
        .sum();

    let mut db_read = Stat::default();
    let mut db_write = Stat::default();
    for ((_, kind, _), stat) in &state.db {
        let target = if *kind == "read" {
            &mut db_read
        } else {
            &mut db_write
        };
        target.calls += stat.calls;
        target.units = target.units.saturating_add(stat.units);
        target.nanos += stat.nanos;
    }
    let mut fs_total = Stat::default();
    for stat in state.fs.values() {
        fs_total.calls += stat.calls;
        fs_total.units = fs_total.units.saturating_add(stat.units);
        fs_total.nanos += stat.nanos;
    }

    log::info!(
        "PROFILE totals context={} wall_s={:.3} phase_s={:.3} unattributed_s={:.3} \
         db_read_calls={} db_read_s={:.3} db_read_rows={} db_write_calls={} db_write_s={:.3} \
         fs_calls={} fs_s={:.3} fs_units={}",
        context,
        wall_s,
        phase_total,
        (wall_s - phase_total).max(0.0),
        db_read.calls,
        db_read.seconds(),
        db_read.units,
        db_write.calls,
        db_write.seconds(),
        fs_total.calls,
        fs_total.seconds(),
        fs_total.units,
    );

    for (name, elapsed) in &state.phases {
        let seconds = elapsed.as_secs_f64();
        log::info!(
            "PROFILE phase name={} elapsed_s={:.3} share={:.3}",
            name,
            seconds,
            if wall_s > 0.0 { seconds / wall_s } else { 0.0 }
        );
    }

    for ((phase_name, kind, label), stat) in &state.db {
        log::info!(
            "PROFILE db phase={} kind={} label={} calls={} rows={} elapsed_s={:.3} max_ms={:.3}",
            phase_name,
            kind,
            label,
            stat.calls,
            stat.units,
            stat.seconds(),
            stat.max_ms(),
        );
    }

    for ((phase_name, op), stat) in &state.fs {
        log::info!(
            "PROFILE fs phase={} op={} calls={} units={} elapsed_s={:.3} max_ms={:.3}",
            phase_name,
            op,
            stat.calls,
            stat.units,
            stat.seconds(),
            stat.max_ms(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_label_names_verb_and_table() {
        assert_eq!(
            statement_label("INSERT INTO subfiles (file_id, path) VALUES (?, ?)"),
            "insert subfiles"
        );
        assert_eq!(
            statement_label("SELECT id, path FROM subfiles WHERE file_id = ?"),
            "select subfiles"
        );
        assert_eq!(
            statement_label("DELETE FROM addon_files WHERE addon_id = ?"),
            "delete addon_files"
        );
        assert_eq!(
            statement_label("UPDATE files SET local_checksum = ? WHERE id = ?"),
            "update files"
        );
    }

    #[test]
    fn statement_label_sees_through_a_leading_cte() {
        let sql = "WITH v(id, local_checksum) AS (VALUES (?, ?), (?, ?)) \
                   UPDATE subfiles SET local_checksum = v.local_checksum FROM v \
                   WHERE subfiles.id = v.id";
        assert_eq!(statement_label(sql), "update subfiles");
    }

    #[test]
    fn statement_label_keeps_an_upsert_on_its_insert() {
        let sql = "INSERT INTO subfiles (file_id, path) VALUES (?, ?) \
                   ON CONFLICT (file_id, path) DO UPDATE SET path = excluded.path";
        assert_eq!(statement_label(sql), "insert subfiles");
    }

    #[test]
    fn statement_label_collapses_transaction_control() {
        assert_eq!(statement_label("BEGIN"), "begin");
        assert_eq!(statement_label("BEGIN CONCURRENT"), "begin");
        assert_eq!(statement_label("COMMIT"), "commit");
        assert_eq!(statement_label("ROLLBACK"), "rollback");
    }

    #[test]
    fn statement_label_handles_ddl_and_pragmas() {
        assert_eq!(
            statement_label("DROP INDEX IF EXISTS idx_subfiles_file_id_path"),
            "drop"
        );
        assert_eq!(
            statement_label("CREATE UNIQUE INDEX IF NOT EXISTS i ON subfiles(file_id, path)"),
            "create"
        );
        assert_eq!(statement_label("PRAGMA journal_mode"), "pragma");
    }

    #[test]
    fn find_word_requires_whole_words() {
        assert_eq!(find_word("SELECT a FROM t", "FROM"), Some(9));
        assert_eq!(find_word("SELECT fromage FROM t", "FROM"), Some(15));
        assert_eq!(find_word("SELECT informal", "FROM"), None);
    }

    #[test]
    fn hooks_are_inert_when_profiling_is_off() {
        // The suite runs without `FOXY_PROFILE`, so every hook has to no-op
        // rather than allocate a report nobody asked for.
        assert!(!enabled());
        phase("never-recorded");
        record_db("read", "SELECT 1 FROM files", Duration::from_millis(1), 1);
        record_fs("read", 4096, Duration::from_millis(1));
        FsTimer::start().stop("write", 1);
        report("test");
        let guard = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(guard.is_none());
    }
}
