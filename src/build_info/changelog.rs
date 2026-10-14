pub fn latest_version(changelog: &str) -> Result<&str, &'static str> {
    let heading = changelog
        .trim_start_matches('\u{feff}')
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .ok_or("CHANGELOG.md must start with a release heading: # MAJOR.MINOR.PATCH")?;

    let version = heading.trim();
    let components: Vec<_> = version.split('.').collect();
    if components.len() != 3
        || components.iter().any(|component| {
            component.is_empty()
                || !component.bytes().all(|byte| byte.is_ascii_digit())
                || (component.len() > 1 && component.starts_with('0'))
                || component.parse::<u64>().is_err()
        })
    {
        return Err("The first CHANGELOG.md release heading must be # MAJOR.MINOR.PATCH");
    }

    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_first_release() {
        assert_eq!(
            latest_version("# 2.0.0\n## Changed\n- Change\n# 1.9.9\n"),
            Ok("2.0.0")
        );
    }

    #[test]
    fn handles_utf8_bom_and_windows_line_endings() {
        assert_eq!(
            latest_version("\u{feff}# 1.2.1\r\n## Fixed\r\n"),
            Ok("1.2.1")
        );
    }

    #[test]
    fn rejects_missing_or_invalid_latest_release() {
        for changelog in [
            "",
            "## Fixed\n- Change",
            "# Unreleased\n# 1.2.0",
            "# 1.2\n# 1.2.0",
            "# 1.2.3.4",
            "# 1.02.3",
            "# 1.two.3",
            "# 1.2.3 extra",
            "# 18446744073709551616.0.0",
        ] {
            assert!(latest_version(changelog).is_err(), "{changelog:?}");
        }
    }
}
