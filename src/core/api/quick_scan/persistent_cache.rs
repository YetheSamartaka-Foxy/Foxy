use super::super::*;
use crate::core::utils::format::sanitize_log_path;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AddonRootFingerprint {
    pub(super) exists: bool,
    pub(super) is_dir: bool,
    pub(super) length: u64,
    pub(super) modified_ns: u128,
    pub(super) created_ns: u128,
    pub(super) readonly: bool,
    #[serde(default)]
    pub(super) normalized_path: String,
    #[serde(default)]
    pub(super) content_fingerprint_ready: bool,
    #[serde(default)]
    pub(super) relevant_file_count: u64,
    #[serde(default)]
    pub(super) relevant_dir_count: u64,
    #[serde(default)]
    pub(super) aggregate_file_size: u64,
    #[serde(default)]
    pub(super) newest_relevant_file_modified_ns: u128,
    #[serde(default)]
    pub(super) layout_hash: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct PersistentAddonHashEntry {
    pub(super) fingerprint: AddonRootFingerprint,
    pub(super) content_hash: String,
    pub(super) updated_unix_ms: u64,
    /// Last scan hit/write time used for save-time eviction.
    #[serde(default)]
    pub(super) last_seen_unix_ms: u64,
}

impl PersistentAddonHashEntry {
    fn effective_last_seen_unix_ms(&self) -> u64 {
        self.last_seen_unix_ms.max(self.updated_unix_ms)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistentAddonHashCacheFile {
    /// Content-hash format generation for cached `content_hash` values.
    #[serde(default)]
    format: u32,
    entries: HashMap<String, PersistentAddonHashEntry>,
}

pub(super) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn quick_scan_addon_hash_cache_path() -> PathBuf {
    let mut path = app_paths::foxy_data_dir();
    path.push("quick_scan_addon_hash_cache.json");
    path
}

pub(super) fn load_persistent_addon_hash_cache() -> HashMap<String, PersistentAddonHashEntry> {
    let started = std::time::Instant::now();
    let path = quick_scan_addon_hash_cache_path();
    let payload = match std::fs::read_to_string(&path) {
        Ok(payload) => payload,
        Err(err) => {
            if err.kind() != ErrorKind::NotFound {
                warn!(
                    "Failed to read quick-scan addon hash cache {}: {}",
                    sanitize_log_path(&path),
                    err
                );
            }
            return HashMap::new();
        }
    };

    match serde_json::from_str::<PersistentAddonHashCacheFile>(&payload) {
        Ok(cache) => {
            let current_format = crate::core::tasks::db_schema_version::CONTENT_HASH_FORMAT;
            if cache.format != current_format {
                info!(
                    "Persistent addon hash cache format {} != current {}; discarding {} entries (content-hash algorithm changed)",
                    cache.format,
                    current_format,
                    cache.entries.len()
                );
                return HashMap::new();
            }
            let stale_count = cache
                .entries
                .values()
                .filter(|e| {
                    let age_ms = now_unix_ms().saturating_sub(e.updated_unix_ms);
                    !e.fingerprint.content_fingerprint_ready && age_ms > PERSISTENT_CACHE_TTL_MS
                })
                .count();
            info!(
                "Persistent addon hash cache loaded: entries={} stale={} elapsed={:.2?}",
                cache.entries.len(),
                stale_count,
                started.elapsed()
            );
            cache.entries
        }
        Err(err) => {
            warn!(
                "Failed to parse quick-scan addon hash cache {}: {}",
                sanitize_log_path(&path),
                err
            );
            HashMap::new()
        }
    }
}

pub(super) fn save_persistent_addon_hash_cache(
    entries: &HashMap<String, PersistentAddonHashEntry>,
) {
    let started = std::time::Instant::now();
    let path = quick_scan_addon_hash_cache_path();
    let eviction_cutoff = now_unix_ms().saturating_sub(PERSISTENT_CACHE_EVICT_AFTER_MS);
    let retained: HashMap<String, PersistentAddonHashEntry> = entries
        .iter()
        .filter(|(_, entry)| entry.effective_last_seen_unix_ms() >= eviction_cutoff)
        .map(|(key, entry)| (key.clone(), entry.clone()))
        .collect();
    let evicted = entries.len().saturating_sub(retained.len());
    if evicted > 0 {
        info!(
            "Persistent addon hash cache evicting {} unused entries (older than {} days)",
            evicted,
            PERSISTENT_CACHE_EVICT_AFTER_MS / (24 * 60 * 60 * 1000)
        );
    }
    let file = PersistentAddonHashCacheFile {
        format: crate::core::tasks::db_schema_version::CONTENT_HASH_FORMAT,
        entries: retained,
    };
    let payload = match serde_json::to_vec(&file) {
        Ok(payload) => payload,
        Err(err) => {
            warn!("Failed to serialize quick-scan addon hash cache: {}", err);
            return;
        }
    };

    let mut temp_path = path.clone();
    temp_path.set_extension("tmp");
    if let Err(err) = std::fs::write(&temp_path, &payload) {
        warn!(
            "Failed to write quick-scan addon hash cache temp file {}: {}",
            sanitize_log_path(&temp_path),
            err
        );
        return;
    }
    // On Windows, rename fails if the target exists - remove it first,
    // handling NotFound gracefully instead of a TOCTOU exists() check.
    match std::fs::remove_file(&path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            warn!(
                "Failed to remove old quick-scan addon hash cache {}: {}",
                sanitize_log_path(&path),
                err
            );
        }
    }
    if let Err(err) = std::fs::rename(&temp_path, &path) {
        warn!(
            "Failed to rotate quick-scan addon hash cache temp file {} -> {}: {}",
            sanitize_log_path(&temp_path),
            sanitize_log_path(&path),
            err
        );
        let _ = std::fs::remove_file(&temp_path);
        return;
    }

    info!(
        "Persistent addon hash cache saved: entries={} size={} bytes elapsed={:.2?}",
        file.entries.len(),
        payload.len(),
        started.elapsed()
    );
}

pub(super) fn addon_root_fingerprint(path: &str) -> AddonRootFingerprint {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return AddonRootFingerprint::default();
    }
    let root = Path::new(trimmed);
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(_) => return AddonRootFingerprint::default(),
    };
    let mut fingerprint = AddonRootFingerprint {
        exists: true,
        is_dir: metadata.is_dir(),
        length: metadata.len(),
        modified_ns: system_time_unix_ns(metadata.modified().ok()),
        created_ns: system_time_unix_ns(metadata.created().ok()),
        readonly: metadata.permissions().readonly(),
        normalized_path: crate::core::utils::content_hash::normalize_path(trimmed),
        ..Default::default()
    };

    if !metadata.is_dir() {
        return fingerprint;
    }

    match collect_addon_root_fingerprint_stats(root) {
        Ok(stats) => {
            fingerprint.content_fingerprint_ready = true;
            fingerprint.relevant_file_count = stats.relevant_file_count;
            fingerprint.relevant_dir_count = stats.relevant_dir_count;
            fingerprint.aggregate_file_size = stats.aggregate_file_size;
            fingerprint.newest_relevant_file_modified_ns = stats.newest_relevant_file_modified_ns;
            fingerprint.layout_hash = stats.layout_hash;
        }
        Err(err) => {
            debug!(
                "Failed to build quick-scan addon root fingerprint for {}: {}",
                fingerprint.normalized_path, err
            );
        }
    }

    fingerprint
}

