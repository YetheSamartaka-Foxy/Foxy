use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default)]
pub struct AggregationInput<'a> {
    pub actual_s: Option<f64>,
    pub work_bytes: Option<u64>,
    pub useful_work_bytes: Option<u64>,
    pub rated: bool,
    pub start_offset_ns: Option<u64>,
    pub end_offset_ns: Option<u64>,
    pub reference_id: Option<&'a str>,
    pub reference_status: Option<&'a str>,
    pub metric_version: Option<u64>,
    pub malformed: bool,
    pub outcome: Option<&'a str>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Aggregation {
    pub runs: usize,
    pub rated_runs: usize,
    pub service_s: f64,
    pub interval_coverage_s: Option<f64>,
    pub makespan_s: Option<f64>,
    pub work_bytes: u64,
    pub useful_work_bytes: u64,
    pub actual_bps: Option<f64>,
    pub reference_ids: Vec<String>,
    pub reference_statuses: Vec<String>,
    pub metric_versions: Vec<u64>,
    pub malformed_records: usize,
    pub outcomes: Vec<String>,
    pub completed: bool,
}

fn push_distinct<T: Ord + Clone>(values: &mut Vec<T>, seen: &mut BTreeSet<T>, value: T) {
    if seen.insert(value.clone()) {
        values.push(value);
    }
}

fn interval_metrics(records: &[AggregationInput<'_>]) -> (Option<f64>, Option<f64>) {
    let mut intervals: Vec<(u64, u64)> = records
        .iter()
        .filter_map(|record| {
            let start = record.start_offset_ns?;
            let end = record.end_offset_ns?;
            (end >= start).then_some((start, end))
        })
        .collect();
    if intervals.is_empty() {
        return (None, None);
    }
    intervals.sort_unstable();
    let first = intervals[0].0;
    let last = intervals.iter().map(|(_, end)| *end).max().unwrap_or(first);
    let mut covered = 0_u64;
    let (mut start, mut end) = intervals[0];
    for (next_start, next_end) in intervals.into_iter().skip(1) {
        if next_start <= end {
            end = end.max(next_end);
        } else {
            covered = covered.saturating_add(end.saturating_sub(start));
            (start, end) = (next_start, next_end);
        }
    }
    covered = covered.saturating_add(end.saturating_sub(start));
    (
        Some(covered as f64 / 1e9),
        Some(last.saturating_sub(first) as f64 / 1e9),
    )
}

pub fn aggregate(records: &[AggregationInput<'_>]) -> Aggregation {
    let service_s = records.iter().filter_map(|record| record.actual_s).sum();
    let work_bytes = records.iter().filter_map(|record| record.work_bytes).sum();
    let useful_work_bytes = records
        .iter()
        .map(|record| record.useful_work_bytes.or(record.work_bytes).unwrap_or(0))
        .sum();
    let (interval_coverage_s, makespan_s) = interval_metrics(records);
    let mut reference_ids = Vec::new();
    let mut reference_id_set = BTreeSet::new();
    let mut reference_statuses = Vec::new();
    let mut reference_status_set = BTreeSet::new();
    let mut metric_versions = Vec::new();
    let mut metric_version_set = BTreeSet::new();
    let mut outcomes = Vec::new();
    let mut outcome_set = BTreeSet::new();
    for record in records {
        if let Some(value) = record.reference_id {
            push_distinct(&mut reference_ids, &mut reference_id_set, value.to_owned());
        }
        if let Some(value) = record.reference_status {
            push_distinct(
                &mut reference_statuses,
                &mut reference_status_set,
                value.to_owned(),
            );
        }
        if let Some(value) = record.metric_version {
            push_distinct(&mut metric_versions, &mut metric_version_set, value);
        }
        if let Some(value) = record.outcome {
            push_distinct(&mut outcomes, &mut outcome_set, value.to_owned());
        }
    }
    let completed = outcomes
        .iter()
        .all(|outcome| outcome != "cancelled" && !outcome.starts_with("failed"));
    Aggregation {
        runs: records.len(),
        rated_runs: records.iter().filter(|record| record.rated).count(),
        service_s,
        interval_coverage_s,
        makespan_s,
        work_bytes,
        useful_work_bytes,
        actual_bps: (work_bytes > 0 && service_s > 0.0).then(|| work_bytes as f64 / service_s),
        reference_ids,
        reference_statuses,
        metric_versions,
        malformed_records: records.iter().filter(|record| record.malformed).count(),
        outcomes,
        completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Corpus {
        cases: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        records: Vec<Record>,
        expected: Expected,
    }

    #[derive(Deserialize)]
    struct Record {
        actual_s: Option<f64>,
        work_bytes: Option<u64>,
        useful_work_bytes: Option<u64>,
        rated: Option<bool>,
        start_offset_ns: Option<u64>,
        end_offset_ns: Option<u64>,
        reference_id: Option<String>,
        reference_status: Option<String>,
        metric_version: Option<u64>,
        malformed: Option<bool>,
        outcome: Option<String>,
    }

    #[derive(Deserialize)]
    struct Expected {
        service_s: f64,
        interval_coverage_s: Option<f64>,
        makespan_s: Option<f64>,
        useful_work_bytes: u64,
        rated_runs: usize,
        reference_ids: Vec<String>,
        reference_statuses: Vec<String>,
        metric_versions: Vec<u64>,
        malformed_records: usize,
        outcomes: Vec<String>,
        completed: bool,
    }

    #[test]
    fn golden_aggregation_contract() {
        let corpus: Corpus = serde_json::from_str(include_str!("../tests/aggregation-corpus.json"))
            .expect("aggregation corpus");
        for case in corpus.cases {
            let records: Vec<_> = case
                .records
                .iter()
                .map(|record| AggregationInput {
                    actual_s: record.actual_s,
                    work_bytes: record.work_bytes,
                    useful_work_bytes: record.useful_work_bytes,
                    rated: record.rated.unwrap_or(false),
                    start_offset_ns: record.start_offset_ns,
                    end_offset_ns: record.end_offset_ns,
                    reference_id: record.reference_id.as_deref(),
                    reference_status: record.reference_status.as_deref(),
                    metric_version: record.metric_version,
                    malformed: record.malformed.unwrap_or(false),
                    outcome: record.outcome.as_deref(),
                })
                .collect();
            let actual = aggregate(&records);
            assert_eq!(actual.service_s, case.expected.service_s, "{}", case.name);
            assert_eq!(
                actual.interval_coverage_s, case.expected.interval_coverage_s,
                "{}",
                case.name
            );
            assert_eq!(actual.makespan_s, case.expected.makespan_s, "{}", case.name);
            assert_eq!(
                actual.useful_work_bytes, case.expected.useful_work_bytes,
                "{}",
                case.name
            );
            assert_eq!(actual.rated_runs, case.expected.rated_runs, "{}", case.name);
            assert_eq!(
                actual.reference_ids, case.expected.reference_ids,
                "{}",
                case.name
            );
            assert_eq!(
                actual.reference_statuses, case.expected.reference_statuses,
                "{}",
                case.name
            );
            assert_eq!(
                actual.metric_versions, case.expected.metric_versions,
                "{}",
                case.name
            );
            assert_eq!(
                actual.malformed_records, case.expected.malformed_records,
                "{}",
                case.name
            );
            assert_eq!(actual.outcomes, case.expected.outcomes, "{}", case.name);
            assert_eq!(actual.completed, case.expected.completed, "{}", case.name);
        }
    }
}
