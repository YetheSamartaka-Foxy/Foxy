//! Parser for the app's `FOXY_PROFILE` report and for the pipeline stage table.
//!
//! The app emits `PROFILE ...` lines only when profiling is enabled, so an
//! unprofiled run parses to a null profile rather than to an error. The stage
//! table is always present and is parsed here too, because it names far more
//! phases than the profiler's four coarse ones.

use regex::Regex;
use serde_json::{Value, json};

/// Parse every profiler artifact out of one operation's log slice.
pub fn parse(text: &str) -> Value {
    let totals = totals(text);
    if totals.is_null() {
        return json!({"profiled": false, "stages": stages(text)});
    }
    json!({
        "profiled": true,
        "totals": totals,
        "phases": phases(text),
        "db": db(text),
        "fs": fs(text),
        "stages": stages(text),
    })
}

fn number(raw: &str) -> Value {
    raw.parse::<f64>().map_or(Value::Null, |value| {
        serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
    })
}

fn totals(text: &str) -> Value {
    let re = Regex::new(
        r"PROFILE totals context=(?<context>.*?) wall_s=(?<wall>[0-9.]+) phase_s=(?<phase>[0-9.]+) unattributed_s=(?<unattributed>[0-9.]+) db_read_calls=(?<read_calls>\d+) db_read_s=(?<read_s>[0-9.]+) db_read_rows=(?<read_rows>\d+) db_write_calls=(?<write_calls>\d+) db_write_s=(?<write_s>[0-9.]+) fs_calls=(?<fs_calls>\d+) fs_s=(?<fs_s>[0-9.]+) fs_units=(?<fs_units>\d+)",
    )
    .expect("totals pattern");
    let Some(caps) = re.captures(text) else {
        return Value::Null;
    };
    json!({
        "context": caps["context"].trim(),
        "wall_s": number(&caps["wall"]),
        "phase_s": number(&caps["phase"]),
        "unattributed_s": number(&caps["unattributed"]),
        "db_read_calls": number(&caps["read_calls"]),
        "db_read_s": number(&caps["read_s"]),
        "db_read_rows": number(&caps["read_rows"]),
        "db_write_calls": number(&caps["write_calls"]),
        "db_write_s": number(&caps["write_s"]),
        "fs_calls": number(&caps["fs_calls"]),
        "fs_s": number(&caps["fs_s"]),
        "fs_units": number(&caps["fs_units"]),
    })
}

fn phases(text: &str) -> Value {
    let re = Regex::new(
        r"PROFILE phase name=(?<name>\S+) elapsed_s=(?<elapsed>[0-9.]+) share=(?<share>[0-9.]+)",
    )
    .expect("phase pattern");
    Value::Array(
        re.captures_iter(text)
            .map(|caps| {
                json!({
                    "name": &caps["name"],
                    "elapsed_s": number(&caps["elapsed"]),
                    "share": number(&caps["share"]),
                })
            })
            .collect(),
    )
}

fn db(text: &str) -> Value {
    let re = Regex::new(
        r"PROFILE db phase=(?<phase>\S+) kind=(?<kind>\S+) label=(?<label>.*?) calls=(?<calls>\d+) rows=(?<rows>\d+) elapsed_s=(?<elapsed>[0-9.]+) max_ms=(?<max>[0-9.]+)",
    )
    .expect("db pattern");
    Value::Array(
        re.captures_iter(text)
            .map(|caps| {
                json!({
                    "phase": &caps["phase"],
                    "kind": &caps["kind"],
                    "label": &caps["label"],
                    "calls": number(&caps["calls"]),
                    "rows": number(&caps["rows"]),
                    "elapsed_s": number(&caps["elapsed"]),
                    "max_ms": number(&caps["max"]),
                })
            })
            .collect(),
    )
}

fn fs(text: &str) -> Value {
    let re = Regex::new(
        r"PROFILE fs phase=(?<phase>\S+) op=(?<op>\S+) calls=(?<calls>\d+) units=(?<units>\d+) elapsed_s=(?<elapsed>[0-9.]+) max_ms=(?<max>[0-9.]+)",
    )
    .expect("fs pattern");
    Value::Array(
        re.captures_iter(text)
            .map(|caps| {
                json!({
                    "phase": &caps["phase"],
                    "op": &caps["op"],
                    "calls": number(&caps["calls"]),
                    "units": number(&caps["units"]),
                    "elapsed_s": number(&caps["elapsed"]),
                    "max_ms": number(&caps["max"]),
                })
            })
            .collect(),
    )
}

