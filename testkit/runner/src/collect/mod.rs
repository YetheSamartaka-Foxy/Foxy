pub mod expect;
pub mod logs;
pub mod memory;
pub mod metrics;
pub mod profile;
pub mod sol;

// `Collect.ps1` had no tests, so a log format change degraded a metric to null
// instead of failing. These snapshots pin every parser against real recorded
// slices under tests/log-corpus/; refresh them with `cargo insta review` only
// when the log format changed on purpose.
#[cfg(test)]
mod snapshots {
    use serde_json::{Value, json};

    fn digest(text: &str) -> Value {
        let breakdown = metrics_breakdown(text);
        let records = super::sol::parse(text);
        json!({
            "run_metrics": breakdown["run_metrics"],
            "category_line_counts": categories(&breakdown),
            "db_values": breakdown["db"]["values"],
            "sol_operations": Vec::from_iter(["download", "hash", "quick_scan"].map(|name| json!({"op": name, "record": strip_raw(&super::sol::operation(&records, name))}))),
            "sol_records": records.len(),
        })
    }

    fn metrics_breakdown(text: &str) -> Value {
        super::metrics::breakdown(text)
    }

    fn categories(breakdown: &Value) -> Value {
        let mut counts = serde_json::Map::new();
        for (name, value) in breakdown.as_object().expect("breakdown object") {
            if let Some(lines) = value.as_array() {
                counts.insert(name.clone(), lines.len().into());
            }
        }
        counts.insert(
            "db".into(),
            breakdown["db"]["lines"]
                .as_array()
                .map_or(0, Vec::len)
                .into(),
        );
        Value::Object(counts)
    }

    fn strip_raw(record: &Value) -> Value {
        let mut record = record.clone();
        if let Some(object) = record.as_object_mut() {
            object.remove("raw");
        }
        record
    }

    fn corpus(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/log-corpus")
            .join(format!("{name}.log"));
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    #[test]
    fn remote_refresh() {
        insta::assert_json_snapshot!(digest(&corpus("remote-refresh")));
    }
    #[test]
    fn download() {
        insta::assert_json_snapshot!(digest(&corpus("download")));
    }
    #[test]
    fn delta_patch() {
        insta::assert_json_snapshot!(digest(&corpus("delta-patch")));
    }
    #[test]
    fn write_gate() {
        insta::assert_json_snapshot!(digest(&corpus("write-gate")));
    }
    #[test]
    fn hdd_download() {
        insta::assert_json_snapshot!(digest(&corpus("hdd-download")));
    }
}
