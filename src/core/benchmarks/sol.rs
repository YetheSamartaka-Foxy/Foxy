//! Speed-of-light summary of the `SOL op=...` lines a benchmark captured,
//! folded to one row per operation so a run that logged dozens of hash
//! batches still reads as a single ratio. See `conventions/SPEED_OF_LIGHT.md`.

use std::collections::BTreeMap;

use super::record::BenchmarkRecord;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolLightSource {
    /// The user's bandwidth cap is the light (exact ceiling).
    LimiterCap,
    /// The best 1-second sample of the same run is the light.
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
            Self::Peak1s => "same-run peak second",
            Self::SelfBaseline => "no absolute light",
            Self::SameRunBest => "fastest batch of this run",
            Self::NominalHddSequential => "nominal HDD sequential rate",
            Self::Other(_) => "unknown light",
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
    pub sol: Option<f64>,
    pub light_src: SolLightSource,
}

impl SolSubPart {
    pub fn headroom(&self) -> Option<f64> {
        self.sol.filter(|sol| *sol > 0.0).map(|sol| 1.0 / sol)
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
    pub actual_s: f64,
    pub work_bytes: u64,
    pub actual_bps: Option<f64>,
    pub light_bps: Option<f64>,
    pub ideal_s: Option<f64>,
    /// Ratio in `[0, 1]`; `None` when no light is knowable.
    pub sol: Option<f64>,
    pub light_src: SolLightSource,
    pub sub_parts: Vec<SolSubPart>,
    pub details: Vec<SolDetail>,
}

impl SolOpSummary {
    /// How many times faster the operation could run before physics
    /// objects (E3 of the convention).
    pub fn headroom(&self) -> Option<f64> {
        self.sol.filter(|sol| *sol > 0.0).map(|sol| 1.0 / sol)
    }

