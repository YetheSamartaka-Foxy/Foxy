#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RecheckLevel {
    DEFAULT = 0,

    REPOSITORY = 1,

    MOD = 2,

    FILE = 3,

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
