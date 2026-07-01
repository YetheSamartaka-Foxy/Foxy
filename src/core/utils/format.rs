use std::path::Path;

/// Sanitize a filesystem path for logging.
///
/// Home-directory paths remain useful as `~`-relative paths. Other absolute
/// paths keep their full structure with only username segments redacted.
pub(crate) fn sanitize_log_path(path: &std::path::Path) -> String {
    sanitize_log_path_str(&path.display().to_string())
}

/// Sanitize a string path for logging.
pub(crate) fn sanitize_log_path_str(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return String::new();
    }

    if let Some(home) = dirs::home_dir() {
        let home_str = home.display().to_string();
        if let Some(stripped) = path.strip_prefix(&home_str) {
            return format!("~{}", stripped);
        }
        // Also try forward-slash variant for URLs
        let home_fwd = home_str.replace('\\', "/");
        if let Some(stripped) = path.strip_prefix(&home_fwd) {
            return format!("~{}", stripped);
        }
    }

    if path.contains("://") {
        return sanitize_log_url(path);
    }

    if looks_absolute_path(path) {
        return redacted_absolute_path(path);
    }

    clean_log_text(path)
}

/// Clean a URL before it enters logs or exported diagnostics.
///
/// Repository and other URLs are kept intact; only stray newlines are removed
/// so a single log line cannot be split across rows.
pub(crate) fn sanitize_log_url(raw: &str) -> String {
    clean_log_text(raw.trim())
}

/// Redact the current user's home directory (and account name) from arbitrary
/// log text.
///
/// This is the last line of defense for sinks and exported historical logs.
/// Only the local account name is removed; repository URLs and other paths are
/// left intact. Replacing the full home-directory string keeps account names
/// made of two or more words (e.g. `Daniel Elisak`) from being parsed
/// incorrectly.
pub(crate) fn redact_log_text(text: &str) -> String {
    redact_home_paths(&clean_log_text(text))
}

fn redact_home_paths(text: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return text.to_string();
    };

    let parent = home
        .parent()
        .map(|parent| parent.display().to_string().replace('\\', "/"));
    let replacement = match parent.as_deref() {
        Some(parent) if !parent.is_empty() => format!("{parent}/<redacted-user>"),
        _ => "<redacted-user>".to_string(),
    };

    let home_back = home.display().to_string();
    let home_fwd = home_back.replace('\\', "/");
    if home_back.is_empty() {
        return text.to_string();
    }

    // Replace the full home path (handles multi-word account names) in both the
    // native back-slash form and the normalized forward-slash form.
    let mut out = text.replace(&home_back, &replacement);
    if home_fwd != home_back {
        out = out.replace(&home_fwd, &replacement);
    }
    out
}

fn clean_log_text(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn redacted_absolute_path(path: &str) -> String {
    let path = path.replace('\\', "/");

    if let Some(rest) = path.strip_prefix("C:/Users/") {
        if let Some((_, tail)) = rest.split_once('/') {
            return format!("C:/Users/<redacted-user>/{tail}");
        }
        return "C:/Users/<redacted-user>".to_string();
    }

    if let Some(rest) = path.strip_prefix("/Users/") {
        if let Some((_, tail)) = rest.split_once('/') {
            return format!("/Users/<redacted-user>/{tail}");
        }
        return "/Users/<redacted-user>".to_string();
    }

    if let Some(rest) = path.strip_prefix("/home/") {
        if let Some((_, tail)) = rest.split_once('/') {
            return format!("/home/<redacted-user>/{tail}");
        }
        return "/home/<redacted-user>".to_string();
    }

    path
}

fn looks_absolute_path(path: &str) -> bool {
    Path::new(path).is_absolute()
        || path.starts_with("\\\\")
        || path.starts_with("//")
        || path.as_bytes().get(..3).is_some_and(|prefix| {
            prefix[0].is_ascii_alphabetic()
                && prefix[1] == b':'
                && matches!(prefix[2], b'\\' | b'/')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitize_log_path_str ───────────────────────────────────────────

    #[test]
    fn sanitize_log_path_str_no_home_prefix_returns_original() {
        let result = sanitize_log_path_str("some/other/path");
        assert_eq!(result, "some/other/path");
    }

    #[test]
    fn sanitize_log_path_str_empty_returns_empty() {
        assert_eq!(sanitize_log_path_str(""), "");
    }

    #[test]
    fn sanitize_log_path_returns_tilde_for_home_subpath() {
        if let Some(home) = dirs::home_dir() {
            let test_path = home.join("some").join("file.txt");
            let result = sanitize_log_path(&test_path);
            assert!(
                result.starts_with('~'),
                "Expected tilde prefix, got: {}",
                result
            );
            assert!(result.contains("some"));
            assert!(result.contains("file.txt"));
        }
    }

    #[test]
    fn sanitize_log_path_non_home_path_redacts_root() {
        #[cfg(windows)]
        let path = std::path::Path::new("D:\\tmp\\random\\test.log");
        #[cfg(not(windows))]
        let path = std::path::Path::new("/tmp/random/test.log");
        let result = sanitize_log_path(path);
        if let Some(home) = dirs::home_dir() {
            if !path.starts_with(&home) {
                #[cfg(windows)]
                assert_eq!(result, "D:/tmp/random/test.log");
                #[cfg(not(windows))]
                assert_eq!(result, "/tmp/random/test.log");
            }
        } else {
            #[cfg(windows)]
            assert_eq!(result, "D:/tmp/random/test.log");
            #[cfg(not(windows))]
            assert_eq!(result, "/tmp/random/test.log");
        }
    }

    #[test]
    fn sanitize_log_path_str_preserves_forward_slash_variant() {
        if let Some(home) = dirs::home_dir() {
            let home_fwd = home.display().to_string().replace('\\', "/");
            let test_path = format!("{}/deep/nested/file.log", home_fwd);
            let result = sanitize_log_path_str(&test_path);
            assert!(
                result.starts_with('~'),
                "Expected tilde prefix, got: {}",
                result
            );
            assert!(result.contains("deep"));
        }
    }

    #[test]
    fn sanitize_log_url_keeps_repository_urls_intact() {
        // Repository URL anonymization is disabled: the URL is returned as-is.
        assert_eq!(
            sanitize_log_url("https://user:token@example.test/private/repo?sig=abc#frag"),
            "https://user:token@example.test/private/repo?sig=abc#frag"
        );
    }

    #[test]
    fn redact_log_text_keeps_urls_but_redacts_home_account_name() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let account = match home.file_name().and_then(|name| name.to_str()) {
            Some(account) => account.to_string(),
            None => return,
        };

        let mod_path = home.join("Mods").join("@ace");
        let text = format!(
            "path={} url=https://token@example.test/a?sig=1",
            mod_path.display()
        );
        let redacted = redact_log_text(&text);

        // The account name is removed, with the path structure preserved.
        assert!(redacted.contains("<redacted-user>"));
        assert!(redacted.contains("@ace"));
        assert!(!redacted.contains(&account));
        // Repository / generic URLs are left intact.
        assert!(redacted.contains("url=https://token@example.test/a?sig=1"));
    }
}
