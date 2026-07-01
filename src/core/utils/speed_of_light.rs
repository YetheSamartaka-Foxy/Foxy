//! Speed-of-light (SoL) performance accounting.
//!
//! Canonical math and log-line grammar for the `SOL op=...` lines defined in
//! `conventions/SPEED_OF_LIGHT.md`. Every crucial operation emits one SOL line
//! at info level so efficiency ratios can be recomputed from log files alone.
//!
//! Core equations (see the convention doc for derivations):
//! - `T_ideal = W / R_light` (E1)
//! - `sol = T_ideal / T_actual = R_actual / R_light`, clamped to `[0, 1]` (E2)

use std::time::Duration;

/// Where the reference rate ("light") for an SoL ratio came from.
pub(crate) enum SolLight {
    /// User-configured bandwidth cap in bytes/sec; an exact ceiling.
    LimiterCap(u64),
    /// Best demonstrated 1-second throughput sample within the same run,
    /// in bytes/sec. Acts as the proven capacity of the whole path.
    PeakSample(u64),
    /// No absolute reference is available; the rate is tracked against the
    /// best previously recorded run for the same machine instead.
    SelfBaseline,
}

impl SolLight {
    fn bytes_per_sec(&self) -> Option<u64> {
        match self {
            SolLight::LimiterCap(bps) | SolLight::PeakSample(bps) if *bps > 0 => Some(*bps),
            _ => None,
        }
    }

    fn source_label(&self) -> &'static str {
        match self {
            SolLight::LimiterCap(_) => "limiter_cap",
            SolLight::PeakSample(_) => "peak_1s",
            SolLight::SelfBaseline => "self_baseline",
        }
    }
}

/// E1: ideal duration in seconds for `work_bytes` at the light rate.
/// Returns `None` when the light rate is zero (no meaningful ideal).
pub(crate) fn ideal_seconds(work_bytes: u64, light_bytes_per_sec: u64) -> Option<f64> {
    if light_bytes_per_sec == 0 {
        return None;
    }
    Some(work_bytes as f64 / light_bytes_per_sec as f64)
}

/// E2: SoL ratio `T_ideal / T_actual`, clamped to `[0, 1]`.
/// Returns `None` when the actual duration is not positive.
pub(crate) fn sol_ratio(ideal_secs: f64, actual_secs: f64) -> Option<f64> {
    if actual_secs <= 0.0 || !ideal_secs.is_finite() || ideal_secs < 0.0 {
        return None;
    }
    Some((ideal_secs / actual_secs).clamp(0.0, 1.0))
}

/// Render the canonical SOL log line.
///
/// Grammar (stable; parsers depend on it - only append new keys, never rename):
/// `SOL op=<op> actual_s=<secs> [work_bytes=<n> actual_bps=<n>]
///  [light_bps=<n> ideal_s=<secs>] sol=<ratio|na> light_src=<src>[ k=v ...]`
///
/// `work_bytes`/`actual_bps` are omitted for operations whose work is not
/// byte-denominated (pass `work_bytes = 0` and carry counts in `extras`).
pub(crate) fn sol_line(
    op: &str,
    work_bytes: u64,
    actual: Duration,
    light: &SolLight,
    extras: &[(&str, String)],
) -> String {
    let actual_secs = actual.as_secs_f64();
    let mut line = format!("SOL op={} actual_s={:.3}", op, actual_secs);

    if work_bytes > 0 {
        let actual_bps = if actual_secs > 0.0 {
            (work_bytes as f64 / actual_secs).round() as u64
        } else {
            0
        };
        line.push_str(&format!(
            " work_bytes={} actual_bps={}",
            work_bytes, actual_bps
        ));
    }

    let ratio = light.bytes_per_sec().and_then(|light_bps| {
        let ideal = ideal_seconds(work_bytes, light_bps)?;
        line.push_str(&format!(" light_bps={} ideal_s={:.3}", light_bps, ideal));
        sol_ratio(ideal, actual_secs)
    });
    match ratio {
        Some(value) => line.push_str(&format!(" sol={:.3}", value)),
        None => line.push_str(" sol=na"),
    }
    line.push_str(&format!(" light_src={}", light.source_label()));

    for (key, value) in extras {
        line.push_str(&format!(" {}={}", key, value));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ideal_seconds_divides_work_by_light() {
        assert_eq!(ideal_seconds(100, 50), Some(2.0));
        assert_eq!(ideal_seconds(0, 50), Some(0.0));
    }

    #[test]
    fn ideal_seconds_zero_light_is_none() {
        assert_eq!(ideal_seconds(100, 0), None);
    }

    #[test]
    fn sol_ratio_is_ideal_over_actual() {
        assert_eq!(sol_ratio(2.0, 4.0), Some(0.5));
        assert_eq!(sol_ratio(4.0, 4.0), Some(1.0));
    }

    #[test]
    fn sol_ratio_clamps_to_one() {
        assert_eq!(sol_ratio(5.0, 4.0), Some(1.0));
    }

    #[test]
    fn sol_ratio_degenerate_actual_is_none() {
        assert_eq!(sol_ratio(2.0, 0.0), None);
        assert_eq!(sol_ratio(2.0, -1.0), None);
    }

    #[test]
    fn sol_line_with_limiter_light_has_full_grammar() {
        let line = sol_line(
            "download",
            100 * 1024 * 1024,
            Duration::from_secs(20),
            &SolLight::LimiterCap(10 * 1024 * 1024),
            &[("files", "3".to_string())],
        );
        assert_eq!(
            line,
            "SOL op=download actual_s=20.000 work_bytes=104857600 actual_bps=5242880 \
             light_bps=10485760 ideal_s=10.000 sol=0.500 light_src=limiter_cap files=3"
        );
    }

    #[test]
    fn sol_line_peak_sample_light_source() {
        let line = sol_line(
            "download",
            1000,
            Duration::from_secs(1),
            &SolLight::PeakSample(2000),
            &[],
        );
        assert!(line.contains("light_src=peak_1s"));
        assert!(line.contains("sol=0.500"));
    }

    #[test]
    fn sol_line_self_baseline_omits_light_fields() {
        let line = sol_line(
            "quick_scan",
            0,
            Duration::from_millis(1500),
            &SolLight::SelfBaseline,
            &[("addons_total", "12".to_string())],
        );
        assert_eq!(
            line,
            "SOL op=quick_scan actual_s=1.500 sol=na light_src=self_baseline addons_total=12"
        );
        assert!(!line.contains("work_bytes"));
        assert!(!line.contains("light_bps"));
    }

    #[test]
    fn sol_line_zero_light_cap_falls_back_to_na() {
        let line = sol_line(
            "download",
            1000,
            Duration::from_secs(1),
            &SolLight::LimiterCap(0),
            &[],
        );
        assert!(line.contains("sol=na"));
        assert!(!line.contains("light_bps"));
    }
}