    /// Crucial-operation number and name from `conventions/SPEED_OF_LIGHT.md`,
    /// `None` for an operation the convention does not list.
    pub fn category(&self) -> Option<(&'static str, &'static str)> {
        Some(match self.op.as_str() {
            "download" => ("O1", "Full-file download"),
            "hash" => ("O3", "Tree hash verification"),
            "quick_scan" => ("O4", "Quick scan"),
            "remote_refresh" => ("O5", "Remote metadata refresh"),
            "startup" | "startup_probe" => ("O8", "Startup to first sync verdict"),
            "app_update_check" => ("O5", "App update check"),
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
            _ => "overall",
        }
    }
}

#[derive(Clone, Copy)]
enum Fold {
    Sum,
    Max,
    Mean,
    Distinct,
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
            ("peak_1s_bps", "Peak second", Kind::BytesPerSec, Fold::Max),
            (
                "delta_savings_percent",
                "Delta savings",
                Kind::Percent,
                Fold::Mean,
            ),
            (
                "destination_storage",
                "Destination storage",
                Kind::Text,
                Fold::Distinct,
            ),
        ],
        "hash" => &[
            ("files", "Files", Kind::Count, Fold::Sum),
            ("parts", "Parts", Kind::Count, Fold::Sum),
            ("compute_s", "Hash compute (CPU)", Kind::Secs, Fold::Sum),
            ("wait_s", "I/O wait", Kind::Secs, Fold::Sum),
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
                Fold::Mean,
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
        _ => &[],
    }
}

fn fold_details(op: &str, maps: &[&BTreeMap<String, String>]) -> Vec<SolDetail> {
    let mut out = Vec::new();
    for (key, label, kind, fold) in detail_specs(op) {
        let raw: Vec<&str> = maps
            .iter()
            .filter_map(|map| map.get(*key).map(String::as_str))
            .collect();
        if raw.is_empty() {
            continue;
        }
        let value = match kind {
            Kind::Text => {
                let mut distinct: Vec<&str> = Vec::new();
                for value in raw {
                    if !distinct.contains(&value) {
                        distinct.push(value);
                    }
                }
                SolDetailValue::Text(distinct.join(", "))
            }
            _ => {
                let numbers: Vec<f64> = raw
                    .iter()
                    .filter_map(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite())
                    .collect();
                if numbers.is_empty() {
                    continue;
                }
                let folded = match fold {
                    Fold::Sum => numbers.iter().sum(),
                    Fold::Max => numbers.iter().copied().fold(0.0_f64, f64::max),
                    Fold::Mean => numbers.iter().sum::<f64>() / numbers.len() as f64,
                    Fold::Distinct => numbers[0],
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
    let sol = if ideal > 0.0 && actual > 0.0 {
        Some((ideal / actual).clamp(0.0, 1.0))
    } else {
        number(rated[0], "disk_sol").map(|value| value.clamp(0.0, 1.0))
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
        sol,
        light_src: SolLightSource::parse(rated[0].get("disk_light_src").map(String::as_str)),
    })
}

struct SolLine {
    actual_s: Option<f64>,
    work_bytes: Option<u64>,
    actual_bps: Option<f64>,
    light_bps: Option<f64>,
    ideal_s: Option<f64>,
    sol: Option<f64>,
    light_src: SolLightSource,
}

fn number(map: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    map.get(key)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

fn parse_line(map: &BTreeMap<String, String>) -> SolLine {
    SolLine {
        actual_s: number(map, "actual_s"),
        work_bytes: number(map, "work_bytes").map(|value| value.max(0.0) as u64),
        actual_bps: number(map, "actual_bps"),
        light_bps: number(map, "light_bps"),
        ideal_s: number(map, "ideal_s"),
        sol: number(map, "sol").map(|value| value.clamp(0.0, 1.0)),
        light_src: SolLightSource::parse(map.get("light_src").map(String::as_str)),
    }
}

fn summarize(op: &str, maps: &[&BTreeMap<String, String>]) -> SolOpSummary {
    let lines: Vec<SolLine> = maps.iter().map(|map| parse_line(map)).collect();
    let lines = lines.as_slice();
    let actual_s: f64 = lines.iter().filter_map(|line| line.actual_s).sum();
    let work_bytes: u64 = lines.iter().filter_map(|line| line.work_bytes).sum();
    let actual_bps = if work_bytes > 0 && actual_s > 0.0 {
        Some(work_bytes as f64 / actual_s)
    } else if let [single] = lines {
        single.actual_bps
    } else {
        None
    };
    let mut summary = SolOpSummary {
        op: op.to_owned(),
        runs: lines.len(),
        actual_s,
        work_bytes,
        actual_bps,
        light_bps: None,
        ideal_s: None,
        sol: None,
        light_src: lines
            .first()
            .map_or(SolLightSource::SelfBaseline, |line| line.light_src.clone()),
        sub_parts: if op == "download" {
            disk_sub_part(maps).into_iter().collect()
        } else {
            Vec::new()
        },
        details: fold_details(op, maps),
    };
    let rated: Vec<&SolLine> = lines.iter().filter(|line| line.sol.is_some()).collect();
    if !rated.is_empty() {
        // Serial stages add (E5): the ideal of the whole is the sum of the
        // ideals, measured against the sum of the actuals.
        let with_ideal: Vec<&&SolLine> = rated
            .iter()
            .filter(|line| line.ideal_s.is_some() && line.actual_s.is_some())
            .collect();
        summary.sol = if !with_ideal.is_empty() {
            let ideal: f64 = with_ideal.iter().filter_map(|line| line.ideal_s).sum();
            let actual: f64 = with_ideal.iter().filter_map(|line| line.actual_s).sum();
            summary.ideal_s = Some(ideal);
            (actual > 0.0).then(|| (ideal / actual).clamp(0.0, 1.0))
        } else {
            let weight = |line: &SolLine| line.work_bytes.map_or(1.0, |bytes| bytes.max(1) as f64);
            let total: f64 = rated.iter().map(|line| weight(line)).sum();
            Some(
                rated
                    .iter()
                    .map(|line| weight(line) * line.sol.unwrap_or(0.0))
                    .sum::<f64>()
                    / total,
            )
        };
        summary.light_bps = rated
            .iter()
            .filter_map(|line| line.light_bps)
            .fold(None, |best: Option<f64>, value| {
                Some(best.map_or(value, |best| best.max(value)))
            });
        summary.light_src = rated[0].light_src.clone();
        return summary;
    }
    // No line knew its light: the fastest batch of the run is the best
    // demonstrated rate, which the convention allows as the light until
    // physics says otherwise.
    if lines.len() >= 2
        && let Some(aggregate) = actual_bps
    {
        let best = lines
            .iter()
            .filter(|line| line.work_bytes.is_some_and(|bytes| bytes > 0))
            .filter_map(|line| line.actual_bps)
            .fold(0.0_f64, f64::max);
        if best > 0.0 {
            summary.light_bps = Some(best);
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

    /// The ratio that summarises the action: the download operation for
    /// transfers, hashing for checks, else the first operation with a light.
    pub fn headline_sol(&self) -> Option<SolOpSummary> {
        let summaries = self.sol_summaries();
        let preferred = if self.kind.transfers_files() {
            "download"
        } else {
            "hash"
        };
        summaries
            .iter()
            .find(|summary| summary.op == preferred && summary.sol.is_some())
            .or_else(|| summaries.iter().find(|summary| summary.sol.is_some()))
            .cloned()
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
        assert!((summary.sol.unwrap() - 36.545 / 39.904).abs() < 1e-6);
        assert_eq!(summary.light_bps, Some(118_513_659.0));
        assert_eq!(summary.light_src, SolLightSource::Peak1s);
        assert!((summary.headroom().unwrap() - 1.0919).abs() < 1e-3);
    }

    #[test]
    fn hash_batches_fold_to_best_batch_light() {
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
                ]),
                line(&[
                    ("op", "hash"),
                    ("actual_s", "1.0"),
                    ("work_bytes", "3000"),
                    ("actual_bps", "3000"),
                    ("sol", "na"),
                    ("light_src", "self_baseline"),
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
    }

    #[test]
    fn download_disk_keys_become_a_sub_part_and_details_fold() {
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
        let labels: Vec<&str> = summary.details.iter().map(|d| d.label).collect();
        assert_eq!(
            labels,
            [
                "Files",
                "Peak second",
                "Delta savings",
                "Destination storage"
            ]
        );
        assert_eq!(summary.details[0].value, SolDetailValue::Count(7.0));
        assert_eq!(summary.details[3].value, SolDetailValue::Text("Hdd".into()));
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
            .find(|d| d.label == "Hash compute (CPU)")
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
        assert!(record.headline_sol().is_none());
    }

    #[test]
    fn headline_falls_back_to_any_rated_operation() {
        let record = record(
            BenchmarkKind::Update,
            vec![
                line(&[("op", "hash"), ("actual_s", "1.0"), ("sol", "na")]),
                line(&[
                    ("op", "download"),
                    ("actual_s", "2.0"),
                    ("sol", "0.5"),
                    ("light_src", "limiter_cap"),
                ]),
            ],
        );
        let summary = record.headline_sol().expect("download fallback");
        assert_eq!(summary.op, "download");
        assert_eq!(summary.sol, Some(0.5));
        assert_eq!(summary.light_src, SolLightSource::LimiterCap);
    }
}
