//! Chart series derived from a benchmark record.

use eframe::egui::Color32;

use super::charts::{BarRow, ChartSeries, SeriesStyle, YUnit};
use crate::core::benchmarks::BenchmarkRecord;

/// One chart of a record: title key, unit and the series it plots.
pub struct ChartDef {
    pub key: &'static str,
    pub title: &'static str,
    pub unit: YUnit,
    pub series: Vec<ChartSeries>,
}

/// Raw CPU samples sum over logical CPUs; scale them to a share of the
/// whole machine when the record knows its thread count.
pub fn cpu_scale(record: &BenchmarkRecord) -> f64 {
    if record.machine.cpu_threads > 0 {
        1.0 / record.machine.cpu_threads as f64
    } else {
        1.0
    }
}

/// Chart and metric title for CPU, saying which scale the values use.
pub fn cpu_title(record: &BenchmarkRecord) -> &'static str {
    if record.machine.cpu_threads > 0 {
        "CPU (all cores)"
    } else {
        "CPU (one core = 100%)"
    }
}

fn points(
    record: &BenchmarkRecord,
    value: impl Fn(&crate::core::benchmarks::BenchmarkSample) -> f64,
) -> Vec<[f64; 2]> {
    record
        .samples
        .iter()
        .map(|sample| [sample.t_ms as f64 / 1000.0, value(sample)])
        .collect()
}

pub fn charts_for(record: &BenchmarkRecord, prefix: &str, colors: &[Color32]) -> Vec<ChartDef> {
    build_charts(record, prefix, colors, SeriesStyle::Single, false)
}

fn build_charts(
    record: &BenchmarkRecord,
    prefix: &str,
    colors: &[Color32],
    style: SeriesStyle,
    split_rates: bool,
) -> Vec<ChartDef> {
    let color = |index: usize| colors[index % colors.len()];
    let label = |text: &str| format!("{prefix}{text}");
    let rate_charts = if split_rates {
        vec![
            ChartDef {
                key: "download_rate",
                title: "Download speed",
                unit: YUnit::BytesPerSec,
                series: vec![ChartSeries {
                    label: label("Download"),
                    color: color(0),
                    points: points(record, |s| s.download_bps),
                    cumulative: false,
                    style,
                }],
            },
            ChartDef {
                key: "disk_write_rate",
                title: "Disk write speed",
                unit: YUnit::BytesPerSec,
                series: vec![ChartSeries {
                    label: label("Disk write"),
                    color: color(1),
                    points: points(record, |s| s.disk_write_bps),
                    cumulative: false,
                    style,
                }],
            },
        ]
    } else {
        vec![ChartDef {
            key: "rates",
            title: "Transfer rates",
            unit: YUnit::BytesPerSec,
            series: vec![
                ChartSeries {
                    label: label("Download"),
                    color: color(0),
                    points: points(record, |s| s.download_bps),
                    cumulative: false,
                    style,
                },
                ChartSeries {
                    label: label("Disk write"),
                    color: color(1),
                    points: points(record, |s| s.disk_write_bps),
                    cumulative: false,
                    style,
                },
            ],
        }]
    };
    let mut defs = rate_charts;
    defs.extend([
        ChartDef {
            key: "downloaded",
            title: "Downloaded",
            unit: YUnit::Bytes,
            series: vec![ChartSeries {
                label: label("Downloaded"),
                color: color(0),
                points: points(record, |s| s.downloaded_bytes as f64),
                cumulative: true,
                style,
            }],
        },
        ChartDef {
            key: "hashing",
            title: "Files checked",
            unit: YUnit::Count,
            series: vec![ChartSeries {
                label: label("Files"),
                color: color(2),
                points: points(record, |s| s.hash_files_done as f64),
                cumulative: true,
                style,
            }],
        },
        ChartDef {
            key: "memory",
            title: "Process memory",
            unit: YUnit::Bytes,
            series: vec![ChartSeries {
                label: label("Memory"),
                color: color(3),
                points: points(record, |s| s.memory_bytes as f64),
                cumulative: false,
                style,
            }],
        },
        ChartDef {
            key: "cpu",
            title: cpu_title(record),
            unit: YUnit::Percent,
            series: vec![ChartSeries {
                label: label("CPU"),
                color: color(4),
                points: points(record, |s| s.cpu_percent * cpu_scale(record)),
                cumulative: false,
                style,
            }],
        },
        ChartDef {
            key: "progress",
            title: "Progress",
            unit: YUnit::Percent,
            series: vec![ChartSeries {
                label: label("Progress"),
                color: color(5),
                points: points(record, |s| s.progress_percent as f64),
                cumulative: true,
                style,
            }],
        },
    ]);
    for def in &mut defs {
        def.series.retain(ChartSeries::has_signal);
    }
    defs.retain(|def| !def.series.is_empty());
    defs
}

