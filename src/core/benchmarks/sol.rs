//! Speed-of-light summary of the `SOL op=...` lines a benchmark captured,
//! folded to one row per operation so a run that logged dozens of hash
//! batches still reads as a single ratio. See `conventions/SPEED_OF_LIGHT.md`.
//!
//! The folded `actual_s` is a service sum over the operation's lines, not the
//! action's wall time: batches that overlapped (hashing during a download)
//! add up to more than the action took. The record's `elapsed_ms` is the
//! makespan.

use std::collections::BTreeMap;

use super::record::{BenchmarkKind, BenchmarkRecord};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolLightSource {
    /// The user's bandwidth cap is the light (policy ceiling).
    LimiterCap,
    /// The best sampler window of the same run is the light.
    Peak1s,
    /// No absolute light is computable; the line carries `sol=na`.
    SelfBaseline,
    /// Derived here: the fastest batch of the operation within this
    /// benchmark is the light for the aggregate of all its batches.
    SameRunBest,
    /// The nominal sequential rate of a rotational disk (download `disk_*`).
    NominalHddSequential,
    Other(String),
}

impl SolLightSource {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("limiter_cap") => Self::LimiterCap,
            Some("peak_1s") => Self::Peak1s,
            Some("self_baseline") | None => Self::SelfBaseline,
            Some("nominal_hdd_sequential") => Self::NominalHddSequential,
            Some(other) => Self::Other(other.to_owned()),
        }
    }

    /// Translation key describing where the light came from.
    pub fn label(&self) -> &'static str {
        match self {
            Self::LimiterCap => "bandwidth limit",
            Self::Peak1s => "same-run peak window",
            Self::SelfBaseline => "no absolute light",
            Self::SameRunBest => "fastest batch of this run",
            Self::NominalHddSequential => "nominal HDD sequential rate",
            Self::Other(_) => "unknown light",
        }
    }

    /// The comparison a ratio against this light answers.
    pub fn metric_kind(&self) -> SolMetricKind {
        match self {
            Self::LimiterCap => SolMetricKind::ModeledBound,
            Self::Peak1s | Self::SameRunBest => SolMetricKind::PeakConsistency,
            Self::NominalHddSequential => SolMetricKind::Nominal,
            Self::SelfBaseline | Self::Other(_) => SolMetricKind::None,
        }
    }
}

/// What a displayed percentage means (convention section 2.1). A modeled
/// bound, a same-run peak and a nominal device rate are different questions
/// and must not be read as one "efficiency".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolMetricKind {
    /// `T_bound / T_actual` against a policy or modeled lower bound.
    ModeledBound,
    /// `R_average / R_peak` of the same run: consistency, not physics.
    PeakConsistency,
    /// Against a nominal device constant that was not calibrated here.
    Nominal,
    /// No valid reference; only the actual time is meaningful.
    None,
}

impl SolMetricKind {
    /// Translation key, short enough to sit beside the percentage.
    pub fn label(self) -> &'static str {
        match self {
            Self::ModeledBound => "vs bound",
            Self::PeakConsistency => "peak consistency",
            Self::Nominal => "vs nominal",
            Self::None => "reference missing",
        }
    }

    /// Translation key for the long explanation.
    pub fn description(self) -> &'static str {
        match self {
            Self::ModeledBound => {
                "Ratio of the modeled lower-bound time to the actual time. Above 100% means the bound was not a bound for this run."
            }
            Self::PeakConsistency => {
                "Average rate against the best window of the same run. It shows whether the run held its own peak, not how close it came to physics."
            }
            Self::Nominal => {
                "Ratio against a nominal device rate that was not calibrated on this machine. A reading aid, not a bound."
            }
            Self::None => {
                "No valid reference exists for this operation; compare the actual time with the best measured run instead."
            }
        }
    }
}

/// A second resource the same operation was judged against, with its own
/// light: the destination disk of a download on rotational storage.
#[derive(Clone, Debug, PartialEq)]
pub struct SolSubPart {
    /// Translation key naming the resource.
    pub name: &'static str,
    pub work_bytes: u64,
    pub light_bps: Option<f64>,
    pub ideal_s: Option<f64>,
    /// Clamped to `[0, 1]`.
    pub sol: Option<f64>,
    /// Unclamped; above one means the reference was not a bound.
    pub sol_raw: Option<f64>,
    pub light_src: SolLightSource,
    pub reference_status: String,
}

impl SolSubPart {
    pub fn headroom(&self) -> Option<f64> {
        self.sol.filter(|sol| *sol > 0.0).map(|sol| 1.0 / sol)
    }

    pub fn metric_kind(&self) -> SolMetricKind {
        if self.sol.is_none() {
            SolMetricKind::None
        } else {
            self.light_src.metric_kind()
        }
    }

    pub fn display_sol(&self) -> Option<f64> {
        (self.reference_status != "above_bound"
            && self.reference_status != "invalid_actual"
            && self.reference_status != "invalid_reference"
            && self.sol_raw.is_none_or(|raw| raw <= 1.0))
        .then_some(self.sol)
        .flatten()
    }
}

