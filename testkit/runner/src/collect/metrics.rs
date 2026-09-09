use serde_json::{Value, json};

pub fn run_metrics(text: &str) -> Value {
    let mut result = json!({"files":null,"bytes":null,"db_write_time_ms":null,"lock_retries":null,"total_backoff_ms":null,"elapsed_ms":null,"permit_wait_ms_total":null,"write_calls_total":null,"write_failures_total":null,"write_retries_total":null,"checkpoint_total_s":null});
    let pattern =
        regex::Regex::new(r"TOTAL DOWNLOAD total:\s*files=(\d+)\s+bytes=([\d.]+)\s*([KMGT]?i?B)")
            .unwrap();
    if let Some(c) = pattern.captures(text) {
        let scale = match &c[3] {
            "KiB" => 1024.0,
            "MiB" => 1048576.0,
            "GiB" => 1073741824.0,
            "TiB" => 1099511627776.0,
            _ => 1.0,
        };
        result["files"] = c[1].parse::<u64>().unwrap().into();
        result["bytes"] = (c[2].parse::<f64>().unwrap() * scale)
            .round_ties_even()
            .into();
    }
    let pattern=regex::Regex::new(r"sqlite: mode=\w+ lock_retries=(\d+) avg_backoff_ms=[\d.]+ total_backoff_ms=(\d+) db_write_time_ms=([\d.]+)(?: [a-z_]+=[^ ]+)* elapsed_ms=(\d+)").unwrap();
    if let Some(c) = pattern.captures(text) {
        for (key, index) in [
            ("lock_retries", 1),
            ("total_backoff_ms", 2),
            ("db_write_time_ms", 3),
            ("elapsed_ms", 4),
        ] {
            result[key] = c[index].parse::<f64>().unwrap().into();
        }
    }
    let pattern=regex::Regex::new(r"calls=(\d+) committed=\d+ failed=(\d+) retries=(\d+) backoff_ms=\d+ permit_wait_ms=([\d.]+)").unwrap();
    let mut sums = [0.0; 4];
    let mut seen = false;
    for c in pattern.captures_iter(text) {
        seen = true;
        for i in 0..4 {
            sums[i] += c[i + 1].parse::<f64>().unwrap();
        }
    }
    if seen {
        for (key, index) in [
            ("write_calls_total", 0),
            ("write_failures_total", 1),
            ("write_retries_total", 2),
            ("permit_wait_ms_total", 3),
        ] {
            result[key] = if index == 3 {
                (sums[index] * 10.0).round_ties_even() / 10.0
            } else {
                sums[index]
            }
            .into();
        }
    }
    let pattern =
        regex::Regex::new(r"checkpoint_batches=\d+ rows=\d+ statements=\d+ total=([\d.]+)s")
            .unwrap();
    if let Some(c) = pattern.captures(text) {
        result["checkpoint_total_s"] = c[1].parse::<f64>().unwrap().into();
    }
    result
}

pub fn breakdown(text: &str) -> Value {
    let definitions = [
        (
            "network",
            r"^network:|permit_wait|p50_latency|p95_latency|active ranges|retries=",
        ),
        ("disk", r"^disk:|write avg=|write p95="),
        (
            "db",
            r"DB transaction metrics|SQLite sync metrics|DATABASE METRICS SUMMARY|Part hash persistence metrics|Repository purge completed|checkpoint",
        ),
        ("hash", r"SOL op=hash"),
        ("hash_profile", r"Hash profile auto benchmark sample:"),
        ("scan", r"SOL op=quick_scan"),
        (
            "patch",
            r"delta patch|copy_local|insert_remote|patch plan|falling back",
        ),
    ];
    let mut result = json!({"run_metrics":run_metrics(text)});
    for (name, pattern) in definitions {
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(str::trim)
            .filter(|line| re.is_match(line))
            .map(Value::from)
            .collect();
        if name == "db" {
            result[name] = json!({"values":lines.iter().map(|line|super::sol::key_values(line.as_str().unwrap())).collect::<Vec<_>>(),"lines":lines});
        } else {
            result[name] = lines.into();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn download_and_write_totals() {
        let metrics = run_metrics(
            "TOTAL DOWNLOAD total: files=2 bytes=1.5 MiB\nsqlite: mode=wal lock_retries=2 avg_backoff_ms=1.0 total_backoff_ms=2 db_write_time_ms=3.5 extra=1 elapsed_ms=12\ncalls=3 committed=3 failed=0 retries=2 backoff_ms=0 permit_wait_ms=1.25\ncalls=4 committed=4 failed=1 retries=0 backoff_ms=0 permit_wait_ms=2.25\ncheckpoint_batches=1 rows=2 statements=3 total=0.5s",
        );
        assert_eq!(metrics["bytes"], 1572864.0);
        assert_eq!(metrics["write_calls_total"], 7.0);
        assert_eq!(metrics["permit_wait_ms_total"], 3.5);
        assert_eq!(metrics["elapsed_ms"], 12.0);
    }
    #[test]
    fn missing_metrics_are_null() {
        assert!(run_metrics("unrelated")["files"].is_null());
        assert_eq!(
            breakdown("SOL op=hash actual_s=1")["hash"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
