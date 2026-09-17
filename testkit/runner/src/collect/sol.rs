use serde_json::{Map, Value};

/// `key=value` pairs of one log line. Integers stay exact (`u64`/`i64`), so a
/// byte counter never round-trips through `f64`; decimals and unit-suffixed
/// numbers (`0.125s`, `3x`, `97%`) become `f64`; anything else stays text.
pub fn key_values(line: &str) -> Value {
    let pattern = regex::Regex::new(r#"([A-Za-z][A-Za-z0-9_.-]*)=("[^"]*"|[^\s,()]+)"#).unwrap();
    let mut result = Map::new();
    for capture in pattern.captures_iter(line) {
        let raw = capture[2].trim_matches('"').trim_end_matches(',');
        result.insert(capture[1].to_owned(), number_or_text(raw));
    }
    Value::Object(result)
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
                let mut record = key_values(raw);
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
    serde_json::json!({
        "runs": rows.len(),
        "rated_runs": rated,
        "actual_s": (actual_s * 1e6).round_ties_even() / 1e6,
        "work_bytes": work_bytes,
        "actual_bps": actual_bps.map(|v| v.round()),
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
}