/// A category-specific figure of an operation, folded over its runs.
#[derive(Clone, Debug, PartialEq)]
pub enum SolDetailValue {
    Count(f64),
    Secs(f64),
    BytesPerSec(f64),
    Percent(f64),
    /// A per-second rate with its translation-key unit (`addons`).
    Rate(f64, &'static str),
    /// Distinct raw values, comma separated.
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SolDetail {
    /// Translation key.
    pub label: &'static str,
    pub value: SolDetailValue,
}

/// One operation of a benchmark, summed over every `SOL` line it logged.
#[derive(Clone, Debug, PartialEq)]
pub struct SolOpSummary {
    pub op: String,
    pub runs: usize,
    /// Lines that carried a usable ratio; below `runs` means the summary's
    /// ratio describes only part of the operation's work.
    pub rated_runs: usize,
    /// Service sum of the lines' `actual_s`, not the action makespan.
    pub actual_s: f64,
    pub interval_coverage_s: Option<f64>,
    pub makespan_s: Option<f64>,
    pub useful_work_bytes: u64,
    pub work_bytes: u64,
    pub actual_bps: Option<f64>,
    pub light_bps: Option<f64>,
    pub ideal_s: Option<f64>,
    /// Service sum of the lines behind `ideal_s`, so the gap to the
    /// reference compares like with like when coverage is partial.
    pub rated_actual_s: Option<f64>,
    /// Ratio in `[0, 1]`; `None` when no light is knowable.
    pub sol: Option<f64>,
    /// Unclamped ratio; above one is evidence about the reference.
    pub sol_raw: Option<f64>,
    pub light_src: SolLightSource,
    /// The lines disagreed on where their light came from; the ratio mixes
    /// reference kinds and should not be read as one number.
    pub mixed_references: bool,
    /// Distinct `label`/`profile` values differed across lines, so the
    /// batches are not comparable work and no fastest-batch light is derived.
    pub heterogeneous: bool,
    pub reference_statuses: Vec<String>,
    pub metric_versions: Vec<u32>,
    pub mixed_metric_versions: bool,
    pub malformed_lines: usize,
    /// Distinct `outcome` values the lines reported, first-seen order.
    pub outcomes: Vec<String>,
    pub sub_parts: Vec<SolSubPart>,
    pub details: Vec<SolDetail>,
}

impl SolOpSummary {
    /// How many times faster the operation could run before its reference
    /// objects (E3 of the convention): a distance to the named reference,
    /// not a promise of achievable speedup.
    pub fn headroom(&self) -> Option<f64> {
        self.sol.filter(|sol| *sol > 0.0).map(|sol| 1.0 / sol)
    }

    pub fn metric_kind(&self) -> SolMetricKind {
        if self.sol.is_none() {
            SolMetricKind::None
        } else {
            self.light_src.metric_kind()
        }
    }

    /// `T_actual - T_reference` in seconds for the rated lines.
    pub fn gap_to_reference_s(&self) -> Option<f64> {
        let ideal = self.ideal_s?;
        (self.rated_runs > 0).then(|| self.rated_actual_s.unwrap_or(self.actual_s) - ideal)
    }

    /// Every line carried a ratio.
    pub fn full_coverage(&self) -> bool {
        self.runs > 0 && self.rated_runs == self.runs
    }

    /// No line reported a terminal outcome other than success.
    pub fn completed(&self) -> bool {
        self.outcomes
            .iter()
            .all(|outcome| outcome != "cancelled" && !outcome.starts_with("failed"))
    }

    /// Whether the ratio may stand as the action's headline: the reference is
    /// one kind, it covers every line, and the operation completed.
    pub fn headline_worthy(&self) -> bool {
        self.display_sol().is_some()
            && self.full_coverage()
            && !self.mixed_references
            && !self.mixed_metric_versions
            && self.malformed_lines == 0
            && self.completed()
    }

    pub fn display_sol(&self) -> Option<f64> {
        let valid_status = self.reference_statuses.iter().all(|status| status == "ok");
        ((self.reference_statuses.is_empty() || valid_status)
            && self.sol_raw.is_none_or(|raw| raw <= 1.0))
        .then_some(self.sol)
        .flatten()
    }

    pub fn unavailable_reason(&self) -> &'static str {
        if self.malformed_lines > 0 {
            "malformed data"
        } else if self.sol_raw.is_some_and(|raw| raw > 1.0)
            || self
                .reference_statuses
                .iter()
                .any(|status| status == "above_bound")
        {
            "above bound"
        } else if self
            .reference_statuses
            .iter()
            .any(|status| status.starts_with("invalid"))
        {
            "invalid reference"
        } else if self.mixed_metric_versions {
            "metric versions differ"
        } else if self.sol.is_none() {
            if self.heterogeneous {
                "batches differ"
            } else {
                "reference missing"
            }
        } else if !self.completed() {
            "not completed"
        } else if self.mixed_references {
            "mixed references"
        } else {
            "partial coverage"
        }
    }

    /// Crucial-operation number and name from `conventions/SPEED_OF_LIGHT.md`,
    /// `None` for an operation the convention does not list.
    pub fn category(&self) -> Option<(&'static str, &'static str)> {
        Some(match self.op.as_str() {
            "download" => ("O1", "Full-file download"),
            "delta_patch" => ("O2", "Delta patch"),
            "hash" => ("O3", "Tree hash verification"),
            "quick_scan" => ("O4", "Quick scan"),
            "remote_refresh" => ("O5", "Remote metadata refresh"),
            "startup" | "startup_probe" => ("O8", "Startup to first sync verdict"),
            "app_update_check" => ("O5", "App update check"),
            "db_persist" => ("O7", "Turso persistence"),
            "sync_action" => ("A", "Complete sync action"),
            "db_purge" => ("O7", "Turso purge"),
            "space_switch" => ("S", "Game space switch"),
            _ => return None,
        })
    }

    /// Translation key naming the resource the main ratio measures.
    pub fn main_part_name(&self) -> &'static str {
        match self.op.as_str() {
            "download" => "network",
            "hash" => "disk and CPU",
            "quick_scan" => "directory walk",
            "startup" | "startup_probe" | "remote_refresh" | "app_update_check" => "round trips",
            "db_persist" | "db_purge" => "writer",
            _ => "overall",
        }
    }
}

