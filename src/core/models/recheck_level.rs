/// Represents the levels of tree-pruning via checksums to be ignored during a forced recheck.
///
/// This enum defines various levels at which checksums are bypassed during a
/// forced recheck operation, ranging from rechecking every file part
/// (`FILE_PART`) down to default checks (`DEFAULT`).
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RecheckLevel {
    /// Default recheck behaviour - local checksums that match remote checksums are ignored.
    DEFAULT = 0,
    /// Force Recheck of repository at minimum
    REPOSITORY = 1,
    /// Force Recheck of repository and all mods at minimum
    MOD = 2,
    /// Force Recheck of repository and all mods and all files at minimum
    FILE = 3,
    /// Force Recheck of repository and all mods and all files and all file parts at minimum
    FILE_PART = 4,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recheck_level_ordering_default_is_lowest() {
        assert!(RecheckLevel::DEFAULT < RecheckLevel::REPOSITORY);
        assert!(RecheckLevel::REPOSITORY < RecheckLevel::MOD);
        assert!(RecheckLevel::MOD < RecheckLevel::FILE);
        assert!(RecheckLevel::FILE < RecheckLevel::FILE_PART);
    }

    #[test]
    fn recheck_level_ge_comparison_for_thresholds() {
        // Typical usage: context.recheck_level >= RecheckLevel::FILE_PART
        assert!(RecheckLevel::FILE_PART >= RecheckLevel::FILE_PART);
        assert!(RecheckLevel::FILE < RecheckLevel::FILE_PART);
    }

    #[test]
    fn recheck_level_equality() {
        assert_eq!(RecheckLevel::DEFAULT, RecheckLevel::DEFAULT);
        assert_ne!(RecheckLevel::DEFAULT, RecheckLevel::FILE_PART);
    }

    #[test]
    fn recheck_level_copy_semantics() {
        let level = RecheckLevel::FILE;
        let copied = level;
        assert_eq!(level, copied);
    }

    #[test]
    fn recheck_level_repr_values() {
        assert_eq!(RecheckLevel::DEFAULT as u8, 0);
        assert_eq!(RecheckLevel::REPOSITORY as u8, 1);
        assert_eq!(RecheckLevel::MOD as u8, 2);
        assert_eq!(RecheckLevel::FILE as u8, 3);
        assert_eq!(RecheckLevel::FILE_PART as u8, 4);
    }
}
