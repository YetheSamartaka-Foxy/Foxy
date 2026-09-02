use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Write `data` to `path` atomically via a sibling temp file + fsync + rename.
///
/// On success the temp file is gone and `path` contains the new content.
/// On failure the temp file is cleaned up and the original `path` is untouched.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let temp_path = sibling_temp_path(path);

    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&temp_path)
            .with_context(|| format!("failed to create temp file {}", temp_path.display()))?;
        file.write_all(data)
            .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to fsync temp file {}", temp_path.display()))?;
        drop(file);

        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "failed to rename {} -> {}",
                temp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

/// Validate that `child` is a safe path component to join under `base`.
///
/// Rejects directory traversal (`..`), absolute paths, NTFS stream separators (`:`),
/// and null bytes.
pub fn is_safe_child_path(child: &str) -> bool {
    if child.is_empty() {
        return false;
    }
    if child.contains('\0') {
        return false;
    }

    // Reject absolute paths (Unix `/` prefix, Windows drive letter or UNC)
    if child.starts_with('/') || child.starts_with('\\') {
        return false;
    }
    if child.len() >= 2 && child.as_bytes()[1] == b':' {
        return false;
    }

    // Reject NTFS alternate data streams
    if child.contains(':') {
        return false;
    }

    // Reject any `..` component
    for component in child.split(['/', '\\']) {
        if component == ".." {
            return false;
        }
    }

    true
}

pub fn resolve_child_dir_case_insensitive(base: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    let direct = base.join(name);
    if direct.is_dir() {
        return Some(direct);
    }

    let entries = fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let entry_name = entry.file_name();
        let Some(entry_name) = entry_name.to_str() else {
            continue;
        };
        if entry_name.eq_ignore_ascii_case(name) && entry.path().is_dir() {
            return Some(entry.path());
        }
    }
    None
}

/// Sanitize an installer filename extracted from a URL.
///
/// Strips path separators, rejects dangerous characters, and validates
/// the result is non-empty.
pub fn sanitize_installer_filename(raw: &str) -> Option<String> {
    // Split on both forward and back slashes to get the final component
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw).trim();

    if name.is_empty() {
        return None;
    }

    // Strip query string / fragment
    let name = name.split(['?', '#']).next().unwrap_or(name);

    // Remove dangerous characters (NTFS streams, null, control chars)
    let sanitized: String = name
        .chars()
        .filter(|ch| !matches!(ch, ':' | '<' | '>' | '"' | '|' | '?' | '*') && !ch.is_control())
        .collect();

    let sanitized = sanitized.trim().trim_matches('.');
    if sanitized.is_empty() {
        return None;
    }

    Some(sanitized.to_string())
}

/// Whether this process can create files in `dir`, tested by actually writing a
/// probe file. Windows ACLs and read-only mounts are not visible in metadata,
/// so an attempted write is the only reliable answer.
pub fn directory_is_writable(dir: &Path) -> bool {
    let probe = dir.join(".foxy_write_test");
    match fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

/// Whether a directory that does not exist yet could be created here, tested on
/// the nearest ancestor that does exist.
pub fn destination_is_writable(dir: &Path) -> bool {
    match nearest_existing_ancestor(dir) {
        Some(existing) => directory_is_writable(&existing),
        None => false,
    }
}

fn nearest_existing_ancestor(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|candidate| candidate.is_dir())
        .map(Path::to_path_buf)
}

