use super::*;
use crate::core::utils::format::redact_log_text;

struct UiLogWriter;

impl LogWriter for UiLogWriter {
    fn write(&self, now: &mut DeferredNow, record: &Record) -> std::io::Result<()> {
        let buffer = activity_log_buffer();
        let mut log_buffer = match buffer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if log_buffer.len() >= ACTIVITY_LOG_LIMIT {
            log_buffer.pop_front();
        }
        log_buffer.push_back(LogEntry {
            timestamp: now
                .format(flexi_logger::TS_DASHES_BLANK_COLONS_DOT_BLANK)
                .to_string(),
            level: record.level().to_string(),
            source: record
                .module_path()
                .or_else(|| Some(record.target()))
                .unwrap_or("<unknown>")
                .to_string(),
            message: redact_log_text(&record.args().to_string()),
        });
        ACTIVITY_LOG_GENERATION.fetch_add(1, Ordering::Relaxed);

        // Debug print to verify UI logger is working
        // eprintln!("UI LOG: {} - Buffer Size: {}", record.args(), log_buffer.len());

        Ok(())
    }

    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Process-level start time, initialized on first access (typically during logger init).
/// Used to measure elapsed wall-clock time from app start in diagnostic log messages.
pub(crate) static PROCESS_START: std::sync::LazyLock<Instant> =
    std::sync::LazyLock::new(Instant::now);

const LOG_ROTATION_SIZE_BYTES: u64 = 40 * 1024 * 1024;
const HISTORICAL_LOG_FILE_LIMIT: usize = 15;
const HISTORICAL_LOG_MAX_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct LoggerHealth {
    pub file_logging_active: bool,
    pub detail: String,
}

impl Default for LoggerHealth {
    fn default() -> Self {
        Self {
            file_logging_active: false,
            detail: "Logger not initialized".to_string(),
        }
    }
}

static LOGGER_HEALTH: std::sync::LazyLock<Mutex<LoggerHealth>> =
    std::sync::LazyLock::new(|| Mutex::new(LoggerHealth::default()));
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);
static CLOSED_PROGRESS_CHANNELS: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

pub(crate) fn ensure_logger() {
    ensure_logger_inner(false);
}

pub(crate) fn ensure_logger_with_terminal() {
    ensure_logger_inner(true);
}

fn ensure_logger_inner(duplicate_to_terminal: bool) {
    static INIT: OnceCell<()> = OnceCell::new();
    let _ = INIT.get_or_init(|| {
        // Force-initialize PROCESS_START at logger startup so it captures app start time.
        let _ = &*PROCESS_START;
        let config_dir = app_paths::foxy_logs_dir();
        let cleanup_result = prune_old_historical_logs(&config_dir, std::time::SystemTime::now());

        let file_spec = FileSpec::default()
            .directory(config_dir.clone())
            .basename("foxy");

        match Logger::try_with_env_or_str("warn, Foxy=info, foxy=info") {
            Ok(logger) => {
                let mut logger = logger
                    // Route all standard log records into the in-app activity buffer too.
                    .log_to_file_and_writer(file_spec, Box::new(UiLogWriter))
                    .rotate(
                        Criterion::Size(LOG_ROTATION_SIZE_BYTES),
                        Naming::Timestamps,
                        Cleanup::KeepLogFiles(HISTORICAL_LOG_FILE_LIMIT),
                    )
                    .write_mode(WriteMode::Direct)
                    .format_for_files(redacted_detailed_format)
                    .format_for_stdout(redacted_detailed_format);

                if duplicate_to_terminal {
                    logger = logger.duplicate_to_stdout(Duplicate::Info);
                }

                match logger.start() {
                    Ok(_) => {
                        set_logger_health(true, "File logging active");
                        info!("Logger initialized");
                        info!(
                            "Foxy app version: {} ({}) build={} commit={}",
                            env!("CARGO_PKG_VERSION"),
                            std::env::consts::ARCH,
                            crate::build_info::build_kind(),
                            crate::build_info::GIT_HASH
                        );
                        log_startup_cleanup_result(cleanup_result);
                    }
                    Err(err) => {
                        set_logger_health(false, format!("Logger failed to start: {err}"));
                        eprintln!(
                            "WARNING: Failed to start logger: {}. Continuing without file logging.",
                            err
                        );
                    }
                }
            }
            Err(err) => {
                set_logger_health(false, format!("Logger failed to initialize: {err}"));
                eprintln!(
                    "WARNING: Failed to initialize logger: {}. Continuing without file logging.",
                    err
                );
            }
        }
    });
}

