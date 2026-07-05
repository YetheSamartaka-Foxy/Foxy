use md5::{Digest, Md5};
use std::path::{Path, PathBuf};

/// Finalize a BLAKE3 hasher and return the first 32 hex characters (128 bits),
/// matching the length of MD5 hex output for DB column compatibility.
pub(crate) fn blake3_hex(hasher: blake3::Hasher) -> String {
    hasher.finalize().to_hex()[..32].to_uppercase()
}

/// Returns `true` if `hex_checksum` is a full-length BLAKE3 hash (64 hex chars).
/// MD5 produces 32 hex chars, SHA-1 produces 40.
pub(crate) fn is_blake3_checksum(hex_checksum: &str) -> bool {
    hex_checksum.trim().len() == 64
}

/// Unified hasher that wraps either MD5 or BLAKE3, eliminating branching at call sites.
pub(crate) enum FlexHasher {
    Md5(Md5),
    Blake3(Box<blake3::Hasher>),
}

impl FlexHasher {
    pub fn new_md5() -> Self {
        FlexHasher::Md5(Md5::new())
    }

    pub fn new_blake3() -> Self {
        FlexHasher::Blake3(Box::new(blake3::Hasher::new()))
    }

    /// Pick the algorithm based on the expected checksum's hex length.
    pub fn from_checksum(expected_hex: &str) -> Self {
        if is_blake3_checksum(expected_hex) {
            Self::new_blake3()
        } else {
            Self::new_md5()
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            FlexHasher::Md5(h) => h.update(data),
            FlexHasher::Blake3(h) => {
                h.update(data);
            }
        }
    }

    /// Consume the hasher and return the full-length uppercase hex digest.
    /// MD5 → 32 chars, BLAKE3 → 64 chars. (Unlike `blake3_hex()` which truncates to 32.)
    pub fn finalize_hex(self) -> String {
        match self {
            FlexHasher::Md5(h) => hex::encode_upper(h.finalize()),
            FlexHasher::Blake3(h) => h.finalize().to_hex().to_uppercase(),
        }
    }
}

pub(crate) fn normalize_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    normalized = normalized.trim_end_matches('/').to_string();
    if cfg!(windows) {
        normalized = normalized.to_lowercase();
    }
    normalized
}

pub(crate) fn is_foxy_temp_artifact_path(path: &str) -> bool {
    path.ends_with(".foxy.part") || path.ends_with(".foxy.tmp") || path.ends_with(".foxy.bak")
}

/// Compute a whole-file BLAKE3 hash (synchronous, for use inside `spawn_blocking`).
/// Returns the first 32 hex characters for DB column compatibility.
pub(crate) fn blake3_file_hash(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 512 * 1024];
    loop {
        let bytes = std::io::Read::read(&mut file, &mut buffer)?;
        if bytes == 0 {
            break;
        }
        hasher.update(&buffer[..bytes]);
    }
    Ok(blake3_hex(hasher))
}