fn sibling_temp_path(path: &Path) -> std::path::PathBuf {
    let mut temp = path.as_os_str().to_os_string();
    temp.push(".foxy.atomic.tmp");
    std::path::PathBuf::from(temp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_child_path_rejects_traversal() {
        assert!(!is_safe_child_path(".."));
        assert!(!is_safe_child_path("../etc"));
        assert!(!is_safe_child_path("foo/../../bar"));
        assert!(!is_safe_child_path("foo\\..\\bar"));
    }

    #[test]
    fn test_is_safe_child_path_rejects_absolute() {
        assert!(!is_safe_child_path("/etc/passwd"));
        assert!(!is_safe_child_path("\\Windows\\System32"));
        assert!(!is_safe_child_path("C:\\Windows"));
    }

    #[test]
    fn test_is_safe_child_path_rejects_ntfs_streams() {
        assert!(!is_safe_child_path("file:stream"));
    }

    #[test]
    fn test_is_safe_child_path_accepts_normal() {
        assert!(is_safe_child_path("@mod_name"));
        assert!(is_safe_child_path("my-mod"));
        assert!(is_safe_child_path("addon_v2"));
        assert!(is_safe_child_path("dir/subdir/file.txt"));
    }

    #[test]
    fn test_is_safe_child_path_rejects_empty_and_null() {
        assert!(!is_safe_child_path(""));
        assert!(!is_safe_child_path("foo\0bar"));
    }

    #[test]
    fn test_sanitize_installer_filename() {
        assert_eq!(
            sanitize_installer_filename("https://example.com/path/installer.exe"),
            Some("installer.exe".to_string())
        );
        assert_eq!(
            sanitize_installer_filename("path\\to\\setup.msi"),
            Some("setup.msi".to_string())
        );
        assert_eq!(
            sanitize_installer_filename("file.exe?v=123#anchor"),
            Some("file.exe".to_string())
        );
        assert_eq!(sanitize_installer_filename(""), None);
        assert_eq!(sanitize_installer_filename(":::"), None);
    }

    #[test]
    fn test_atomic_write_basic() {
        let dir = std::env::temp_dir().join("foxy_test_atomic_write");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test_atomic.json");

        // Write initial content
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");

        // Overwrite
        atomic_write(&path, b"world").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "world");

        let _ = fs::remove_dir_all(&dir);
    }

    // ── is_safe_child_path: additional edge cases ───────────────────────

    #[test]
    fn test_is_safe_child_path_rejects_unc_paths() {
        assert!(!is_safe_child_path("\\\\server\\share\\file"));
    }

    #[test]
    fn test_is_safe_child_path_allows_dot_prefix() {
        // Single dot in a component is fine (current directory reference)
        assert!(is_safe_child_path("./foo"));
        assert!(is_safe_child_path(".hidden"));
    }

    #[test]
    fn test_is_safe_child_path_rejects_various_drive_letters() {
        assert!(!is_safe_child_path("D:\\data"));
        assert!(!is_safe_child_path("Z:\\something"));
    }

    #[test]
    fn test_is_safe_child_path_allows_deep_nesting() {
        assert!(is_safe_child_path("a/b/c/d/e/f/g/h.txt"));
    }

    // ── sanitize_installer_filename: additional edge cases ──────────────

    #[test]
    fn test_sanitize_installer_filename_strips_control_chars() {
        assert_eq!(
            sanitize_installer_filename("file\x01name\x02.exe"),
            Some("filename.exe".to_string())
        );
    }

    #[test]
    fn test_sanitize_installer_filename_dots_only() {
        // A filename of just dots should return None after trimming
        assert_eq!(sanitize_installer_filename("..."), None);
    }

    // ── sibling_temp_path ───────────────────────────────────────────────

    #[test]
    fn test_sibling_temp_path_appends_suffix() {
        let original = std::path::Path::new("/some/file.json");
        let temp = sibling_temp_path(original);
        assert!(temp.to_string_lossy().ends_with(".foxy.atomic.tmp"));
        assert!(temp.to_string_lossy().contains("file.json"));
    }

    // ── is_safe_child_path: more edge cases ────────────────────────────

    #[test]
    fn test_is_safe_child_path_single_dot_component() {
        assert!(is_safe_child_path("."));
    }

    #[test]
    fn test_is_safe_child_path_dot_dot_in_filename() {
        // "file..txt" should be fine - ".." is only dangerous as a path component
        assert!(is_safe_child_path("file..txt"));
    }

    #[test]
    fn test_is_safe_child_path_unicode_name() {
        assert!(is_safe_child_path("@моды/файл.pbo"));
    }

    #[test]
    fn test_is_safe_child_path_at_prefix() {
        assert!(is_safe_child_path("@CBA_A3"));
    }

    // ── sanitize_installer_filename: more edge cases ───────────────────

    #[test]
    fn test_sanitize_installer_filename_preserves_normal_name() {
        assert_eq!(
            sanitize_installer_filename("foxy-1.2.0-setup.exe"),
            Some("foxy-1.2.0-setup.exe".to_string())
        );
    }

    #[test]
    fn test_sanitize_installer_filename_strips_pipes_and_wildcards() {
        assert_eq!(
            sanitize_installer_filename("file|name*.exe"),
            Some("filename.exe".to_string())
        );
    }

    #[test]
    fn test_sanitize_installer_filename_just_whitespace() {
        assert_eq!(sanitize_installer_filename("   "), None);
    }

    #[test]
    fn resolve_child_dir_exact_case_returns_direct_path() {
        let dir = std::env::temp_dir().join("foxy_test_addon_exact");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("@Crows_Electronic_Warfare")).unwrap();

        let resolved =
            resolve_child_dir_case_insensitive(&dir, "@Crows_Electronic_Warfare").unwrap();
        assert_eq!(resolved, dir.join("@Crows_Electronic_Warfare"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_child_dir_mismatched_case_resolves_to_real_dir() {
        let dir = std::env::temp_dir().join("foxy_test_addon_case");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("@crows_electronic_warfare")).unwrap();

        let resolved =
            resolve_child_dir_case_insensitive(&dir, "@Crows_Electronic_Warfare").unwrap();
        assert!(resolved.is_dir());
        assert_eq!(
            resolved.canonicalize().unwrap(),
            dir.join("@crows_electronic_warfare")
                .canonicalize()
                .unwrap()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_child_dir_missing_returns_none() {
        let dir = std::env::temp_dir().join("foxy_test_addon_missing");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert!(resolve_child_dir_case_insensitive(&dir, "@not_here").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_child_dir_ignores_files_with_matching_name() {
        let dir = std::env::temp_dir().join("foxy_test_addon_file");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("@addon"), b"x").unwrap();

        assert!(resolve_child_dir_case_insensitive(&dir, "@ADDON").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_child_dir_empty_name_returns_none() {
        let dir = std::env::temp_dir();
        assert!(resolve_child_dir_case_insensitive(&dir, "   ").is_none());
    }

    #[test]
    fn resolve_child_dir_missing_base_returns_none() {
        let base = std::env::temp_dir().join("foxy_test_addon_no_base_dir");
        let _ = fs::remove_dir_all(&base);
        assert!(resolve_child_dir_case_insensitive(&base, "@addon").is_none());
    }

    #[test]
    fn directory_is_writable_accepts_a_normal_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(directory_is_writable(dir.path()));
        assert!(!dir.path().join(".foxy_write_test").exists());
    }

    #[test]
    fn directory_is_writable_rejects_a_missing_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(!directory_is_writable(&dir.path().join("absent")));
    }

    #[test]
    fn destination_is_writable_follows_the_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(destination_is_writable(
            &dir.path().join("userconfig").join("nested")
        ));
    }
}