/// Charts for one record in a comparison, using one color and visual style
/// consistently across metrics.
pub fn comparison_charts_for(record: &BenchmarkRecord, style: SeriesStyle) -> Vec<ChartDef> {
    let (prefix, color) = match style {
        SeriesStyle::ComparisonA => ("A: ", crate::ui::palette::BENCHMARK_COMPARE_A),
        SeriesStyle::ComparisonB => ("B: ", crate::ui::palette::BENCHMARK_COMPARE_B),
        SeriesStyle::Single => ("", crate::ui::palette::BENCHMARK_COMPARE_A),
    };
    build_charts(record, prefix, &[color], style, true)
}

/// Stages under this many seconds in every record are noise the chart
/// would only render as empty tracks.
const STAGE_FLOOR_SECS: f64 = 0.0005;

/// Stage rows for one or two records; a stage missing from a record is 0
/// and a stage that is 0 in every record is left out.
pub fn stage_rows(records: &[&BenchmarkRecord]) -> Vec<BarRow> {
    let mut names: Vec<String> = Vec::new();
    for record in records {
        for stage in &record.stages {
            if !names.contains(&stage.name)
                && !crate::core::benchmarks::record::ACTION_TOTAL_STAGES
                    .contains(&stage.name.as_str())
            {
                names.push(stage.name.clone());
            }
        }
    }
    names
        .into_iter()
        .map(|name| BarRow {
            values: records
                .iter()
                .map(|record| {
                    record
                        .stages
                        .iter()
                        .find(|stage| stage.name == name)
                        .map_or(0.0, |stage| stage.seconds)
                })
                .collect(),
            label: name,
        })
        .filter(|row| row.values.iter().any(|value| *value >= STAGE_FLOOR_SECS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmarks::record::BenchmarkStage;
    use crate::core::benchmarks::*;

    fn record(samples: Vec<BenchmarkSample>, stages: Vec<BenchmarkStage>) -> BenchmarkRecord {
        BenchmarkRecord {
            id: "x".into(),
            record_version: 1,
            name: "x".into(),
            notes: String::new(),
            kind: BenchmarkKind::Recheck,
            started_at: 0,
            started_at_local: String::new(),
            finished_at_local: String::new(),
            elapsed_ms: 0,
            outcome: BenchmarkOutcome::Success,
            operation_id: None,
            repository: BenchmarkRepository::default(),
            addons: vec![],
            favourite: false,
            hidden: false,
            build: BenchmarkBuild::default(),
            machine: BenchmarkMachine::default(),
            metrics: BenchmarkMetrics::default(),
            stages,
            sol: vec![],
            samples,
            stage_marks: vec![],
            log_file: None,
            log_line_count: 0,
        }
    }

    #[test]
    fn charts_drop_flat_series() {
        let record = record(
            vec![BenchmarkSample {
                t_ms: 1000,
                hash_files_done: 3,
                memory_bytes: 10,
                ..Default::default()
            }],
            vec![],
        );
        let defs = charts_for(&record, "A: ", &crate::ui::palette::BENCHMARK_SERIES);
        let keys: Vec<&str> = defs.iter().map(|def| def.key).collect();
        assert_eq!(keys, vec!["hashing", "memory"]);
        assert_eq!(defs[0].series[0].label, "A: Files");
        assert_eq!(defs[0].series[0].points, vec![[1.0, 3.0]]);
    }

    #[test]
    fn comparison_identity_is_consistent_across_metric_charts() {
        let rec = record(
            vec![BenchmarkSample {
                t_ms: 1000,
                download_bps: 10.0,
                disk_write_bps: 20.0,
                downloaded_bytes: 30,
                hash_files_done: 4,
                memory_bytes: 50,
                cpu_percent: 60.0,
                progress_percent: 70.0,
                ..Default::default()
            }],
            vec![],
        );
        let expected_keys = [
            "download_rate",
            "disk_write_rate",
            "downloaded",
            "hashing",
            "memory",
            "cpu",
            "progress",
        ];
        for (style, prefix, color) in [
            (
                SeriesStyle::ComparisonA,
                "A: ",
                crate::ui::palette::BENCHMARK_COMPARE_A,
            ),
            (
                SeriesStyle::ComparisonB,
                "B: ",
                crate::ui::palette::BENCHMARK_COMPARE_B,
            ),
        ] {
            let charts = comparison_charts_for(&rec, style);
            let keys: Vec<_> = charts.iter().map(|chart| chart.key).collect();
            assert_eq!(keys, expected_keys);
            for chart in charts {
                let series = &chart.series[0];
                assert!(series.label.starts_with(prefix));
                assert_eq!(series.color, color);
                assert_eq!(series.style, style);
            }
        }
    }

    #[test]
    fn comparison_rate_charts_filter_independently() {
        let rec = record(
            vec![BenchmarkSample {
                t_ms: 1000,
                download_bps: 10.0,
                ..Default::default()
            }],
            vec![],
        );
        let charts = comparison_charts_for(&rec, SeriesStyle::ComparisonA);
        assert!(charts.iter().any(|chart| chart.key == "download_rate"));
        assert!(!charts.iter().any(|chart| chart.key == "disk_write_rate"));

        let disk_only = record(
            vec![BenchmarkSample {
                t_ms: 1000,
                disk_write_bps: 10.0,
                ..Default::default()
            }],
            vec![],
        );
        let charts = comparison_charts_for(&disk_only, SeriesStyle::ComparisonB);
        assert!(!charts.iter().any(|chart| chart.key == "download_rate"));
        assert!(charts.iter().any(|chart| chart.key == "disk_write_rate"));
        assert!(
            comparison_charts_for(&record(vec![], vec![]), SeriesStyle::ComparisonA).is_empty()
        );
    }

    #[test]
    fn cpu_scale_uses_thread_count_when_known() {
        let mut rec = record(vec![], vec![]);
        assert_eq!(cpu_scale(&rec), 1.0);
        assert_eq!(cpu_title(&rec), "CPU (one core = 100%)");
        rec.machine.cpu_threads = 4;
        assert_eq!(cpu_scale(&rec), 0.25);
        assert_eq!(cpu_title(&rec), "CPU (all cores)");
    }

    #[test]
    fn stage_rows_align_by_name() {
        let a = record(
            vec![],
            vec![
                BenchmarkStage {
                    name: "hash".into(),
                    seconds: 2.0,
                    details: String::new(),
                },
                BenchmarkStage {
                    name: "download".into(),
                    seconds: 5.0,
                    details: String::new(),
                },
            ],
        );
        let b = record(
            vec![],
            vec![BenchmarkStage {
                name: "download".into(),
                seconds: 4.0,
                details: String::new(),
            }],
        );
        let rows = stage_rows(&[&a, &b]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "hash");
        assert_eq!(rows[0].values, vec![2.0, 0.0]);
        assert_eq!(rows[1].values, vec![5.0, 4.0]);
    }

    #[test]
    fn stage_rows_drop_stages_that_are_zero_everywhere() {
        let a = record(
            vec![],
            vec![
                BenchmarkStage {
                    name: "create_context".into(),
                    seconds: 0.0,
                    details: String::new(),
                },
                BenchmarkStage {
                    name: "download".into(),
                    seconds: 5.0,
                    details: String::new(),
                },
            ],
        );
        let b = record(
            vec![],
            vec![BenchmarkStage {
                name: "create_context".into(),
                seconds: 0.002,
                details: String::new(),
            }],
        );
        assert_eq!(stage_rows(&[&a]).len(), 1);
        assert_eq!(stage_rows(&[&a, &b]).len(), 2);
    }

    #[test]
    fn stage_rows_leave_out_the_enclosing_action_total() {
        let a = record(
            vec![],
            vec![
                BenchmarkStage {
                    name: "tree_hash_bootstrap".into(),
                    seconds: 822.0,
                    details: String::new(),
                },
                BenchmarkStage {
                    name: "download_skip_after_quick_verify".into(),
                    seconds: 829.0,
                    details: String::new(),
                },
            ],
        );
        let rows = stage_rows(&[&a]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "tree_hash_bootstrap");
    }
}