fn redacted_detailed_format(
    writer: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> std::io::Result<()> {
    write!(
        writer,
        "[{}] {:<5} [{}] {}",
        now.format(flexi_logger::TS_DASHES_BLANK_COLONS_DOT_BLANK),
        record.level(),
        record.module_path().unwrap_or(record.target()),
        redact_log_text(&record.args().to_string())
    )
}

fn set_logger_health(file_logging_active: bool, detail: impl Into<String>) {
    let mut health = match LOGGER_HEALTH.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    health.file_logging_active = file_logging_active;
    health.detail = detail.into();
}

pub fn logger_health() -> LoggerHealth {
    match LOGGER_HEALTH.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

pub fn next_operation_id(prefix: &str) -> String {
    let prefix = prefix
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect::<String>();
    let prefix = if prefix.is_empty() {
        "operation"
    } else {
        &prefix
    };
    format!(
        "{}-{:04}",
        prefix,
        OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn send_progress_event(
    progress_tx: &Sender<ProgressEvent>,
    event: ProgressEvent,
    operation_id: &str,
) {
    if progress_tx.send(event).is_err() {
        warn_progress_channel_closed(operation_id);
    }
}

pub(crate) fn warn_progress_channel_closed(operation_id: &str) {
    let should_warn = match CLOSED_PROGRESS_CHANNELS.lock() {
        Ok(mut guard) => guard.insert(operation_id.to_string()),
        Err(poisoned) => poisoned.into_inner().insert(operation_id.to_string()),
    };
    if should_warn {
        warn!(
            "Progress observer disconnected; continuing background operation op={}",
            operation_id
        );
    }
}

#[derive(Default)]
struct HistoricalLogCleanupResult {
    removed: usize,
    failed: usize,
}

fn prune_old_historical_logs(
    logs_dir: &Path,
    now: std::time::SystemTime,
) -> HistoricalLogCleanupResult {
    let mut result = HistoricalLogCleanupResult::default();
    let entries = match std::fs::read_dir(logs_dir) {
        Ok(entries) => entries,
        Err(_) => {
            result.failed += 1;
            return result;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                result.failed += 1;
                continue;
            }
        };
        let path = entry.path();
        let should_prune = entry
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .and_then(|metadata| metadata.modified().ok())
            .is_some_and(|modified| should_prune_historical_log(&path, modified, now));

        if should_prune {
            match std::fs::remove_file(&path) {
                Ok(_) => result.removed += 1,
                Err(_) => result.failed += 1,
            }
        }
    }

    result
}

fn should_prune_historical_log(
    path: &Path,
    modified: std::time::SystemTime,
    now: std::time::SystemTime,
) -> bool {
    is_historical_foxy_log(path)
        && now
            .duration_since(modified)
            .is_ok_and(|age| age > HISTORICAL_LOG_MAX_AGE)
}

fn is_historical_foxy_log(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    file_name.starts_with("foxy_r")
        && file_name.ends_with(".log")
        && !file_name.eq_ignore_ascii_case("foxy_rCURRENT.log")
}

fn log_startup_cleanup_result(result: HistoricalLogCleanupResult) {
    if result.removed > 0 {
        info!(
            "Startup log cleanup removed {} historical log files older than 90 days.",
            result.removed
        );
    }
    if result.failed > 0 {
        warn!(
            "Startup log cleanup skipped {} historical log entries due to I/O errors.",
            result.failed
        );
    }
}

const ACTIVITY_LOG_LIMIT: usize = 2000;

pub(super) fn request_background_repaint(repaint_ctx: Option<&egui::Context>) {
    if let Some(ctx) = repaint_ctx {
        ctx.request_repaint();
    }
}

static ACTIVITY_LOG_BUFFER: OnceCell<Arc<Mutex<VecDeque<LogEntry>>>> = OnceCell::new();
static ACTIVITY_LOG_GENERATION: AtomicU64 = AtomicU64::new(0);
pub(super) const CONTENT_HASH_PERSIST_LOG_INTERVAL: usize = 5_000;

fn activity_log_buffer() -> Arc<Mutex<VecDeque<LogEntry>>> {
    ACTIVITY_LOG_BUFFER
        .get_or_init(|| Arc::new(Mutex::new(VecDeque::with_capacity(ACTIVITY_LOG_LIMIT))))
        .clone()
}

pub fn activity_log_snapshot() -> Vec<LogEntry> {
    let buffer = activity_log_buffer();
    let guard = match buffer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.iter().cloned().collect()
}

pub fn activity_log_generation() -> u64 {
    ACTIVITY_LOG_GENERATION.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_only_rotated_foxy_logs_as_historical() {
        assert!(is_historical_foxy_log(Path::new(
            "foxy_r2026-05-17_11-47-19.log"
        )));
        assert!(!is_historical_foxy_log(Path::new("foxy_rCURRENT.log")));
        assert!(!is_historical_foxy_log(Path::new("other_r2026-05-17.log")));
        assert!(!is_historical_foxy_log(Path::new("foxy_r2026-05-17.txt")));
    }

    #[test]
    fn prunes_only_historical_logs_older_than_max_age() {
        let now = std::time::UNIX_EPOCH + HISTORICAL_LOG_MAX_AGE + Duration::from_secs(1);
        let expired = std::time::UNIX_EPOCH;
        let fresh = now - HISTORICAL_LOG_MAX_AGE;

        assert!(should_prune_historical_log(
            Path::new("foxy_r2026-01-01_00-00-00.log"),
            expired,
            now
        ));
        assert!(!should_prune_historical_log(
            Path::new("foxy_r2026-05-17_11-47-19.log"),
            fresh,
            now
        ));
        assert!(!should_prune_historical_log(
            Path::new("foxy_rCURRENT.log"),
            expired,
            now
        ));
    }
}
