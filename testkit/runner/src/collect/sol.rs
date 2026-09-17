use serde_json::{Map, Value};

/// `key=value` pairs of one log line. Integers stay exact (`u64`/`i64`), so a
/// byte counter never round-trips through `f64`; decimals and unit-suffixed
/// numbers (`0.125s`, `3x`, `97%`) become `f64`; anything else stays text.
/// Malformed input is retained where possible and marked with parser-owned
/// status fields.
pub fn key_values(line: &str) -> Value {
    let (fields, errors) = parse_fields(line);
    let mut result = Map::new();
    for (key, raw) in fields {
        result.insert(key, number_or_text(&raw));
    }
    if !errors.is_empty() {
        result.insert("parse_status".to_owned(), "malformed".into());
        result.insert("parse_error".to_owned(), errors.join(",").into());
    }
    Value::Object(result)
}

const RESERVED_SOL_KEYS: &[&str] = &[
    "op",
    "actual_s",
    "work_bytes",
    "actual_bps",
    "light_bps",
    "ideal_s",
    "sol",
    "light_src",
    "actual_ns",
    "sol_raw",
    "metric_kind",
    "reference_status",
    "metric_version",
    "parse_status",
    "parse_error",
];

fn parse_fields(line: &str) -> (Vec<(String, String)>, Vec<&'static str>) {
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut rest = line.trim_start();
    let mut errors = Vec::new();
    while !rest.is_empty() {
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let Some(eq) = rest.find('=') else {
            push_parse_error(&mut errors, "malformed_token");
            break;
        };
        if eq > token_end {
            push_parse_error(&mut errors, "malformed_token");
            rest = rest[token_end..].trim_start();
            continue;
        }
        let key = &rest[..eq];
        let valid_key = key
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'));
        let after = &rest[eq + 1..];
        let (value, remainder) = if let Some(mut quoted) = after.strip_prefix('"') {
            let mut value = String::new();
            let mut closed = false;
            while !quoted.is_empty() {
                let ch = quoted.chars().next().expect("non-empty quoted value");
                quoted = &quoted[ch.len_utf8()..];
                match ch {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match quoted.chars().next() {
                        Some(escaped @ ('"' | '\\')) => {
                            value.push(escaped);
                            quoted = &quoted[escaped.len_utf8()..];
                        }
                        Some(other) => {
                            push_parse_error(&mut errors, "invalid_escape");
                            value.push('\\');
                            value.push(other);
                            quoted = &quoted[other.len_utf8()..];
                        }
                        None => {
                            push_parse_error(&mut errors, "unterminated_escape");
                            value.push('\\');
                        }
                    },
                    _ => value.push(ch),
                }
            }
            if !closed {
                push_parse_error(&mut errors, "unterminated_quote");
                (value, "")
            } else if quoted.is_empty() || quoted.chars().next().is_some_and(char::is_whitespace) {
                (value, quoted)
            } else {
                push_parse_error(&mut errors, "trailing_characters");
                let end = quoted.find(char::is_whitespace).unwrap_or(quoted.len());
                (value, &quoted[end..])
            }
        } else {
            match after.find(char::is_whitespace) {
                Some(end) => (after[..end].to_owned(), &after[end..]),
                None => (after.to_owned(), ""),
            }
        };
        if !valid_key {
            push_parse_error(&mut errors, "invalid_key");
        } else if let Some((_, existing)) = fields.iter_mut().find(|(name, _)| name == key) {
            if RESERVED_SOL_KEYS.contains(&key) {
                push_parse_error(&mut errors, "duplicate_reserved_key");
            }
            *existing = value;
        } else {
            fields.push((key.to_owned(), value));
        }
        rest = remainder.trim_start();
    }
    (fields, errors)
}

fn push_parse_error(errors: &mut Vec<&'static str>, error: &'static str) {
    if !errors.contains(&error) {
        errors.push(error);
    }
}

fn number_or_text(raw: &str) -> Value {
    if let Ok(integer) = raw.parse::<u64>() {
        return Value::from(integer);
    }
    if let Ok(integer) = raw.parse::<i64>() {
        return Value::from(integer);
    }
    let numeric = raw.trim_end_matches(['x', '%', 's']);
    numeric
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map_or_else(|| Value::String(raw.to_owned()), Value::from)
}

/// Every `SOL op=...` record of the slice, in log order, with the raw line
/// under `raw`. A GUI-harness slice carries each event twice (the file line
/// and the driver's un-timestamped echo); only the canonical event lines are
/// parsed, so two genuine repeated operations still yield two records.
pub fn parse(text: &str) -> Vec<Value> {
    super::event_lines(text)
        .filter_map(|line| {
            line.find("SOL op=").map(|index| {
                let raw = line[index..].trim();
                let mut record = key_values(&raw["SOL ".len()..]);
                record["raw"] = raw.into();
                record
            })
        })
        .collect()
}

