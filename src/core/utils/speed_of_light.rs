//! Speed-of-light (SoL) performance accounting.
//!
//! Canonical math and log-line grammar for the `SOL op=...` lines defined in
//! `conventions/SPEED_OF_LIGHT.md`. Every crucial operation emits one SOL line
//! at info level so efficiency ratios can be recomputed from log files alone.
//!
//! Core equations (see the convention doc for derivations):
//! - `T_ideal = W / R_light` (E1)
//! - `sol_raw = T_ideal / T_actual = R_actual / R_light` (E2); the legacy
//!   `sol` field is `sol_raw` clamped to `[0, 1]`.

use std::time::Duration;

/// Grammar revision of the line this module renders. Bump when keys are
/// appended so consumers can tell which fields a line is guaranteed to carry.
pub(crate) const METRIC_VERSION: u32 = 2;

/// Where the reference rate ("light") for an SoL ratio came from.
pub(crate) enum SolLight {
    /// User-configured bandwidth cap in bytes/sec; a policy ceiling.
    LimiterCap(u64),
    /// Best demonstrated sampler-window throughput within the same run,
    /// in bytes/sec. Compares the run against its own peak, not physics.
    PeakSample(u64),
    /// No absolute reference is available; the rate is tracked against the
    /// best previously recorded run for the same machine instead.
    SelfBaseline,
}

/// What kind of comparison a ratio answers (convention section 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SolMetricKind {
    /// `T_lb / T_actual` against a modeled or policy bound.
    ModeledBound,
    /// `R_average / R_peak_window` of the same run.
    PeakConsistency,
    /// No valid reference; only the actual time is meaningful.
    None,
}

impl SolMetricKind {
    pub(crate) fn slug(self) -> &'static str {
        match self {
            SolMetricKind::ModeledBound => "modeled_bound",
            SolMetricKind::PeakConsistency => "peak_consistency",
            SolMetricKind::None => "none",
        }
    }
}

/// Whether the reference produced a usable ratio, and why not otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SolReferenceStatus {
    Ok,
    /// No light rate is knowable for this line.
    Missing,
    /// The raw ratio exceeds one: the reference is not a lower bound for
    /// this run, or the timer scopes differ.
    AboveBound,
    /// The actual duration is zero or non-finite; no ratio is computable.
    InvalidActual,
}

