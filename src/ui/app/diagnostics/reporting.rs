use crate::ui::app::Foxy;
use crate::ui::i18n::fmt_bytes;

impl Foxy {
    pub(crate) fn format_bytes_short(bytes: u64) -> String {
        fmt_bytes(bytes)
    }

    pub(crate) fn format_optional_bytes(bytes: Option<u64>) -> String {
        bytes.map(fmt_bytes).unwrap_or_else(|| "n/a".to_string())
    }

    pub(crate) fn format_optional_count(value: Option<u64>) -> String {
        value
            .map(|count| count.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    }

    pub(crate) fn format_bytes_delta(delta: i64) -> String {
        if delta >= 0 {
            format!("+{}", fmt_bytes(delta as u64))
        } else {
            format!("-{}", fmt_bytes(delta.unsigned_abs()))
        }
    }

    pub(crate) fn format_optional_bytes_delta(
        current: Option<u64>,
        baseline: Option<u64>,
    ) -> String {
        match (current, baseline) {
            (Some(current), Some(baseline)) => {
                Self::format_bytes_delta(current as i64 - baseline as i64)
            }
            _ => "n/a".to_string(),
        }
    }
}