// Do not include creation time: it changes on copies/restores while content does not.
pub fn calculate_addon_folder_content_hash(path: &Path) -> Result<String, std::io::Error> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_dir() {
        return Ok(String::new());
    }

    let mut file_entries: Vec<(String, u64, u128)> = Vec::new();
    let mut dir_entries: Vec<String> = Vec::new();
    let mut pending_dirs: Vec<PathBuf> = vec![path.to_path_buf()];

    while let Some(dir_path) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir_path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let file_type = entry.file_type()?;
            let entry_meta = entry.metadata()?;
            let relative_path = match entry_path.strip_prefix(path) {
                Ok(relative) => normalize_path(&relative.to_string_lossy()),
                Err(_) => continue,
            };

            if file_type.is_dir() {
                pending_dirs.push(entry_path);
                dir_entries.push(relative_path);
            } else if file_type.is_file() && !is_foxy_temp_artifact_path(&relative_path) {
                let modified_ns = entry_meta
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0);
                file_entries.push((relative_path, entry_meta.len(), modified_ns));
            }
        }
    }

    dir_entries.sort();
    file_entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"FOXY_ADDON_FOLDER_HASH_V3");
    hasher.update(normalize_path(&path.to_string_lossy()).as_bytes());

    hasher.update(&(dir_entries.len() as u64).to_le_bytes());
    for relative_path in dir_entries {
        hasher.update(relative_path.as_bytes());
    }

    hasher.update(&(file_entries.len() as u64).to_le_bytes());
    for (relative_path, len, modified_ns) in file_entries {
        hasher.update(relative_path.as_bytes());
        hasher.update(&len.to_le_bytes());
        hasher.update(&modified_ns.to_le_bytes());
    }

    Ok(blake3_hex(hasher))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;


    #[test]
    fn blake3_hex_returns_32_uppercase_hex_chars() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"hello world");
        let hex = blake3_hex(hasher);
        assert_eq!(hex, "D74981EFA70A0C880B8D8C1985D075DB");
    }

    #[test]
    fn blake3_hex_empty_input() {
        let hasher = blake3::Hasher::new();
        let hex = blake3_hex(hasher);
        assert_eq!(hex, "AF1349B9F5F9A1A6A0404DEA36DCC949");
    }


    #[test]
    fn is_blake3_checksum_64_chars() {
        let hex64 = "A".repeat(64);
        assert!(is_blake3_checksum(&hex64));
    }

    #[test]
    fn is_blake3_checksum_32_chars_is_false() {
        let hex32 = "B".repeat(32);
        assert!(!is_blake3_checksum(&hex32));
    }

    #[test]
    fn is_blake3_checksum_empty() {
        assert!(!is_blake3_checksum(""));
    }

    #[test]
    fn is_blake3_checksum_40_chars_sha1_is_false() {
        let hex40 = "D".repeat(40);
        assert!(!is_blake3_checksum(&hex40));
    }


    #[test]
    fn flex_hasher_md5_produces_32_hex() {
        // MD5("test") = 098F6BCD4621D373CADE4E832627B4F6
        let mut h = FlexHasher::new_md5();
        h.update(b"test");
        assert_eq!(h.finalize_hex(), "098F6BCD4621D373CADE4E832627B4F6");
    }

    #[test]
    fn flex_hasher_blake3_produces_64_hex() {
        let mut h = FlexHasher::new_blake3();
        h.update(b"test");
        assert_eq!(
            h.finalize_hex(),
            "4878CA0425C739FA427F7EDA20FE845F6B2E46BA5FE2A14DF5B1E32F50603215"
        );
    }

    #[test]
    fn flex_hasher_from_checksum_picks_blake3_for_64() {
        let mut h = FlexHasher::from_checksum(&"A".repeat(64));
        h.update(b"x");
        let hex = h.finalize_hex();
        assert_eq!(hex.len(), 64, "should use BLAKE3 for 64-char expected");
    }

    #[test]
    fn flex_hasher_from_checksum_picks_md5_for_32() {
        let mut h = FlexHasher::from_checksum(&"A".repeat(32));
        h.update(b"x");
        let hex = h.finalize_hex();
        assert_eq!(hex.len(), 32, "should use MD5 for 32-char expected");
    }

    #[test]
    fn flex_hasher_md5_empty_matches_known() {
        // MD5("") = D41D8CD98F00B204E9800998ECF8427E
        let h = FlexHasher::new_md5();
        assert_eq!(h.finalize_hex(), "D41D8CD98F00B204E9800998ECF8427E");
    }

    #[test]
    fn flex_hasher_md5_known_value() {
        // MD5("hello") = 5D41402ABC4B2A76B9719D911017C592
        let mut h = FlexHasher::new_md5();
        h.update(b"hello");
        assert_eq!(h.finalize_hex(), "5D41402ABC4B2A76B9719D911017C592");
    }


    #[test]
    fn normalize_path_backslashes_to_forward() {
        let result = normalize_path("foo\\bar\\baz");
        assert!(!result.contains('\\'));
        assert!(result.starts_with("foo/bar/baz") || result.starts_with("foo/bar/baz"));
    }

    #[test]
    fn normalize_path_strips_trailing_slash() {
        assert!(!normalize_path("foo/bar/").ends_with('/'));
    }

    #[test]
    fn normalize_path_strips_trailing_backslash() {
        assert!(!normalize_path("foo\\bar\\").ends_with('/'));
        assert!(!normalize_path("foo\\bar\\").ends_with('\\'));
    }

    #[test]
    fn normalize_path_empty() {
        assert_eq!(normalize_path(""), "");
    }

    #[test]
    fn temp_artifact_paths_are_detected() {
        assert!(is_foxy_temp_artifact_path("addons/file.pbo.foxy.part"));
        assert!(is_foxy_temp_artifact_path("addons/file.pbo.foxy.tmp"));
        assert!(is_foxy_temp_artifact_path("addons/file.pbo.foxy.bak"));
        assert!(!is_foxy_temp_artifact_path("addons/file.pbo"));
    }

    #[cfg(windows)]
    #[test]
    fn normalize_path_lowercases_on_windows() {
        assert_eq!(normalize_path("FOO\\BAR"), "foo/bar");
    }

    #[cfg(not(windows))]
    #[test]
    fn normalize_path_preserves_case_on_unix() {
        assert_eq!(normalize_path("FOO/BAR"), "FOO/BAR");
    }


    #[test]
    fn blake3_file_hash_reads_file_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(b"hello world").unwrap();
        }
        let hash = blake3_file_hash(&file_path).unwrap();
        assert_eq!(hash.len(), 32);

        // Determinism
        let hash2 = blake3_file_hash(&file_path).unwrap();
        assert_eq!(hash, hash2);
    }

    #[test]
    fn blake3_file_hash_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("empty.bin");
        std::fs::File::create(&file_path).unwrap();
        let hash = blake3_file_hash(&file_path).unwrap();
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn blake3_file_hash_missing_file_errors() {
        let result = blake3_file_hash(Path::new("/nonexistent/file.bin"));
        assert!(result.is_err());
    }

    #[test]
    fn blake3_file_hash_matches_manual_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("known.bin");
        std::fs::write(&file_path, b"test data").unwrap();

        let file_hash = blake3_file_hash(&file_path).unwrap();

        // Manually compute the same hash
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"test data");
        let expected = blake3_hex(hasher);

        assert_eq!(file_hash, expected);
    }


    #[test]
    fn addon_folder_hash_not_a_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("file.txt");
        std::fs::File::create(&file_path).unwrap();
        let hash = calculate_addon_folder_content_hash(&file_path).unwrap();
        assert!(hash.is_empty(), "non-directory should return empty hash");
    }

    #[test]
    fn addon_folder_hash_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let hash = calculate_addon_folder_content_hash(dir.path()).unwrap();
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn addon_folder_hash_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"content").unwrap();

        let h1 = calculate_addon_folder_content_hash(dir.path()).unwrap();
        let h2 = calculate_addon_folder_content_hash(dir.path()).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn addon_folder_hash_ignores_foxy_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.pbo");
        std::fs::write(&file, b"content").unwrap();

        let baseline = calculate_addon_folder_content_hash(dir.path()).unwrap();

        std::fs::write(dir.path().join("file.pbo.foxy.part"), b"partial").unwrap();
        std::fs::write(dir.path().join("file.pbo.foxy.tmp"), b"patched").unwrap();
        std::fs::write(dir.path().join("file.pbo.foxy.bak"), b"backup").unwrap();

        assert_eq!(
            baseline,
            calculate_addon_folder_content_hash(dir.path()).unwrap()
        );
    }

    #[test]
    fn addon_folder_hash_missing_dir_errors() {
        let result = calculate_addon_folder_content_hash(Path::new("/nonexistent/addon"));
        assert!(result.is_err());
    }

    #[test]
    fn addon_folder_hash_nested_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let sub1 = dir.path().join("a").join("b");
        std::fs::create_dir_all(&sub1).unwrap();
        std::fs::write(sub1.join("file.txt"), b"nested content").unwrap();

        let hash = calculate_addon_folder_content_hash(dir.path()).unwrap();
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn flex_hasher_incremental_update() {
        // Verify that incremental updates produce the same hash as a single update
        let mut h1 = FlexHasher::new_md5();
        h1.update(b"hello ");
        h1.update(b"world");
        let hex1 = h1.finalize_hex();

        let mut h2 = FlexHasher::new_md5();
        h2.update(b"hello world");
        let hex2 = h2.finalize_hex();

        assert_eq!(hex1, hex2);
    }


    #[test]
    fn temp_artifact_path_mid_path_not_detected() {
        assert!(!is_foxy_temp_artifact_path("file.foxy.part.extra"));
    }

    #[test]
    fn temp_artifact_path_empty() {
        assert!(!is_foxy_temp_artifact_path(""));
    }

    #[test]
    fn temp_artifact_path_suffix_only() {
        assert!(is_foxy_temp_artifact_path(".foxy.part"));
        assert!(is_foxy_temp_artifact_path(".foxy.tmp"));
        assert!(is_foxy_temp_artifact_path(".foxy.bak"));
    }


    #[test]
    fn normalize_path_multiple_trailing_slashes() {
        let result = normalize_path("path///");
        assert!(!result.ends_with('/'));
    }

    #[test]
    fn normalize_path_only_backslashes() {
        let result = normalize_path("a\\b\\c");
        assert_eq!(result.matches('/').count(), 2);
        assert!(!result.contains('\\'));
    }


    #[test]
    fn flex_hasher_blake3_incremental_update() {
        let mut h1 = FlexHasher::new_blake3();
        h1.update(b"hello ");
        h1.update(b"world");
        let hex1 = h1.finalize_hex();

        let mut h2 = FlexHasher::new_blake3();
        h2.update(b"hello world");
        let hex2 = h2.finalize_hex();

        assert_eq!(hex1, hex2);
    }

    #[test]
    fn flex_hasher_from_checksum_empty_picks_md5() {
        let mut h = FlexHasher::from_checksum("");
        h.update(b"x");
        assert_eq!(h.finalize_hex().len(), 32);
    }

    #[test]
    fn flex_hasher_from_checksum_short_picks_md5() {
        let mut h = FlexHasher::from_checksum("ABC");
        h.update(b"x");
        assert_eq!(h.finalize_hex().len(), 32);
    }
}