#[derive(Default)]
struct AddonRootFingerprintStats {
    relevant_file_count: u64,
    relevant_dir_count: u64,
    aggregate_file_size: u64,
    newest_relevant_file_modified_ns: u128,
    layout_hash: String,
}

fn collect_addon_root_fingerprint_stats(root: &Path) -> std::io::Result<AddonRootFingerprintStats> {
    let mut dir_entries: Vec<String> = Vec::new();
    let mut file_entries: Vec<(String, u64, u128)> = Vec::new();
    let mut pending_dirs: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir_path) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir_path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let file_type = entry.file_type()?;
            let entry_meta = entry.metadata()?;
            let relative_path = match entry_path.strip_prefix(root) {
                Ok(relative) => {
                    crate::core::utils::content_hash::normalize_path(&relative.to_string_lossy())
                }
                Err(_) => continue,
            };

            if file_type.is_dir() {
                pending_dirs.push(entry_path);
                dir_entries.push(relative_path);
            } else if file_type.is_file()
                && !crate::core::utils::content_hash::is_foxy_temp_artifact_path(&relative_path)
            {
                file_entries.push((
                    relative_path,
                    entry_meta.len(),
                    system_time_unix_ns(entry_meta.modified().ok()),
                ));
            }
        }
    }

    dir_entries.sort();
    file_entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut aggregate_file_size = 0u64;
    let mut newest_relevant_file_modified_ns = 0u128;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"FOXY_ADDON_ROOT_FINGERPRINT_V2");

    hasher.update(&(dir_entries.len() as u64).to_le_bytes());
    for relative_path in &dir_entries {
        hasher.update(relative_path.as_bytes());
    }

    hasher.update(&(file_entries.len() as u64).to_le_bytes());
    for (relative_path, len, modified_ns) in &file_entries {
        aggregate_file_size = aggregate_file_size.saturating_add(*len);
        newest_relevant_file_modified_ns = newest_relevant_file_modified_ns.max(*modified_ns);
        hasher.update(relative_path.as_bytes());
        hasher.update(&len.to_le_bytes());
        hasher.update(&modified_ns.to_le_bytes());
    }

    Ok(AddonRootFingerprintStats {
        relevant_file_count: file_entries.len() as u64,
        relevant_dir_count: dir_entries.len() as u64,
        aggregate_file_size,
        newest_relevant_file_modified_ns,
        layout_hash: crate::core::utils::content_hash::blake3_hex(hasher),
    })
}

