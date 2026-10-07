pub const SUCCESS: i32 = 0;
pub const VALIDATION_ERROR: i32 = 2;
pub const NOT_FOUND: i32 = 3;
pub const OPERATION_FAILED: i32 = 4;
pub const PARTIAL_SUCCESS: i32 = 5;
/// Another Foxy process owns the game space's database. Distinct from
/// `OPERATION_FAILED` so scripts can tell "retry once the GUI is closed" apart
/// from a command that genuinely failed.
pub const DATABASE_BUSY: i32 = 6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_success_is_zero() {
        assert_eq!(SUCCESS, 0);
    }

    #[test]
    fn exit_codes_are_distinct() {
        let codes = [
            SUCCESS,
            VALIDATION_ERROR,
            NOT_FOUND,
            OPERATION_FAILED,
            PARTIAL_SUCCESS,
            DATABASE_BUSY,
        ];
        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(
                    codes[i], codes[j],
                    "Exit codes {} and {} must be distinct",
                    i, j
                );
            }
        }
    }

    #[test]
    fn exit_code_values_in_expected_range() {
        let codes = [
            SUCCESS,
            VALIDATION_ERROR,
            NOT_FOUND,
            OPERATION_FAILED,
            PARTIAL_SUCCESS,
            DATABASE_BUSY,
        ];
        for code in codes {
            assert!(code < 128, "Exit code {} should be less than 128", code);
        }
    }
}
