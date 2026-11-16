use std::sync::LazyLock;

#[cfg(test)]
mod changelog;

pub const VERSION: &str = env!("FOXY_VERSION");

pub const GIT_HASH: &str = env!("FOXY_GIT_HASH");

pub fn is_dev_build() -> bool {
    cfg!(debug_assertions)
}

pub fn is_prerelease_build() -> bool {
    !cfg!(debug_assertions) && cfg!(feature = "prerelease")
}

pub fn is_official_build() -> bool {
    !cfg!(debug_assertions) && !cfg!(feature = "prerelease")
}

pub fn build_kind() -> &'static str {
    if is_dev_build() {
        "dev"
    } else if is_prerelease_build() {
        "prerelease"
    } else {
        "release"
    }
}

pub fn version_label() -> String {
    if is_dev_build() {
        format!("v{VERSION}-dev ({GIT_HASH})")
    } else if is_prerelease_build() {
        format!("v{VERSION}-pre ({GIT_HASH})")
    } else {
        format!("v{VERSION} ({GIT_HASH})")
    }
}

/// Use a semver pre-release so the matching stable release is offered as an update, while older releases are not.
pub fn update_check_version() -> &'static str {
    static VERSION_FOR_UPDATES: LazyLock<String> =
        LazyLock::new(|| update_check_version_for(VERSION, is_prerelease_build()));
    VERSION_FOR_UPDATES.as_str()
}

fn update_check_version_for(version: &str, prerelease: bool) -> String {
    if prerelease {
        format!("{version}-pre")
    } else {
        version.to_string()
    }
}

pub fn clap_version() -> &'static str {
    static LABEL: LazyLock<String> = LazyLock::new(|| {
        version_label()
            .strip_prefix('v')
            .map(str::to_string)
            .unwrap_or_else(version_label)
    });
    LABEL.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_latest_changelog_release() {
        assert_eq!(
            changelog::latest_version(include_str!("../CHANGELOG.md")),
            Ok(VERSION)
        );
    }

    #[test]
    fn version_label_always_carries_commit() {
        let label = version_label();
        assert!(label.starts_with(&format!("v{VERSION}")));
        assert!(label.ends_with(&format!("({GIT_HASH})")));
    }

    #[test]
    fn prerelease_update_version_sorts_just_below_its_release() {
        let pre = semver::Version::parse(&update_check_version_for("1.2.2", true)).unwrap();
        assert!(pre > semver::Version::parse("1.2.1").unwrap());
        assert!(pre < semver::Version::parse("1.2.2").unwrap());
        assert_eq!(update_check_version_for("1.2.2", false), "1.2.2");
    }

    #[test]
    fn update_check_version_is_valid_semver_for_this_build() {
        let version = semver::Version::parse(update_check_version()).unwrap();
        assert_eq!(version.pre.is_empty(), !is_prerelease_build());
    }

    #[test]
    fn clap_version_drops_v_prefix() {
        assert_eq!(format!("v{}", clap_version()), version_label());
    }
}