impl SolReferenceStatus {
    pub(crate) fn slug(self) -> &'static str {
        match self {
            SolReferenceStatus::Ok => "ok",
            SolReferenceStatus::Missing => "missing",
            SolReferenceStatus::AboveBound => "above_bound",
            SolReferenceStatus::InvalidActual => "invalid_actual",
        }
    }
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

    pub(crate) fn metric_kind(&self) -> SolMetricKind {
        match self {
            SolLight::LimiterCap(bps) | SolLight::PeakSample(bps) if *bps == 0 => {
                SolMetricKind::None
            }
            SolLight::LimiterCap(_) => SolMetricKind::ModeledBound,
            SolLight::PeakSample(_) => SolMetricKind::PeakConsistency,
            SolLight::SelfBaseline => SolMetricKind::None,
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

/// E2 without clamping: `T_ideal / T_actual`, finite and nonnegative.
/// Returns `None` when either input is non-finite or negative, or the actual
/// duration is not positive. A result above one is evidence about the
/// reference, so it is kept rather than folded into `1.0`.
pub(crate) fn sol_ratio_raw(ideal_secs: f64, actual_secs: f64) -> Option<f64> {
    if !ideal_secs.is_finite() || !actual_secs.is_finite() {
        return None;
    }
    if actual_secs <= 0.0 || ideal_secs < 0.0 {
        return None;
    }
    let ratio = ideal_secs / actual_secs;
    ratio.is_finite().then_some(ratio)
}

/// E2 legacy form: `sol_ratio_raw` clamped to `[0, 1]`.
pub(crate) fn sol_ratio(ideal_secs: f64, actual_secs: f64) -> Option<f64> {
    sol_ratio_raw(ideal_secs, actual_secs).map(|ratio| ratio.clamp(0.0, 1.0))
}

/// Per-line derived numbers, so callers can assert on the arithmetic rather
/// than on the rendered text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SolFigures {
    pub(crate) actual_secs: f64,
    pub(crate) actual_ns: u128,
    pub(crate) actual_bps: Option<u64>,
    pub(crate) light_bps: Option<u64>,
    pub(crate) ideal_secs: Option<f64>,
    pub(crate) sol_raw: Option<f64>,
    pub(crate) sol: Option<f64>,
    pub(crate) metric_kind: SolMetricKind,
    pub(crate) reference_status: SolReferenceStatus,
}

pub(crate) fn sol_figures(work_bytes: u64, actual: Duration, light: &SolLight) -> SolFigures {
    let actual_secs = actual.as_secs_f64();
    let actual_bps = (work_bytes > 0 && actual_secs > 0.0)
        .then(|| (work_bytes as f64 / actual_secs).round() as u64);
    let light_bps = light.bytes_per_sec();
    let ideal_secs = light_bps.and_then(|bps| ideal_seconds(work_bytes, bps));
    let sol_raw = ideal_secs.and_then(|ideal| sol_ratio_raw(ideal, actual_secs));
    let reference_status = match (light_bps, sol_raw) {
        (None, _) => SolReferenceStatus::Missing,
        (Some(_), None) => SolReferenceStatus::InvalidActual,
        (Some(_), Some(ratio)) if ratio > 1.0 => SolReferenceStatus::AboveBound,
        (Some(_), Some(_)) => SolReferenceStatus::Ok,
    };
    SolFigures {
        actual_secs,
        actual_ns: actual.as_nanos(),
        actual_bps,
        light_bps,
        ideal_secs,
        sol_raw,
        sol: ideal_secs.and_then(|ideal| sol_ratio(ideal, actual_secs)),
        metric_kind: light.metric_kind(),
        reference_status,
    }
}

/// The `op_id` extra for a line owned by an action, or nothing when the
/// emitter runs outside any action (a bare CLI hash run, a unit test).
pub(crate) fn op_id_extra(operation_id: Option<&str>) -> Option<(&'static str, String)> {
    operation_id
        .filter(|id| !id.is_empty())
        .map(|id| ("op_id", id.to_string()))
}

/// Render the canonical SOL log line.
///
/// Grammar (stable; parsers depend on it - only append new keys, never rename):
/// `SOL op=<op> actual_s=<secs> [work_bytes=<n> actual_bps=<n>]
///  [light_bps=<n> ideal_s=<secs>] sol=<ratio|na> light_src=<src>
///  actual_ns=<n> sol_raw=<ratio|na> metric_kind=<kind>
///  reference_status=<status> metric_version=<n>[ k=v ...]`
///
/// `work_bytes`/`actual_bps` are omitted for operations whose work is not
/// byte-denominated (pass `work_bytes = 0` and carry counts in `extras`).
/// `sol` stays clamped for legacy readers; `sol_raw` keeps ratios above one
/// and `actual_ns` keeps sub-millisecond durations that `actual_s` rounds away.
pub(crate) fn sol_line(
    op: &str,
    work_bytes: u64,
    actual: Duration,
    light: &SolLight,
    extras: &[(&str, String)],
) -> String {
    let figures = sol_figures(work_bytes, actual, light);
    let mut line = format!("SOL op={} actual_s={:.3}", op, figures.actual_secs);

    if work_bytes > 0 {
        line.push_str(&format!(
            " work_bytes={} actual_bps={}",
            work_bytes,
            figures.actual_bps.unwrap_or(0)
        ));
    }
    if let (Some(light_bps), Some(ideal)) = (figures.light_bps, figures.ideal_secs) {
        line.push_str(&format!(" light_bps={} ideal_s={:.3}", light_bps, ideal));
    }
    match figures.sol {
        Some(value) => line.push_str(&format!(" sol={:.3}", value)),
        None => line.push_str(" sol=na"),
    }
    line.push_str(&format!(" light_src={}", light.source_label()));
    line.push_str(&format!(" actual_ns={}", figures.actual_ns));
    match figures.sol_raw {
        Some(value) => line.push_str(&format!(" sol_raw={:.4}", value)),
        None => line.push_str(" sol_raw=na"),
    }
    line.push_str(&format!(
        " metric_kind={} reference_status={} metric_version={}",
        figures.metric_kind.slug(),
        figures.reference_status.slug(),
        METRIC_VERSION
    ));

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
    fn sol_ratio_clamps_to_one_but_raw_keeps_the_excess() {
        assert_eq!(sol_ratio(5.0, 4.0), Some(1.0));
        assert_eq!(sol_ratio_raw(5.0, 4.0), Some(1.25));
    }

    #[test]
    fn sol_ratio_degenerate_actual_is_none() {
        assert_eq!(sol_ratio(2.0, 0.0), None);
        assert_eq!(sol_ratio(2.0, -1.0), None);
        assert_eq!(sol_ratio_raw(2.0, 0.0), None);
    }

    #[test]
    fn sol_ratio_rejects_non_finite_and_negative_inputs() {
        assert_eq!(sol_ratio_raw(f64::NAN, 1.0), None);
        assert_eq!(sol_ratio_raw(1.0, f64::NAN), None);
        assert_eq!(sol_ratio_raw(f64::INFINITY, 1.0), None);
        assert_eq!(sol_ratio_raw(1.0, f64::INFINITY), None);
        assert_eq!(sol_ratio_raw(-1.0, 1.0), None);
        assert_eq!(sol_ratio(f64::NAN, 1.0), None);
    }

    #[test]
    fn zero_work_against_a_light_is_a_zero_ratio_not_na() {
        // Zero bytes have a zero ideal; the ratio is 0 and the status is ok.
        // Callers with zero-work operations should use a latency model or
        // `SelfBaseline` instead of pretending this is throughput.
        let figures = sol_figures(0, Duration::from_secs(1), &SolLight::LimiterCap(10));
        assert_eq!(figures.sol_raw, Some(0.0));
        assert_eq!(figures.reference_status, SolReferenceStatus::Ok);
        assert_eq!(figures.actual_bps, None);
    }

    #[test]
    fn figures_keep_sub_millisecond_durations() {
        let figures = sol_figures(1000, Duration::from_micros(250), &SolLight::SelfBaseline);
        assert_eq!(figures.actual_ns, 250_000);
        assert_eq!(figures.actual_bps, Some(4_000_000));
        let line = sol_line(
            "hash",
            1000,
            Duration::from_micros(250),
            &SolLight::SelfBaseline,
            &[],
        );
        assert!(line.contains("actual_s=0.000"));
        assert!(line.contains("actual_ns=250000"));
        assert!(line.contains("actual_bps=4000000"));
    }

    #[test]
    fn figures_flag_a_ratio_above_one_as_above_bound() {
        let figures = sol_figures(2000, Duration::from_secs(1), &SolLight::LimiterCap(1000));
        assert_eq!(figures.sol_raw, Some(2.0));
        assert_eq!(figures.sol, Some(1.0));
        assert_eq!(figures.reference_status, SolReferenceStatus::AboveBound);
        assert_eq!(figures.metric_kind, SolMetricKind::ModeledBound);
    }

    #[test]
    fn figures_report_invalid_actual_and_missing_reference() {
        let zero = sol_figures(10, Duration::ZERO, &SolLight::PeakSample(5));
        assert_eq!(zero.sol_raw, None);
        assert_eq!(zero.reference_status, SolReferenceStatus::InvalidActual);
        assert_eq!(zero.metric_kind, SolMetricKind::PeakConsistency);
        let none = sol_figures(10, Duration::from_secs(1), &SolLight::SelfBaseline);
        assert_eq!(none.reference_status, SolReferenceStatus::Missing);
        assert_eq!(none.metric_kind, SolMetricKind::None);
        let zero_cap = sol_figures(10, Duration::from_secs(1), &SolLight::LimiterCap(0));
        assert_eq!(zero_cap.reference_status, SolReferenceStatus::Missing);
        assert_eq!(zero_cap.metric_kind, SolMetricKind::None);
    }

    #[test]
    fn unit_conversion_from_megabits_is_decimal() {
        // 8 Mbps = 1,000,000 bytes/s; 10 MB in 10 s at that cap is the light.
        let cap = 8_u64 * 125_000;
        let figures = sol_figures(
            10_000_000,
            Duration::from_secs(10),
            &SolLight::LimiterCap(cap),
        );
        assert_eq!(figures.light_bps, Some(1_000_000));
        assert_eq!(figures.ideal_secs, Some(10.0));
        assert_eq!(figures.sol_raw, Some(1.0));
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
             light_bps=10485760 ideal_s=10.000 sol=0.500 light_src=limiter_cap \
             actual_ns=20000000000 sol_raw=0.5000 metric_kind=modeled_bound \
             reference_status=ok metric_version=2 files=3"
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
        assert!(line.contains("metric_kind=peak_consistency"));
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
            "SOL op=quick_scan actual_s=1.500 sol=na light_src=self_baseline \
             actual_ns=1500000000 sol_raw=na metric_kind=none reference_status=missing \
             metric_version=2 addons_total=12"
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
        assert!(line.contains("sol_raw=na"));
        assert!(!line.contains("light_bps"));
        assert!(line.contains("reference_status=missing"));
    }

    #[test]
    fn sol_line_keeps_the_raw_ratio_above_one() {
        let line = sol_line(
            "download",
            2000,
            Duration::from_secs(1),
            &SolLight::LimiterCap(1000),
            &[],
        );
        assert!(line.contains(" sol=1.000 "));
        assert!(line.contains(" sol_raw=2.0000 "));
        assert!(line.contains("reference_status=above_bound"));
    }

    #[test]
    fn rendered_values_recompute_within_declared_rounding() {
        let work = 4_331_121_846_u64;
        let actual = Duration::from_millis(39_904);
        let light = SolLight::PeakSample(118_513_659);
        let figures = sol_figures(work, actual, &light);
        let line = sol_line("download", work, actual, &light, &[]);
        let value = |key: &str| -> f64 {
            line.split_whitespace()
                .find_map(|token| token.strip_prefix(&format!("{key}=")))
                .unwrap()
                .parse()
                .unwrap()
        };
        assert!((value("ideal_s") - figures.ideal_secs.unwrap()).abs() < 0.0005);
        assert!((value("sol_raw") - figures.sol_raw.unwrap()).abs() < 0.00005);
        assert!((value("actual_bps") - work as f64 / 39.904).abs() < 1.0);
        assert_eq!(value("actual_ns"), 39_904_000_000.0);
    }
}