#[derive(Clone, Copy)]
enum Fold {
    Sum,
    Max,
    Distinct,
    /// `100 * sum(numerator) / sum(denominator)`; falls back to the
    /// work-weighted mean of the raw percentages on lines without the
    /// byte counters.
    PercentOf(&'static str, &'static str),
    /// `sum(key) / sum(actual_s)` over the lines carrying `key`.
    PerSecondOf(&'static str),
}

#[derive(Clone, Copy)]
enum Kind {
    Count,
    Secs,
    BytesPerSec,
    Percent,
    Rate(&'static str),
    Text,
}

/// Which extra keys of an operation's lines are worth a figure, and how
/// they fold across runs. Keys not listed stay in the raw lines.
fn detail_specs(op: &str) -> &'static [(&'static str, &'static str, Kind, Fold)] {
    match op {
        "download" => &[
            ("files", "Files", Kind::Count, Fold::Sum),
            ("peak_1s_bps", "Peak window", Kind::BytesPerSec, Fold::Max),
            (
                "delta_savings_percent",
                "Delta savings",
                Kind::Percent,
                Fold::PercentOf("delta_savings_bytes", "full_bytes"),
            ),
            ("range_retries", "Range retries", Kind::Count, Fold::Sum),
            (
                "destination_storage",
                "Destination storage",
                Kind::Text,
                Fold::Distinct,
            ),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "delta_patch" => &[
            ("attempts", "Attempts", Kind::Count, Fold::Sum),
            ("patched_files", "Patched files", Kind::Count, Fold::Sum),
            ("fallbacks", "Fallbacks", Kind::Count, Fold::Sum),
            ("cancellations", "Cancellations", Kind::Count, Fold::Sum),
            ("requests", "Requests", Kind::Count, Fold::Sum),
            ("retries", "Retries", Kind::Count, Fold::Sum),
            ("planning_s", "Planning", Kind::Secs, Fold::Sum),
            ("fetch_s", "Fetch", Kind::Secs, Fold::Sum),
            ("apply_s", "Apply", Kind::Secs, Fold::Sum),
            (
                "verify_promote_s",
                "Verify and promote",
                Kind::Secs,
                Fold::Sum,
            ),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "hash" => &[
            ("files", "Files", Kind::Count, Fold::Sum),
            ("parts", "Parts", Kind::Count, Fold::Sum),
            ("compute_s", "Blocking task time", Kind::Secs, Fold::Sum),
            ("wait_s", "Permit wait", Kind::Secs, Fold::Sum),
            ("missing_files", "Missing files", Kind::Count, Fold::Sum),
            ("label", "Profile", Kind::Text, Fold::Distinct),
        ],
        "quick_scan" => &[
            ("addons_total", "Addons", Kind::Count, Fold::Sum),
            ("addons_hashed", "Addons hashed", Kind::Count, Fold::Sum),
            (
                "cache_hits_shared",
                "Shared cache hits",
                Kind::Count,
                Fold::Sum,
            ),
            (
                "cache_hits_persistent",
                "Persistent cache hits",
                Kind::Count,
                Fold::Sum,
            ),
            (
                "deep_scan_files",
                "Deep-scanned files",
                Kind::Count,
                Fold::Sum,
            ),
            (
                "addons_per_s",
                "Scan rate",
                Kind::Rate("addons/s"),
                Fold::PerSecondOf("addons_total"),
            ),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "startup" => &[
            ("repos", "Repositories", Kind::Count, Fold::Sum),
            ("quick_scan_repos", "Quick-scanned", Kind::Count, Fold::Sum),
            ("eligible", "Eligible", Kind::Count, Fold::Sum),
            ("prevalidated", "Prevalidated", Kind::Count, Fold::Sum),
            ("remote_changed", "Remote changed", Kind::Count, Fold::Sum),
            ("rechecks", "Rechecks queued", Kind::Count, Fold::Sum),
            ("first_frame_s", "First frame", Kind::Secs, Fold::Max),
            ("dispatch_s", "Dispatch", Kind::Secs, Fold::Max),
            ("eligibility_s", "Eligibility", Kind::Secs, Fold::Max),
            ("verdict_s", "Verdict", Kind::Secs, Fold::Max),
            (
                "dependency_bound_s",
                "Dependency bound",
                Kind::Secs,
                Fold::Max,
            ),
            ("join_s", "Required join", Kind::Secs, Fold::Max),
            (
                "dependency_coverage_percent",
                "Dependency coverage",
                Kind::Percent,
                Fold::Max,
            ),
        ],
        "startup_probe" => &[
            ("repos", "Repositories", Kind::Count, Fold::Sum),
            ("answered", "Answered", Kind::Count, Fold::Sum),
            ("changed", "Changed", Kind::Count, Fold::Sum),
            ("unknown", "Unknown", Kind::Count, Fold::Sum),
        ],
        "app_update_check" => &[
            ("mode", "Mode", Kind::Text, Fold::Distinct),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "remote_refresh" => &[
            ("index_requests", "Index requests", Kind::Count, Fold::Sum),
            (
                "manifest_requests",
                "Manifest requests",
                Kind::Count,
                Fold::Sum,
            ),
            ("mods", "Mods", Kind::Count, Fold::Sum),
            ("files", "Files", Kind::Count, Fold::Sum),
            ("parts", "Parts", Kind::Count, Fold::Sum),
            ("fetch_sum_s", "Manifest fetch", Kind::Secs, Fold::Sum),
            ("parse_sum_s", "Manifest parse", Kind::Secs, Fold::Sum),
            ("persist_sum_s", "Persist", Kind::Secs, Fold::Sum),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "db_persist" => &[
            ("write_calls", "Write calls", Kind::Count, Fold::Sum),
            ("write_failed", "Failed writes", Kind::Count, Fold::Sum),
            ("lock_retries", "Lock retries", Kind::Count, Fold::Sum),
            ("categories", "Categories", Kind::Count, Fold::Max),
            ("write_gate", "Write gate", Kind::Count, Fold::Max),
            ("mode", "Mode", Kind::Text, Fold::Distinct),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "sync_action" => &[
            ("stages", "Stages", Kind::Count, Fold::Max),
            ("mode", "Mode", Kind::Text, Fold::Distinct),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "db_purge" => &[
            ("steps", "Statements", Kind::Count, Fold::Sum),
            ("rows_affected", "Rows affected", Kind::Count, Fold::Sum),
            ("txn_s", "Transaction", Kind::Secs, Fold::Sum),
            ("checkpoint_s", "Checkpoint", Kind::Secs, Fold::Sum),
            ("kind", "Kind", Kind::Text, Fold::Distinct),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        "space_switch" => &[
            ("drain_s", "Drain", Kind::Secs, Fold::Sum),
            ("reset_s", "Reset", Kind::Secs, Fold::Sum),
            ("reload_s", "Reload", Kind::Secs, Fold::Sum),
            ("repositories", "Repositories", Kind::Count, Fold::Max),
            ("outcome", "Outcome", Kind::Text, Fold::Distinct),
        ],
        _ => &[],
    }
}

fn distinct_values<'a>(maps: &[&'a BTreeMap<String, String>], key: &str) -> Vec<&'a str> {
    let mut distinct: Vec<&str> = Vec::new();
    for value in maps.iter().filter_map(|map| map.get(key)) {
        if !distinct.iter().any(|known| known == value) {
            distinct.push(value);
        }
    }
    distinct
}

fn sum_of(maps: &[&BTreeMap<String, String>], key: &str) -> Option<f64> {
    let values: Vec<f64> = maps.iter().filter_map(|map| number(map, key)).collect();
    (!values.is_empty()).then(|| values.iter().sum())
}

/// Work-weighted mean of a per-line ratio, for legacy lines that carry the
/// percentage but not the counters it was derived from.
fn weighted_mean(maps: &[&BTreeMap<String, String>], key: &str) -> Option<f64> {
    let mut total_weight = 0.0;
    let mut total = 0.0;
    for map in maps {
        let Some(value) = number(map, key) else {
            continue;
        };
        let weight = number(map, "work_bytes")
            .or_else(|| number(map, "full_bytes"))
            .map_or(1.0, |bytes| bytes.max(1.0));
        total_weight += weight;
        total += weight * value;
    }
    (total_weight > 0.0).then(|| total / total_weight)
}

fn fold_details(op: &str, maps: &[&BTreeMap<String, String>]) -> Vec<SolDetail> {
    let mut out = Vec::new();
    for (key, label, kind, fold) in detail_specs(op) {
        let present = maps.iter().any(|map| map.contains_key(*key));
        if !present {
            continue;
        }
        let value = match (kind, fold) {
            (Kind::Text, _) => SolDetailValue::Text(distinct_values(maps, key).join(", ")),
            (_, Fold::PercentOf(numerator, denominator)) => {
                let derived = match (sum_of(maps, numerator), sum_of(maps, denominator)) {
                    (Some(saved), Some(full)) if full > 0.0 => Some(100.0 * saved / full),
                    _ => weighted_mean(maps, key),
                };
                let Some(percent) = derived else { continue };
                SolDetailValue::Percent(percent)
            }
            (Kind::Rate(unit), Fold::PerSecondOf(count_key)) => {
                let counted: Vec<&&BTreeMap<String, String>> = maps
                    .iter()
                    .filter(|map| number(map, count_key).is_some())
                    .collect();
                let count: f64 = counted
                    .iter()
                    .filter_map(|map| number(map, count_key))
                    .sum();
                let secs: f64 = counted
                    .iter()
                    .filter_map(|map| number(map, "actual_s"))
                    .sum();
                let rate = if secs > 0.0 {
                    count / secs
                } else if let Some(mean) = weighted_mean(maps, key) {
                    mean
                } else {
                    continue;
                };
                SolDetailValue::Rate(rate, unit)
            }
            (_, Fold::PerSecondOf(_)) => continue,
            _ => {
                let numbers: Vec<f64> = maps.iter().filter_map(|map| number(map, key)).collect();
                if numbers.is_empty() {
                    continue;
                }
                let folded = match fold {
                    Fold::Sum => numbers.iter().sum(),
                    Fold::Max => numbers.iter().copied().fold(0.0_f64, f64::max),
                    Fold::Distinct => numbers[0],
                    Fold::PercentOf(..) | Fold::PerSecondOf(_) => unreachable!(),
                };
                match kind {
                    Kind::Count => SolDetailValue::Count(folded),
                    Kind::Secs => SolDetailValue::Secs(folded),
                    Kind::BytesPerSec => SolDetailValue::BytesPerSec(folded),
                    Kind::Percent => SolDetailValue::Percent(folded),
                    Kind::Rate(unit) => SolDetailValue::Rate(folded, unit),
                    Kind::Text => unreachable!(),
                }
            }
        };
        out.push(SolDetail { label, value });
    }
    out
}

/// The download's disk ratio, logged as `disk_*` keys on rotational
/// destinations; serial over runs like the main ratio.
fn disk_sub_part(maps: &[&BTreeMap<String, String>]) -> Option<SolSubPart> {
    let rated: Vec<&&BTreeMap<String, String>> = maps
        .iter()
        .filter(|map| number(map, "disk_sol").is_some())
        .collect();
    if rated.is_empty() {
        return None;
    }
    let work_bytes: u64 = rated
        .iter()
        .filter_map(|map| number(map, "disk_bytes"))
        .map(|bytes| bytes.max(0.0) as u64)
        .sum();
    let ideal: f64 = rated
        .iter()
        .filter_map(|map| number(map, "disk_ideal_s"))
        .sum();
    let actual: f64 = rated.iter().filter_map(|map| number(map, "actual_s")).sum();
    let sol_raw = if ideal > 0.0 && actual > 0.0 {
        Some(ideal / actual)
    } else {
        number(rated[0], "disk_sol_raw").or_else(|| number(rated[0], "disk_sol"))
    };
    Some(SolSubPart {
        name: "disk",
        work_bytes,
        light_bps: rated
            .iter()
            .filter_map(|map| number(map, "disk_light_bps"))
            .fold(None, |best: Option<f64>, value| {
                Some(best.map_or(value, |best| best.max(value)))
            }),
        ideal_s: (ideal > 0.0).then_some(ideal),
        sol: sol_raw.map(|value| value.clamp(0.0, 1.0)),
        sol_raw,
        light_src: SolLightSource::parse(rated[0].get("disk_light_src").map(String::as_str)),
        reference_status: rated[0]
            .get("disk_reference_status")
            .cloned()
            .unwrap_or_else(|| {
                if sol_raw.is_some_and(|raw| raw > 1.0) {
                    "above_bound".to_owned()
                } else {
                    "nominal".to_owned()
                }
            }),
    })
}

struct SolLine {
    actual_s: Option<f64>,
    work_bytes: Option<u64>,
    actual_bps: Option<f64>,
    light_bps: Option<f64>,
    ideal_s: Option<f64>,
    sol_raw: Option<f64>,
    light_src: SolLightSource,
    reference_status: Option<String>,
    metric_version: Option<u32>,
    malformed: bool,
}

fn number(map: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    map.get(key)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

fn parse_line(map: &BTreeMap<String, String>) -> SolLine {
    let actual_s = number(map, "actual_ns")
        .map(|ns| ns / 1e9)
        .or_else(|| number(map, "actual_s"));
    let ideal_s = number(map, "ideal_s");
    // Prefer the unclamped field; older lines only carry the clamped `sol`,
    // which is recomputed from ideal/actual when both are present.
    let sol_raw = number(map, "sol_raw")
        .or_else(|| match (ideal_s, actual_s) {
            (Some(ideal), Some(actual)) if actual > 0.0 => Some(ideal / actual),
            _ => None,
        })
        .or_else(|| number(map, "sol"))
        .filter(|value| *value >= 0.0);
    SolLine {
        actual_s,
        work_bytes: number(map, "work_bytes").map(|value| value.max(0.0) as u64),
        actual_bps: number(map, "actual_bps"),
        light_bps: number(map, "light_bps"),
        ideal_s,
        sol_raw,
        light_src: SolLightSource::parse(map.get("light_src").map(String::as_str)),
        reference_status: map.get("reference_status").cloned(),
        metric_version: map
            .get("metric_version")
            .and_then(|value| value.parse::<u32>().ok()),
        malformed: map.get("parse_status").is_some_and(|status| status != "ok"),
    }
}

fn summarize(op: &str, maps: &[&BTreeMap<String, String>]) -> SolOpSummary {
    let lines: Vec<SolLine> = maps.iter().map(|map| parse_line(map)).collect();
    let lines = lines.as_slice();
    let inputs: Vec<_> = maps
        .iter()
        .zip(lines)
        .map(|(map, line)| foxy_sol::AggregationInput {
            actual_s: line.actual_s,
            work_bytes: line.work_bytes,
            useful_work_bytes: map
                .get("useful_output_bytes")
                .and_then(|value| value.parse::<u64>().ok()),
            rated: line.sol_raw.is_some(),
            start_offset_ns: map
                .get("start_offset_ns")
                .and_then(|value| value.parse::<u64>().ok()),
            end_offset_ns: map
                .get("end_offset_ns")
                .and_then(|value| value.parse::<u64>().ok()),
            reference_id: map.get("reference_id").map(String::as_str),
            reference_status: line.reference_status.as_deref(),
            metric_version: line.metric_version.map(u64::from),
            malformed: line.malformed,
            outcome: map.get("outcome").map(String::as_str),
        })
        .collect();
    let aggregate = foxy_sol::aggregate(&inputs);
    let actual_bps = aggregate.actual_bps.or({
        if let [single] = lines {
            single.actual_bps
        } else {
            None
        }
    });
    let labels = distinct_values(maps, "label");
    let heterogeneous = labels.len() > 1;
    let metric_versions: Vec<u32> = aggregate
        .metric_versions
        .iter()
        .filter_map(|version| u32::try_from(*version).ok())
        .collect();
    let mut summary = SolOpSummary {
        op: op.to_owned(),
        runs: aggregate.runs,
        rated_runs: 0,
        actual_s: aggregate.service_s,
        interval_coverage_s: aggregate.interval_coverage_s,
        makespan_s: aggregate.makespan_s,
        useful_work_bytes: aggregate.useful_work_bytes,
        work_bytes: aggregate.work_bytes,
        actual_bps,
        light_bps: None,
        ideal_s: None,
        sol: None,
        sol_raw: None,
        light_src: lines
            .first()
            .map_or(SolLightSource::SelfBaseline, |line| line.light_src.clone()),
        mixed_references: false,
        heterogeneous,
        mixed_metric_versions: metric_versions.len() > 1,
        reference_statuses: aggregate.reference_statuses,
        metric_versions,
        malformed_lines: aggregate.malformed_records,
        outcomes: aggregate.outcomes,
        sub_parts: if op == "download" {
            disk_sub_part(maps).into_iter().collect()
        } else {
            Vec::new()
        },
        details: fold_details(op, maps),
        rated_actual_s: None,
    };
    let rated: Vec<&SolLine> = lines.iter().filter(|line| line.sol_raw.is_some()).collect();
    if !rated.is_empty() {
        summary.rated_runs = rated.len();
        summary.mixed_references = rated
            .iter()
            .any(|line| line.light_src != rated[0].light_src);
        // Serial stages add (E5): the ideal of the whole is the sum of the
        // ideals, measured against the sum of the actuals.
        let with_ideal: Vec<&&SolLine> = rated
            .iter()
            .filter(|line| line.ideal_s.is_some() && line.actual_s.is_some())
            .collect();
        summary.sol_raw = if !with_ideal.is_empty() {
            let ideal: f64 = with_ideal.iter().filter_map(|line| line.ideal_s).sum();
            let actual: f64 = with_ideal.iter().filter_map(|line| line.actual_s).sum();
            summary.ideal_s = Some(ideal);
            summary.rated_actual_s = Some(actual);
            (actual > 0.0).then(|| ideal / actual)
        } else {
            let weight = |line: &SolLine| line.work_bytes.map_or(1.0, |bytes| bytes.max(1) as f64);
            let total: f64 = rated.iter().map(|line| weight(line)).sum();
            Some(
                rated
                    .iter()
                    .map(|line| weight(line) * line.sol_raw.unwrap_or(0.0))
                    .sum::<f64>()
                    / total,
            )
        };
        summary.sol = summary.sol_raw.map(|value| value.clamp(0.0, 1.0));
        summary.light_bps = rated
            .iter()
            .filter_map(|line| line.light_bps)
            .fold(None, |best: Option<f64>, value| {
                Some(best.map_or(value, |best| best.max(value)))
            });
        summary.light_src = rated[0].light_src.clone();
        return summary;
    }
    // No line knew its light. When the batches are the same kind of work,
    // the fastest batch is a same-run peak: a consistency figure, never a
    // physical light. Batches of different profiles or phases (calibration
    // trials against production hashing) are not comparable and get no ratio.
    if lines.len() >= 2
        && !heterogeneous
        && let Some(aggregate) = actual_bps
    {
        let best = lines
            .iter()
            .filter(|line| line.work_bytes.is_some_and(|bytes| bytes > 0))
            .filter_map(|line| line.actual_bps)
            .fold(0.0_f64, f64::max);
        if best > 0.0 {
            summary.rated_runs = lines.len();
            summary.light_bps = Some(best);
            summary.sol_raw = Some(aggregate / best);
            summary.sol = Some((aggregate / best).clamp(0.0, 1.0));
            summary.light_src = SolLightSource::SameRunBest;
        }
    }
    summary
}

impl BenchmarkRecord {
    /// One summary per operation, in first-logged order.
    pub fn sol_summaries(&self) -> Vec<SolOpSummary> {
        let mut ops: Vec<(String, Vec<&BTreeMap<String, String>>)> = Vec::new();
        for map in &self.sol {
            if map.get("record_kind").is_some_and(|kind| kind == "stage") {
                continue;
            }
            let Some(op) = map.get("op") else {
                continue;
            };
            match ops.iter_mut().find(|(known, _)| known == op) {
                Some((_, maps)) => maps.push(map),
                None => ops.push((op.clone(), vec![map])),
            }
        }
        ops.iter().map(|(op, maps)| summarize(op, maps)).collect()
    }

    /// The operation whose ratio describes the action: the download for
    /// transfers, hashing for checks. No other operation stands in for it:
    /// a rated app-update probe in the frame is not the benchmark's ratio.
    pub fn headline_op(&self) -> &'static str {
        if self
            .sol
            .iter()
            .any(|line| line.get("op").is_some_and(|op| op == "sync_action"))
        {
            "sync_action"
        } else if self.kind.transfers_files() {
            "download"
        } else {
            "hash"
        }
    }

    pub fn operation_identity(&self) -> String {
        match self.kind {
            BenchmarkKind::QuickCheck => return "O4 quick scan".to_owned(),
            BenchmarkKind::IntegrityCheck => return "O3 tree hash verification".to_owned(),
            BenchmarkKind::Recheck
                if self
                    .sol
                    .iter()
                    .any(|line| line.get("op").is_some_and(|op| op == "hash"))
                    || self.metrics.hash_files_total > 0 =>
            {
                return "O3 tree hash verification".to_owned();
            }
            BenchmarkKind::Recheck => return "O6 no-change sync".to_owned(),
            BenchmarkKind::Update
            | BenchmarkKind::ForceRedownload
            | BenchmarkKind::AddonDownload
            | BenchmarkKind::AddonForceRedownload => {}
        }
        let delta_lines: Vec<&BTreeMap<String, String>> = self
            .sol
            .iter()
            .filter(|line| line.get("op").is_some_and(|op| op == "delta_patch"))
            .collect();
        if !delta_lines.is_empty() {
            let patched: u64 = delta_lines
                .iter()
                .filter_map(|line| line.get("patched_files"))
                .filter_map(|value| value.parse::<u64>().ok())
                .sum();
            let fallbacks: u64 = delta_lines
                .iter()
                .filter_map(|line| line.get("fallbacks"))
                .filter_map(|value| value.parse::<u64>().ok())
                .sum();
            return if fallbacks > 0 {
                "O2 delta patch + O1 full-download fallback".to_owned()
            } else if patched > 0 {
                "O2 delta patch".to_owned()
            } else {
                "O2 delta patch attempt".to_owned()
            };
        }
        if self
            .sol
            .iter()
            .any(|line| line.get("op").is_some_and(|op| op == "download"))
        {
            return "O1 full-file download".to_owned();
        }
        self.headline_op().to_owned()
    }

    /// The summary of the headline operation, when it exists at all.
    pub fn headline_summary(&self) -> Option<SolOpSummary> {
        let op = self.headline_op();
        self.sol_summaries()
            .into_iter()
            .find(|summary| summary.op == op)
    }

    /// The ratio that summarises the action, only when it is trustworthy as
    /// a headline: one reference kind, full coverage, completed outcome.
    pub fn headline_sol(&self) -> Option<SolOpSummary> {
        (self.outcome == crate::core::benchmarks::BenchmarkOutcome::Success)
            .then(|| self.headline_summary())
            .flatten()
            .filter(SolOpSummary::headline_worthy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmarks::record::*;

    fn line(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn record(kind: BenchmarkKind, sol: Vec<BTreeMap<String, String>>) -> BenchmarkRecord {
        BenchmarkRecord {
            id: "x".into(),
            record_version: 1,
            name: "x".into(),
            notes: String::new(),
            kind,
            started_at: 0,
            started_at_local: String::new(),
            finished_at_local: String::new(),
            elapsed_ms: 1,
            outcome: BenchmarkOutcome::Success,
            operation_id: None,
            repository: BenchmarkRepository::default(),
            addons: vec![],
            favourite: false,
            hidden: false,
            build: BenchmarkBuild::default(),
            machine: BenchmarkMachine::default(),
            metrics: BenchmarkMetrics::default(),
            stages: vec![],
            sol,
            samples: vec![],
            stage_marks: vec![],
            log_file: None,
            log_line_count: 0,
        }
    }

    #[test]
    fn download_line_keeps_its_own_ratio() {
        let record = record(
            BenchmarkKind::ForceRedownload,
            vec![line(&[
                ("op", "download"),
                ("actual_s", "39.904"),
                ("work_bytes", "4331121846"),
                ("actual_bps", "108538515"),
                ("light_bps", "118513659"),
                ("ideal_s", "36.545"),
                ("sol", "0.916"),
                ("light_src", "peak_1s"),
            ])],
        );
        let summary = record.headline_sol().expect("download summary");
        assert_eq!(summary.op, "download");
        assert_eq!(summary.runs, 1);
        assert_eq!(summary.rated_runs, 1);
        assert!((summary.sol.unwrap() - 36.545 / 39.904).abs() < 1e-6);
        assert_eq!(summary.light_bps, Some(118_513_659.0));
        assert_eq!(summary.light_src, SolLightSource::Peak1s);
        assert_eq!(summary.metric_kind(), SolMetricKind::PeakConsistency);
        assert!((summary.headroom().unwrap() - 1.0919).abs() < 1e-3);
        assert!((summary.gap_to_reference_s().unwrap() - 3.359).abs() < 1e-6);
    }

    #[test]
    fn hash_batches_of_one_profile_fold_to_a_same_run_peak() {
        let record = record(
            BenchmarkKind::Recheck,
            vec![
                line(&[
                    ("op", "hash"),
                    ("actual_s", "1.0"),
                    ("work_bytes", "1000"),
                    ("actual_bps", "1000"),
                    ("sol", "na"),
                    ("light_src", "self_baseline"),
                    ("label", "sticky_auto"),
                ]),
                line(&[
                    ("op", "hash"),
                    ("actual_s", "1.0"),
                    ("work_bytes", "3000"),
                    ("actual_bps", "3000"),
                    ("sol", "na"),
                    ("light_src", "self_baseline"),
                    ("label", "sticky_auto"),
                ]),
            ],
        );
        let summary = record.headline_sol().expect("hash summary");
        assert_eq!(summary.runs, 2);
        assert_eq!(summary.work_bytes, 4000);
        assert_eq!(summary.actual_bps, Some(2000.0));
        assert_eq!(summary.light_bps, Some(3000.0));
        assert!((summary.sol.unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(summary.light_src, SolLightSource::SameRunBest);
        assert_eq!(summary.metric_kind(), SolMetricKind::PeakConsistency);
        assert!(!summary.heterogeneous);
    }

    #[test]
    fn heterogeneous_hash_batches_get_totals_but_no_fastest_batch_light() {
        // A calibration trial and the production pass are different work; the
        // trial's page-cache rate must not become the light for the whole run.
        let record = record(
            BenchmarkKind::Recheck,
            vec![
                line(&[
                    ("op", "hash"),
                    ("actual_s", "0.1"),
                    ("work_bytes", "700000000"),
                    ("actual_bps", "7000000000"),
                    ("sol", "na"),
                    ("label", "auto_benchmark_sample"),
                ]),
                line(&[
                    ("op", "hash"),
                    ("actual_s", "35.0"),
                    ("work_bytes", "3600000000"),
                    ("actual_bps", "102857142"),
                    ("sol", "na"),
                    ("label", "selected_remaining"),
                ]),
            ],
        );
        let summary = &record.sol_summaries()[0];
        assert!(summary.heterogeneous);
        assert_eq!(summary.sol, None);
        assert_eq!(summary.rated_runs, 0);
        assert_eq!(summary.metric_kind(), SolMetricKind::None);
        assert_eq!(summary.work_bytes, 4_300_000_000);
        assert!((summary.actual_s - 35.1).abs() < 1e-9);
        assert!(record.headline_sol().is_none());
    }

    #[test]
    fn service_sum_can_exceed_the_action_makespan_without_becoming_a_share() {
        // Three overlapped 1 s batches inside a 1.2 s action: the folded
        // `actual_s` is occupancy (3 s), and nothing here divides it by the
        // record's elapsed time.
        let mut record = record(
            BenchmarkKind::Update,
            (0..3)
                .map(|_| {
                    line(&[
                        ("op", "hash"),
                        ("actual_s", "1.0"),
                        ("work_bytes", "100"),
                        ("actual_bps", "100"),
                        ("label", "incremental"),
                    ])
                })
                .collect(),
        );
        record.elapsed_ms = 1200;
        let summary = &record.sol_summaries()[0];
        assert_eq!(summary.actual_s, 3.0);
        assert!(summary.actual_s > record.elapsed_secs());
        assert_eq!(summary.sol, Some(1.0));
    }

    #[test]
    fn intervals_keep_service_coverage_and_makespan_distinct() {
        let record = record(
            BenchmarkKind::Recheck,
            vec![
                line(&[
                    ("op", "hash"),
                    ("actual_s", "2"),
                    ("work_bytes", "20"),
                    ("start_offset_ns", "0"),
                    ("end_offset_ns", "2000000000"),
                ]),
                line(&[
                    ("op", "hash"),
                    ("actual_s", "2"),
                    ("work_bytes", "20"),
                    ("start_offset_ns", "1000000000"),
                    ("end_offset_ns", "3000000000"),
                ]),
                line(&[
                    ("op", "hash"),
                    ("actual_s", "1"),
                    ("work_bytes", "10"),
                    ("start_offset_ns", "4000000000"),
                    ("end_offset_ns", "5000000000"),
                ]),
            ],
        );
        let summary = &record.sol_summaries()[0];
        assert_eq!(summary.actual_s, 5.0);
        assert_eq!(summary.interval_coverage_s, Some(4.0));
        assert_eq!(summary.makespan_s, Some(5.0));
        assert_eq!(summary.useful_work_bytes, 50);
    }

    #[test]
    fn shared_aggregation_corpus_matches_benchmark_summaries() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../foxy-sol/tests/aggregation-corpus.json"
        ))
        .expect("aggregation corpus");
        for case in corpus["cases"].as_array().expect("cases") {
            let maps: Vec<BTreeMap<String, String>> = case["records"]
                .as_array()
                .expect("records")
                .iter()
                .map(|record| {
                    let mut map: BTreeMap<String, String> = record
                        .as_object()
                        .expect("record")
                        .iter()
                        .filter(|(key, _)| !matches!(key.as_str(), "rated" | "malformed"))
                        .map(|(key, value)| {
                            (
                                if key == "useful_work_bytes" {
                                    "useful_output_bytes".to_owned()
                                } else {
                                    key.clone()
                                },
                                value
                                    .as_str()
                                    .map_or_else(|| value.to_string(), str::to_owned),
                            )
                        })
                        .collect();
                    if record["rated"] == true {
                        map.insert("sol".into(), "1".into());
                    }
                    if record["malformed"] == true {
                        map.insert("parse_status".into(), "malformed".into());
                    }
                    map
                })
                .collect();
            let refs: Vec<_> = maps.iter().collect();
            let actual = summarize("hash", &refs);
            let expected = &case["expected"];
            let name = case["name"].as_str().expect("name");
            assert_eq!(actual.actual_s, expected["service_s"], "{name}");
            assert_eq!(
                serde_json::to_value(actual.interval_coverage_s).expect("coverage"),
                expected["interval_coverage_s"],
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(actual.makespan_s).expect("makespan"),
                expected["makespan_s"],
                "{name}"
            );
            assert_eq!(
                actual.useful_work_bytes, expected["useful_work_bytes"],
                "{name}"
            );
            assert_eq!(actual.rated_runs, expected["rated_runs"], "{name}");
            assert_eq!(
                serde_json::to_value(&actual.reference_statuses).expect("reference statuses"),
                expected["reference_statuses"],
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(&actual.metric_versions).expect("metric versions"),
                expected["metric_versions"],
                "{name}"
            );
            assert_eq!(
                actual.malformed_lines, expected["malformed_records"],
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(&actual.outcomes).expect("outcomes"),
                expected["outcomes"],
                "{name}"
            );
            assert_eq!(actual.completed(), expected["completed"], "{name}");
        }
    }

    #[test]
    fn raw_ratio_above_one_is_kept_and_flags_the_reference() {
        let record = record(
            BenchmarkKind::Update,
            vec![line(&[
                ("op", "download"),
                ("actual_s", "10.0"),
                ("work_bytes", "1000"),
                ("light_bps", "50"),
                ("ideal_s", "20.0"),
                ("sol", "1.000"),
                ("sol_raw", "2.0000"),
                ("light_src", "limiter_cap"),
            ])],
        );
        let summary = &record.sol_summaries()[0];
        assert_eq!(summary.sol, Some(1.0));
        assert_eq!(summary.sol_raw, Some(2.0));
        assert_eq!(summary.metric_kind(), SolMetricKind::ModeledBound);
        assert_eq!(summary.gap_to_reference_s(), Some(-10.0));
        assert_eq!(summary.display_sol(), None);
        assert_eq!(summary.unavailable_reason(), "above bound");
        assert!(record.headline_sol().is_none());
    }

    #[test]
    fn malformed_mixed_version_and_failed_summaries_cannot_be_headlines() {
        for (extra, reason) in [
            (vec![("parse_status", "malformed")], "malformed data"),
            (
                vec![("reference_status", "invalid_actual")],
                "invalid reference",
            ),
        ] {
            let mut pairs = vec![
                ("op", "download"),
                ("actual_s", "1"),
                ("ideal_s", "0.5"),
                ("sol", "0.5"),
                ("sol_raw", "0.5"),
            ];
            pairs.extend(extra);
            let record = record(BenchmarkKind::Update, vec![line(&pairs)]);
            let summary = &record.sol_summaries()[0];
            assert_eq!(summary.unavailable_reason(), reason);
            assert!(record.headline_sol().is_none());
        }

        let mixed = record(
            BenchmarkKind::Update,
            vec![
                line(&[
                    ("op", "download"),
                    ("actual_s", "1"),
                    ("ideal_s", "0.5"),
                    ("sol", "0.5"),
                    ("metric_version", "1"),
                ]),
                line(&[
                    ("op", "download"),
                    ("actual_s", "1"),
                    ("ideal_s", "0.5"),
                    ("sol", "0.5"),
                    ("metric_version", "2"),
                ]),
            ],
        );
        let summary = &mixed.sol_summaries()[0];
        assert!(summary.mixed_metric_versions);
        assert_eq!(summary.unavailable_reason(), "metric versions differ");
        assert!(mixed.headline_sol().is_none());

        let mut failed = record(
            BenchmarkKind::Update,
            vec![line(&[
                ("op", "download"),
                ("actual_s", "1"),
                ("ideal_s", "0.5"),
                ("sol", "0.5"),
            ])],
        );
        failed.outcome = BenchmarkOutcome::Failed {
            message: "failed after transfer".into(),
        };
        assert!(failed.headline_sol().is_none());
    }

    #[test]
    fn terminal_sync_action_owns_the_headline() {
        let record = record(
            BenchmarkKind::Update,
            vec![
                line(&[("op", "download"), ("actual_s", "1"), ("sol", "0.8")]),
                line(&[
                    ("op", "sync_action"),
                    ("actual_s", "2"),
                    ("outcome", "completed"),
                ]),
            ],
        );
        assert_eq!(record.headline_op(), "sync_action");
        assert_eq!(record.headline_summary().unwrap().op, "sync_action");
        assert!(record.headline_sol().is_none());
    }

    #[test]
    fn patch_identity_keeps_success_and_fallback_attempts_distinct() {
        let pure = record(
            BenchmarkKind::Update,
            vec![line(&[
                ("op", "delta_patch"),
                ("patched_files", "4"),
                ("fallbacks", "0"),
                ("outcome", "completed"),
            ])],
        );
        assert_eq!(pure.operation_identity(), "O2 delta patch");
        assert_eq!(
            pure.sol_summaries()[0].category(),
            Some(("O2", "Delta patch"))
        );

        let fallback = record(
            BenchmarkKind::Update,
            vec![
                line(&[
                    ("op", "delta_patch"),
                    ("patched_files", "3"),
                    ("fallbacks", "1"),
                    ("outcome", "completed_with_fallback"),
                ]),
                line(&[("op", "download"), ("actual_s", "2")]),
            ],
        );
        assert_eq!(
            fallback.operation_identity(),
            "O2 delta patch + O1 full-download fallback"
        );
        assert_eq!(fallback.sol_summaries().len(), 2);
    }

    #[test]
    fn patch_stage_records_do_not_become_ui_operations() {
        let patch = record(
            BenchmarkKind::Update,
            vec![
                line(&[
                    ("op", "delta_patch_stage"),
                    ("record_kind", "stage"),
                    ("stage_id", "apply"),
                    ("actual_ns", "12"),
                ]),
                line(&[
                    ("op", "delta_patch"),
                    ("patched_files", "1"),
                    ("fallbacks", "0"),
                    ("outcome", "completed"),
                ]),
            ],
        );
        assert_eq!(patch.sol_summaries().len(), 1);
        assert_eq!(patch.sol_summaries()[0].op, "delta_patch");
    }

    #[test]
    fn non_transfer_identity_uses_the_requested_benchmark_action() {
        let incidental = vec![
            line(&[("op", "remote_refresh"), ("outcome", "graph_unchanged")]),
            line(&[("op", "quick_scan"), ("outcome", "no_changes")]),
        ];
        assert_eq!(
            record(BenchmarkKind::Recheck, incidental.clone()).operation_identity(),
            "O6 no-change sync"
        );
        assert_eq!(
            record(BenchmarkKind::QuickCheck, incidental.clone()).operation_identity(),
            "O4 quick scan"
        );
        assert_eq!(
            record(
                BenchmarkKind::IntegrityCheck,
                vec![line(&[("op", "hash"), ("outcome", "completed")])],
            )
            .operation_identity(),
            "O3 tree hash verification"
        );
    }

    #[test]
    fn precise_duration_field_wins_over_the_rounded_seconds() {
        let record = record(
            BenchmarkKind::Recheck,
            vec![line(&[
                ("op", "hash"),
                ("actual_s", "0.000"),
                ("actual_ns", "250000"),
                ("work_bytes", "1000"),
            ])],
        );
        let summary = &record.sol_summaries()[0];
        assert!((summary.actual_s - 0.00025).abs() < 1e-12);
        assert_eq!(summary.actual_bps, Some(4_000_000.0));
    }

    #[test]
    fn download_disk_keys_become_a_nominal_sub_part_and_details_fold() {
        let record = record(
            BenchmarkKind::Update,
            vec![line(&[
                ("op", "download"),
                ("actual_s", "100.0"),
                ("work_bytes", "1000"),
                ("sol", "0.9"),
                ("light_src", "limiter_cap"),
                ("files", "7"),
                ("peak_1s_bps", "5000"),
                ("delta_savings_percent", "40"),
                ("destination_storage", "Hdd"),
                ("outcome", "completed"),
                ("disk_bytes", "2000"),
                ("disk_light_bps", "40"),
                ("disk_ideal_s", "50.0"),
                ("disk_sol", "0.5"),
                ("disk_light_src", "nominal_hdd_sequential"),
            ])],
        );
        let summary = &record.sol_summaries()[0];
        assert_eq!(summary.category(), Some(("O1", "Full-file download")));
        let disk = &summary.sub_parts[0];
        assert_eq!(disk.name, "disk");
        assert_eq!(disk.work_bytes, 2000);
        assert_eq!(disk.sol, Some(0.5));
        assert_eq!(disk.light_src, SolLightSource::NominalHddSequential);
        assert_eq!(disk.metric_kind(), SolMetricKind::Nominal);
        let labels: Vec<&str> = summary.details.iter().map(|d| d.label).collect();
        assert_eq!(
            labels,
            [
                "Files",
                "Peak window",
                "Delta savings",
                "Destination storage",
                "Outcome"
            ]
        );
        assert_eq!(summary.details[0].value, SolDetailValue::Count(7.0));
        assert_eq!(summary.details[2].value, SolDetailValue::Percent(40.0));
        assert_eq!(summary.details[3].value, SolDetailValue::Text("Hdd".into()));
        assert_eq!(summary.outcomes, ["completed"]);
        assert!(summary.completed());
    }

    #[test]
    fn savings_fold_from_summed_bytes_not_the_mean_of_percentages() {
        // 90% of 100 bytes and 10% of 900 bytes is 18% overall, not 50%.
        let record = record(
            BenchmarkKind::Update,
            vec![
                line(&[
                    ("op", "download"),
                    ("actual_s", "1"),
                    ("delta_savings_percent", "90"),
                    ("delta_savings_bytes", "90"),
                    ("full_bytes", "100"),
                ]),
                line(&[
                    ("op", "download"),
                    ("actual_s", "1"),
                    ("delta_savings_percent", "10"),
                    ("delta_savings_bytes", "90"),
                    ("full_bytes", "900"),
                ]),
            ],
        );
        let summary = &record.sol_summaries()[0];
        let savings = summary
            .details
            .iter()
            .find(|d| d.label == "Delta savings")
            .unwrap();
        assert_eq!(savings.value, SolDetailValue::Percent(18.0));
    }

    #[test]
    fn scan_rate_folds_from_summed_addons_over_summed_time() {
        // 100 addons in 0.1 s and 100 addons in 0.9 s is 200 addons/s, not
        // the mean of 1000 and 111.
        let record = record(
            BenchmarkKind::QuickCheck,
            vec![
                line(&[
                    ("op", "quick_scan"),
                    ("actual_s", "0.1"),
                    ("addons_total", "100"),
                    ("addons_per_s", "1000"),
                ]),
                line(&[
                    ("op", "quick_scan"),
                    ("actual_s", "0.9"),
                    ("addons_total", "100"),
                    ("addons_per_s", "111.1"),
                ]),
            ],
        );
        let summary = &record.sol_summaries()[0];
        let rate = summary
            .details
            .iter()
            .find(|d| d.label == "Scan rate")
            .unwrap();
        assert_eq!(rate.value, SolDetailValue::Rate(200.0, "addons/s"));
    }

    #[test]
    fn hash_details_sum_runs_and_list_distinct_profiles() {
        let record = record(
            BenchmarkKind::Recheck,
            vec![
                line(&[
                    ("op", "hash"),
                    ("actual_s", "1.0"),
                    ("work_bytes", "10"),
                    ("files", "3"),
                    ("compute_s", "0.5"),
                    ("label", "auto_heuristic"),
                ]),
                line(&[
                    ("op", "hash"),
                    ("actual_s", "1.0"),
                    ("work_bytes", "10"),
                    ("files", "4"),
                    ("compute_s", "0.25"),
                    ("label", "sticky_auto"),
                ]),
                line(&[
                    ("op", "hash"),
                    ("actual_s", "1.0"),
                    ("work_bytes", "10"),
                    ("files", "1"),
                    ("compute_s", "0.25"),
                    ("label", "sticky_auto"),
                ]),
            ],
        );
        let summary = &record.sol_summaries()[0];
        assert!(summary.sub_parts.is_empty());
        let files = summary.details.iter().find(|d| d.label == "Files").unwrap();
        assert_eq!(files.value, SolDetailValue::Count(8.0));
        let compute = summary
            .details
            .iter()
            .find(|d| d.label == "Blocking task time")
            .unwrap();
        assert_eq!(compute.value, SolDetailValue::Secs(1.0));
        let profile = summary
            .details
            .iter()
            .find(|d| d.label == "Profile")
            .unwrap();
        assert_eq!(
            profile.value,
            SolDetailValue::Text("auto_heuristic, sticky_auto".into())
        );
    }

    #[test]
    fn single_unrated_line_has_no_ratio() {
        let record = record(
            BenchmarkKind::Recheck,
            vec![line(&[
                ("op", "hash"),
                ("actual_s", "1.0"),
                ("work_bytes", "1000"),
                ("sol", "na"),
            ])],
        );
        let summaries = record.sol_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].sol, None);
        assert_eq!(summaries[0].light_src, SolLightSource::SelfBaseline);
        assert_eq!(summaries[0].metric_kind(), SolMetricKind::None);
        assert!(record.headline_sol().is_none());
        assert!(record.headline_summary().is_some());
    }

    #[test]
    fn headline_never_falls_back_to_another_operation() {
        let record = record(
            BenchmarkKind::Update,
            vec![
                line(&[("op", "hash"), ("actual_s", "1.0"), ("sol", "na")]),
                line(&[
                    ("op", "app_update_check"),
                    ("actual_s", "0.1"),
                    ("sol", "0.9"),
                    ("light_src", "limiter_cap"),
                ]),
            ],
        );
        assert_eq!(record.headline_op(), "download");
        assert!(record.headline_sol().is_none());
        assert!(record.headline_summary().is_none());
    }

    #[test]
    fn partial_coverage_and_failed_outcomes_are_not_headlines() {
        let partial = record(
            BenchmarkKind::Update,
            vec![
                line(&[
                    ("op", "download"),
                    ("actual_s", "2.0"),
                    ("ideal_s", "1.0"),
                    ("sol", "0.5"),
                    ("light_src", "limiter_cap"),
                ]),
                line(&[("op", "download"), ("actual_s", "2.0"), ("sol", "na")]),
            ],
        );
        let summary = &partial.sol_summaries()[0];
        assert_eq!(summary.sol, Some(0.5));
        assert_eq!(summary.rated_runs, 1);
        assert!(!summary.full_coverage());
        assert!(partial.headline_sol().is_none());

        let cancelled = record(
            BenchmarkKind::Update,
            vec![line(&[
                ("op", "download"),
                ("actual_s", "2.0"),
                ("ideal_s", "1.0"),
                ("sol", "0.5"),
                ("light_src", "limiter_cap"),
                ("outcome", "cancelled"),
            ])],
        );
        let summary = &cancelled.sol_summaries()[0];
        assert!(!summary.completed());
        assert!(cancelled.headline_sol().is_none());
    }

    #[test]
    fn mixed_reference_kinds_are_flagged() {
        let record = record(
            BenchmarkKind::Update,
            vec![
                line(&[
                    ("op", "download"),
                    ("actual_s", "2.0"),
                    ("ideal_s", "1.0"),
                    ("sol", "0.5"),
                    ("light_src", "limiter_cap"),
                ]),
                line(&[
                    ("op", "download"),
                    ("actual_s", "2.0"),
                    ("ideal_s", "1.0"),
                    ("sol", "0.5"),
                    ("light_src", "peak_1s"),
                ]),
            ],
        );
        let summary = &record.sol_summaries()[0];
        assert!(summary.mixed_references);
        assert!(record.headline_sol().is_none());
    }
}
