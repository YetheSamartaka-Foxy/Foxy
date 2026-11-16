use std::sync::LazyLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const GIT_HASH: &str = env!("FOXY_GIT_HASH");

pub fn version_label() -> String {
    if cfg!(debug_assertions) {
        format!("{VERSION}-dev ({GIT_HASH})")
    } else {
        format!("{VERSION} ({GIT_HASH})")
    }
}

pub fn clap_version() -> &'static str {
    static LABEL: LazyLock<String> = LazyLock::new(version_label);
    LABEL.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_label_carries_version_and_commit() {
        let label = version_label();
        assert!(label.starts_with(VERSION));
        assert!(label.ends_with(&format!("({GIT_HASH})")));
        assert_eq!(clap_version(), label);
    }
}