fn system_time_unix_ns(value: Option<std::time::SystemTime>) -> u128 {
    value
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn addon_root_fingerprint_stable_match(
    cached: &AddonRootFingerprint,
    current: &AddonRootFingerprint,
) -> bool {
    // Persistent addon hash cache key:
    // - path identity, existence, and directory bit
    // - recursive counts, aggregate file size, newest relevant file mtime
    // - BLAKE3 layout hash of relative dir/file names, file sizes, and file
    //   mtimes, excluding Foxy temp artifacts
    //
    // Root directory length/ctime/mtime are intentionally excluded because NTFS
    // and sync-side metadata writes can change them without changing addon
    // contents. File-level metadata remains part of the key because the cached
    // addon content hash is also metadata-sensitive.
    cached.content_fingerprint_ready
        && current.content_fingerprint_ready
        && cached.exists == current.exists
        && cached.is_dir == current.is_dir
        && cached.normalized_path == current.normalized_path
        && cached.relevant_file_count == current.relevant_file_count
        && cached.relevant_dir_count == current.relevant_dir_count
        && cached.aggregate_file_size == current.aggregate_file_size
        && cached.newest_relevant_file_modified_ns == current.newest_relevant_file_modified_ns
        && cached.layout_hash == current.layout_hash
}

pub(super) fn persistent_addon_fingerprint_is_current(
    cached: &AddonRootFingerprint,
    current: &AddonRootFingerprint,
) -> bool {
    addon_root_fingerprint_stable_match(cached, current) || cached == current
}

/// Maximum age for legacy entries whose fingerprints predate the content-aware format.
const PERSISTENT_CACHE_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// Save-time eviction window for entries no scan still touches.
const PERSISTENT_CACHE_EVICT_AFTER_MS: u64 = 30 * 24 * 60 * 60 * 1000;

pub(super) fn persistent_addon_fingerprint_matches(
    entry: &PersistentAddonHashEntry,
    current: &AddonRootFingerprint,
) -> bool {
    if entry.content_hash.is_empty() {
        return false;
    }
    if !entry.fingerprint.content_fingerprint_ready {
        let age_ms = now_unix_ms().saturating_sub(entry.updated_unix_ms);
        if age_ms > PERSISTENT_CACHE_TTL_MS {
            return false;
        }
    }
    addon_root_fingerprint_stable_match(&entry.fingerprint, current)
}

pub(super) fn persistent_addon_fingerprint_mismatch_reasons(
    entry: &PersistentAddonHashEntry,
    current: &AddonRootFingerprint,
) -> Vec<&'static str> {
    let cached = &entry.fingerprint;
    let mut reasons = Vec::new();
    if entry.content_hash.is_empty() {
        reasons.push("empty_content_hash");
    }
    let age_ms = now_unix_ms().saturating_sub(entry.updated_unix_ms);
    if !cached.content_fingerprint_ready && age_ms > PERSISTENT_CACHE_TTL_MS {
        reasons.push("expired");
    }
    if !cached.content_fingerprint_ready || !current.content_fingerprint_ready {
        reasons.push("content_fingerprint_unready");
    }
    if cached.exists != current.exists {
        reasons.push("exists");
    }
    if cached.is_dir != current.is_dir {
        reasons.push("is_dir");
    }
    if cached.normalized_path != current.normalized_path {
        reasons.push("normalized_path");
    }
    if cached.relevant_file_count != current.relevant_file_count {
        reasons.push("file_count");
    }
    if cached.relevant_dir_count != current.relevant_dir_count {
        reasons.push("dir_count");
    }
    if cached.aggregate_file_size != current.aggregate_file_size {
        reasons.push("aggregate_file_size");
    }
    if cached.newest_relevant_file_modified_ns != current.newest_relevant_file_modified_ns {
        reasons.push("newest_file_mtime");
    }
    if cached.layout_hash != current.layout_hash {
        reasons.push("layout_hash");
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stable_fingerprint(path: &str) -> AddonRootFingerprint {
        AddonRootFingerprint {
            exists: true,
            is_dir: true,
            length: 0,
            modified_ns: 10,
            created_ns: 20,
            readonly: false,
            normalized_path: crate::core::utils::content_hash::normalize_path(path),
            content_fingerprint_ready: true,
            relevant_file_count: 2,
            relevant_dir_count: 1,
            aggregate_file_size: 42,
            newest_relevant_file_modified_ns: 30,
            layout_hash: "LAYOUT".to_string(),
        }
    }

    #[test]
    fn persistent_fingerprint_match_ignores_volatile_root_times() {
        let mut cached = stable_fingerprint("S:\\Mods\\@Addon");
        cached.modified_ns = 100;
        cached.created_ns = 200;
        cached.length = 4096;

        let mut current = stable_fingerprint("S:/Mods/@Addon");
        current.modified_ns = 300;
        current.created_ns = 400;
        current.length = 0;

        let entry = PersistentAddonHashEntry {
            fingerprint: cached,
            content_hash: "HASH".to_string(),
            updated_unix_ms: now_unix_ms(),
            last_seen_unix_ms: 0,
        };

        assert!(persistent_addon_fingerprint_matches(&entry, &current));
        assert!(persistent_addon_fingerprint_is_current(
            &entry.fingerprint,
            &current
        ));
    }

    #[test]
    fn persistent_fingerprint_miss_when_content_signature_changes() {
        let cached = stable_fingerprint("S:\\Mods\\@Addon");
        let mut current = stable_fingerprint("S:\\Mods\\@Addon");
        current.aggregate_file_size += 1;
        current.layout_hash = "OTHER".to_string();

        let entry = PersistentAddonHashEntry {
            fingerprint: cached,
            content_hash: "HASH".to_string(),
            updated_unix_ms: now_unix_ms(),
            last_seen_unix_ms: 0,
        };

        assert!(!persistent_addon_fingerprint_matches(&entry, &current));
        let reasons = persistent_addon_fingerprint_mismatch_reasons(&entry, &current);
        assert!(reasons.contains(&"aggregate_file_size"));
        assert!(reasons.contains(&"layout_hash"));
    }

    #[test]
    fn legacy_cache_entries_deserialize_with_stable_fields_defaulted() {
        let payload = r#"{
            "entries": {
                "s:/mods/@addon": {
                    "fingerprint": {
                        "exists": true,
                        "is_dir": true,
                        "length": 4096,
                        "modified_ns": 100,
                        "created_ns": 50,
                        "readonly": false
                    },
                    "content_hash": "HASH",
                    "updated_unix_ms": 1
                }
            }
        }"#;

        let cache: PersistentAddonHashCacheFile = serde_json::from_str(payload).unwrap();
        let entry = cache.entries.get("s:/mods/@addon").unwrap();

        assert!(!entry.fingerprint.content_fingerprint_ready);
        assert!(entry.fingerprint.normalized_path.is_empty());
        assert!(entry.fingerprint.layout_hash.is_empty());
    }

    #[test]
    fn persistent_fingerprint_rejects_empty_content_hash() {
        let cached = stable_fingerprint("S:\\Mods\\@Addon");
        let current = stable_fingerprint("S:\\Mods\\@Addon");
        let entry = PersistentAddonHashEntry {
            fingerprint: cached,
            content_hash: String::new(), // empty hash
            updated_unix_ms: now_unix_ms(),
            last_seen_unix_ms: 0,
        };
        assert!(!persistent_addon_fingerprint_matches(&entry, &current));
    }

    /// Content-aware fingerprint matches do not expire by age.
    #[test]
    fn persistent_fingerprint_survives_age_when_content_ready() {
        let cached = stable_fingerprint("S:\\Mods\\@Addon");
        let current = stable_fingerprint("S:\\Mods\\@Addon");
        let entry = PersistentAddonHashEntry {
            fingerprint: cached,
            content_hash: "VALID_HASH".to_string(),
            updated_unix_ms: 1, // far beyond the legacy TTL
            last_seen_unix_ms: 0,
        };
        assert!(persistent_addon_fingerprint_matches(&entry, &current));
    }

    #[test]
    fn persistent_fingerprint_rejects_expired_legacy_entry() {
        let mut cached = stable_fingerprint("S:\\Mods\\@Addon");
        cached.content_fingerprint_ready = false;
        let current = stable_fingerprint("S:\\Mods\\@Addon");
        let entry = PersistentAddonHashEntry {
            fingerprint: cached,
            content_hash: "VALID_HASH".to_string(),
            updated_unix_ms: 1, // very old timestamp
            last_seen_unix_ms: 0,
        };
        assert!(!persistent_addon_fingerprint_matches(&entry, &current));
        let reasons = persistent_addon_fingerprint_mismatch_reasons(&entry, &current);
        assert!(reasons.contains(&"expired"));
    }

    #[test]
    fn effective_last_seen_prefers_newest_timestamp() {
        let entry = PersistentAddonHashEntry {
            fingerprint: stable_fingerprint("S:\\Mods\\@Addon"),
            content_hash: "HASH".to_string(),
            updated_unix_ms: 10,
            last_seen_unix_ms: 20,
        };
        assert_eq!(entry.effective_last_seen_unix_ms(), 20);

        let legacy = PersistentAddonHashEntry {
            fingerprint: stable_fingerprint("S:\\Mods\\@Addon"),
            content_hash: "HASH".to_string(),
            updated_unix_ms: 10,
            last_seen_unix_ms: 0,
        };
        assert_eq!(legacy.effective_last_seen_unix_ms(), 10);
    }

    #[test]
    fn addon_root_fingerprint_empty_path_returns_default() {
        let fp = addon_root_fingerprint("");
        assert!(!fp.exists);
        assert!(!fp.is_dir);
    }

    #[test]
    fn addon_root_fingerprint_whitespace_only_returns_default() {
        let fp = addon_root_fingerprint("   ");
        assert!(!fp.exists);
    }

    #[test]
    fn now_unix_ms_is_positive() {
        assert!(now_unix_ms() > 0);
    }
}