/// Rows of the app's `PIPELINE SUMMARY` table. The logger flattens the table's
/// newlines into one record, so rows are found by pattern rather than by line,
/// and a row's details run to the start of the next row. Names repeat across a
/// run, so rows are summed per name and counted.
fn stages(text: &str) -> Value {
    let re = Regex::new(r"(?<name>[a-z][a-z0-9_]{2,})\s{2,}(?<seconds>\d+\.\d+)s")
        .expect("stage pattern");
    let found: Vec<_> = re.captures_iter(text).collect();
    let mut names: Vec<String> = Vec::new();
    let mut totals: Vec<(f64, u64, String)> = Vec::new();
    for (index, caps) in found.iter().enumerate() {
        let whole = caps.get(0).expect("whole match");
        let next = found
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map_or(text.len(), |m| m.start());
        let details = text[whole.end()..next]
            .split("---")
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let name = caps["name"].to_owned();
        let seconds = caps["seconds"].parse::<f64>().unwrap_or(0.0);
        match names.iter().position(|known| *known == name) {
            Some(at) => {
                totals[at].0 += seconds;
                totals[at].1 += 1;
                if totals[at].2.is_empty() {
                    totals[at].2 = details;
                }
            }
            None => {
                names.push(name);
                totals.push((seconds, 1, details));
            }
        }
    }
    Value::Array(
        names
            .into_iter()
            .zip(totals)
            .map(|(name, (seconds, count, details))| {
                json!({
                    "name": name,
                    "elapsed_s": number(&format!("{seconds:.3}")),
                    "occurrences": count,
                    "details": details,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[ts] INFO  [Foxy::core::utils::profiling] PROFILE totals context=op=cli-sync-0001 mode=Download outcome=completed wall_s=10.412 phase_s=10.401 unattributed_s=0.011 db_read_calls=1204 db_read_s=0.512 db_read_rows=45011 db_write_calls=1893 db_write_s=6.204 fs_calls=2604 fs_s=1.884 fs_units=43644768
[ts] INFO  [Foxy::core::utils::profiling] PROFILE phase name=purge elapsed_s=0.361 share=0.035
[ts] INFO  [Foxy::core::utils::profiling] PROFILE phase name=download elapsed_s=5.732 share=0.551
[ts] INFO  [Foxy::core::utils::profiling] PROFILE db phase=download kind=write label=insert subfiles calls=1693 rows=433248 elapsed_s=5.601 max_ms=41.200
[ts] INFO  [Foxy::core::utils::profiling] PROFILE db phase=pre-download kind=read label=select subfiles calls=96 rows=0 elapsed_s=0.014 max_ms=1.100
[ts] INFO  [Foxy::core::utils::profiling] PROFILE fs phase=download op=write calls=1402 units=43644768 elapsed_s=0.902 max_ms=12.500
 create_context           0.01s    download                 5.73s  files=864, bytes=43644768  download                 1.00s  files=1";

    #[test]
    fn totals_carry_every_field() {
        let parsed = parse(SAMPLE);
        assert_eq!(parsed["profiled"], json!(true));
        assert_eq!(parsed["totals"]["wall_s"], json!(10.412));
        assert_eq!(parsed["totals"]["db_write_calls"], json!(1893.0));
        assert_eq!(
            parsed["totals"]["context"],
            json!("op=cli-sync-0001 mode=Download outcome=completed")
        );
    }

    #[test]
    fn db_labels_keep_their_space() {
        let parsed = parse(SAMPLE);
        let rows = parsed["db"].as_array().expect("db rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["label"], json!("insert subfiles"));
        assert_eq!(rows[0]["rows"], json!(433248.0));
        assert_eq!(rows[1]["kind"], json!("read"));
    }

    #[test]
    fn fs_rows_are_parsed() {
        let parsed = parse(SAMPLE);
        let rows = parsed["fs"].as_array().expect("fs rows");
        assert_eq!(rows[0]["op"], json!("write"));
        assert_eq!(rows[0]["units"], json!(43644768.0));
    }

    #[test]
    fn repeated_stage_rows_are_summed_and_counted() {
        let parsed = parse(SAMPLE);
        let stages = parsed["stages"].as_array().expect("stages");
        let download = stages
            .iter()
            .find(|stage| stage["name"] == json!("download"))
            .expect("download stage");
        assert_eq!(download["occurrences"], json!(2));
        assert_eq!(download["elapsed_s"], json!(6.73));
    }

    #[test]
    fn an_unprofiled_log_is_reported_as_such() {
        let parsed = parse("nothing to see here");
        assert_eq!(parsed["profiled"], json!(false));
        assert!(parsed["totals"].is_null());
    }
}