/// The last record of `name`: the legacy per-operation view, which on a
/// download row is one arbitrary hash batch. Use `aggregate` for totals.
pub fn operation(records: &[Value], name: &str) -> Value {
    records
        .iter()
        .rev()
        .find(|row| row["op"] == name)
        .cloned()
        .unwrap_or(Value::Null)
}

fn interval_metrics(rows: &[&Value]) -> (Option<f64>, Option<f64>) {
    let mut intervals: Vec<(u64, u64)> = rows
        .iter()
        .filter_map(|row| {
            let start = row["start_offset_ns"].as_u64()?;
            let end = row["end_offset_ns"].as_u64()?;
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

/// Totals over every record of `name`: run count, summed `actual_s` (service
/// time, not makespan), summed `work_bytes`, the derived rate, and the set of
/// outcomes. Null when the operation never logged.
pub fn aggregate(records: &[Value], name: &str) -> Value {
    let rows: Vec<&Value> = records.iter().filter(|row| row["op"] == name).collect();
    if rows.is_empty() {
        return Value::Null;
    }
    let actual_s: f64 = rows
        .iter()
        .map(|row| {
            row["actual_ns"]
                .as_f64()
                .map(|ns| ns / 1e9)
                .or_else(|| row["actual_s"].as_f64())
                .unwrap_or(0.0)
        })
        .sum();
    let work_bytes: u64 = rows
        .iter()
        .filter_map(|row| row["work_bytes"].as_u64())
        .sum();
    let useful_work_bytes: u64 = rows
        .iter()
        .filter_map(|row| {
            row["useful_output_bytes"]
                .as_u64()
                .or_else(|| row["work_bytes"].as_u64())
        })
        .sum();
    let (interval_coverage_s, makespan_s) = interval_metrics(&rows);
    let rated = rows.iter().filter(|row| row["sol"].is_number()).count();
    let mut outcomes: Vec<&str> = Vec::new();
    for row in &rows {
        if let Some(outcome) = row["outcome"].as_str()
            && !outcomes.contains(&outcome)
        {
            outcomes.push(outcome);
        }
    }
    let completed = outcomes
        .iter()
        .all(|outcome| !matches!(*outcome, "cancelled" | "failed"));
    let actual_bps = (work_bytes > 0 && actual_s > 0.0).then(|| work_bytes as f64 / actual_s);
    let reference_statuses: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["reference_status"].as_str())
        .fold(Vec::new(), |mut values, value| {
            if !values.contains(&value) {
                values.push(value);
            }
            values
        });
    let metric_versions: Vec<u64> = rows
        .iter()
        .filter_map(|row| row["metric_version"].as_u64())
        .fold(Vec::new(), |mut values, value| {
            if !values.contains(&value) {
                values.push(value);
            }
            values
        });
    let malformed_records = rows
        .iter()
        .filter(|row| row["parse_status"] == "malformed")
        .count();
    let mixed_metric_versions = metric_versions.len() > 1;
    serde_json::json!({
        "runs": rows.len(),
        "rated_runs": rated,
        "actual_s": (actual_s * 1e6).round_ties_even() / 1e6,
        "interval_coverage_s": interval_coverage_s,
        "makespan_s": makespan_s,
        "work_bytes": work_bytes,
        "useful_work_bytes": useful_work_bytes,
        "actual_bps": actual_bps.map(|v| v.round()),
        "reference_statuses": reference_statuses,
        "metric_versions": metric_versions,
        "mixed_metric_versions": mixed_metric_versions,
        "malformed_records": malformed_records,
        "outcomes": outcomes,
        "completed": completed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn units_and_quotes() {
        let row = key_values("op=hash actual_s=0.125s sol=3x name=\"two words\" percent=97%");
        assert_eq!(row["actual_s"], 0.125);
        assert_eq!(row["name"], "two words");
        assert_eq!(row["percent"], 97.0);
    }
    #[test]
    fn integers_stay_exact_and_malformed_numbers_stay_text() {
        let row = key_values("bytes=18446744073709551615 neg=-5 bad=1.2.3 na=na dup=1 dup=2");
        assert_eq!(row["bytes"], u64::MAX);
        assert_eq!(row["bytes"].as_u64(), Some(u64::MAX));
        assert_eq!(row["neg"], -5);
        assert_eq!(row["bad"], "1.2.3");
        assert_eq!(row["na"], "na");
        assert_eq!(row["dup"], 2);
    }
    #[test]
    fn shared_sol_grammar_corpus_is_byte_stable() {
        let corpus: Value =
            serde_json::from_str(include_str!("../../tests/sol-grammar-corpus.json"))
                .expect("SOL grammar corpus");
        for case in corpus.as_array().expect("corpus array") {
            let name = case["name"].as_str().expect("case name");
            let input = case["input"].as_str().expect("case input");
            let expected: std::collections::BTreeMap<String, String> =
                serde_json::from_value(case["fields"].clone()).expect("expected fields");
            let expected_status = case["status"].as_str().expect("expected status");
            let expected_error = case["error"].as_str().expect("expected error");
            let first = parse_fields(input);
            let second = parse_fields(input);
            let actual: std::collections::BTreeMap<_, _> = first.0.iter().cloned().collect();
            assert_eq!(actual, expected, "{name}");
            assert_eq!(
                if first.1.is_empty() {
                    "ok".to_owned()
                } else {
                    "malformed".to_owned()
                },
                expected_status,
                "{name}"
            );
            assert_eq!(first.1.join(","), expected_error, "{name}");
            assert_eq!(
                serde_json::to_vec(&first).expect("serialize first parse"),
                serde_json::to_vec(&second).expect("serialize second parse"),
                "{name}"
            );
            let public = key_values(input);
            if expected_status == "malformed" {
                assert_eq!(public["parse_status"], "malformed", "{name}");
                assert_eq!(public["parse_error"], expected_error, "{name}");
            } else {
                assert!(public.get("parse_status").is_none(), "{name}");
            }
        }
    }
    #[test]
    fn last_operation_wins() {
        let rows = parse("prefix SOL op=hash sol=1x\nSOL op=hash sol=2x\nother");
        assert_eq!(operation(&rows, "hash")["sol"], 2.0);
        assert!(operation(&rows, "download").is_null());
    }
    #[test]
    fn echoed_events_are_parsed_once_but_repeated_events_twice() {
        let echoed = parse(
            "[2026-09-14 08:00:00.000000 +02:00] INFO  [m] SOL op=hash actual_s=1 work_bytes=100\nSOL op=hash actual_s=1 work_bytes=100",
        );
        assert_eq!(echoed.len(), 1);
        let repeated = parse(
            "[2026-09-14 08:00:00.000000 +02:00] INFO  [m] SOL op=hash actual_s=1 work_bytes=100\n[2026-09-14 08:00:01.000000 +02:00] INFO  [m] SOL op=hash actual_s=1 work_bytes=100",
        );
        assert_eq!(repeated.len(), 2);
        let legacy = parse("SOL op=hash actual_s=1\nSOL op=hash actual_s=2");
        assert_eq!(legacy.len(), 2);
    }
    #[test]
    fn aggregate_sums_every_run_while_operation_keeps_the_last() {
        let rows = parse(
            "SOL op=hash actual_s=30.000 work_bytes=4000000000 sol=na label=a\nSOL op=hash actual_s=0.063 actual_ns=63000000 work_bytes=424000000 sol=na label=b",
        );
        assert_eq!(operation(&rows, "hash")["actual_s"], 0.063);
        let total = aggregate(&rows, "hash");
        assert_eq!(total["runs"], 2);
        assert_eq!(total["rated_runs"], 0);
        assert_eq!(total["actual_s"], 30.063);
        assert_eq!(total["work_bytes"], 4_424_000_000_u64);
        assert_eq!(total["completed"], true);
        assert!(aggregate(&rows, "download").is_null());
        let cancelled = parse("SOL op=download actual_s=1 sol=0.5 outcome=cancelled");
        let total = aggregate(&cancelled, "download");
        assert_eq!(total["rated_runs"], 1);
        assert_eq!(total["completed"], false);
        assert_eq!(total["outcomes"], serde_json::json!(["cancelled"]));
    }

    #[test]
    fn aggregate_separates_service_coverage_and_makespan() {
        let rows = parse(
            "SOL op=hash actual_s=2 actual_ns=2000000000 work_bytes=20 start_offset_ns=0 end_offset_ns=2000000000 metric_version=2 reference_status=ok\n\
             SOL op=hash actual_s=2 actual_ns=2000000000 work_bytes=20 start_offset_ns=1000000000 end_offset_ns=3000000000 metric_version=2 reference_status=ok\n\
             SOL op=hash actual_s=1 actual_ns=1000000000 work_bytes=10 start_offset_ns=4000000000 end_offset_ns=5000000000 metric_version=2 reference_status=missing",
        );
        let total = aggregate(&rows, "hash");
        assert_eq!(total["actual_s"], 5.0);
        assert_eq!(total["interval_coverage_s"], 4.0);
        assert_eq!(total["makespan_s"], 5.0);
        assert_eq!(total["useful_work_bytes"], 50);
        assert_eq!(
            total["reference_statuses"],
            serde_json::json!(["ok", "missing"])
        );
        assert_eq!(total["mixed_metric_versions"], false);
    }
}
