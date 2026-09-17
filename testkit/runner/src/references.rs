//! Calibrated-estimate ratios: a row's measured time against the independent
//! references `foxy-testkit calibrate` recorded for this machine and origin.
//! The references a row may cite are chosen once per run (`select`) and kept
//! on the row, so `replay` derives the same ratios from the same inputs.

use crate::collect::expect::dotted;
use crate::ledger::round;
use serde_json::{Value, json};

/// The environment keys a calibration lane must share with the row before
/// it can be a reference for it; the origin is checked for network lanes
/// and the storage class and volume for the disk lane.
const MACHINE_KEYS: &[&str] = &["cpu", "os", "memory_gb"];

fn same_machine(lane: &Value, environment: &Value) -> bool {
    MACHINE_KEYS.iter().all(|key| {
        !lane["environment"][*key].is_null() && lane["environment"][*key] == environment[*key]
    })
}

/// The lanes of `calibration` that apply to a run in `environment`, on
/// `storage_class`, whose repository sits on `volume`: only their ids and the
/// numbers the ratios use.
pub fn select(
    calibration: &Value,
    environment: &Value,
    storage_class: &str,
    volume: Option<&str>,
) -> Value {
    let lanes = &calibration["lanes"];
    let mut references = json!({});
    for lane in ["network", "latency"] {
        let entry = &lanes[lane];
        if entry.is_null()
            || !same_machine(entry, environment)
            || entry["values"]["origin"] != environment["origin"]
        {
            continue;
        }
        references[lane] = match lane {
            "network" => json!({
                "id": entry["id"],
                "sustained_bps": entry["values"]["sustained_bps"],
                "per_connection_bps": entry["values"]["per_connection_bps"],
            }),
            _ => json!({
                "id": entry["id"],
                "fresh_request_s": entry["values"]["fresh_request_s"],
                "reused_request_s": entry["values"]["reused_request_s"],
            }),
        };
    }
    // Disk lanes are keyed by volume; a run without a repository volume has
    // no disk reference at all rather than another volume's.
    let disk = volume.map_or(&Value::Null, |volume| {
        &lanes["disk"][volume.to_ascii_lowercase()]
    });
    if !disk.is_null()
        && same_machine(disk, environment)
        && disk["values"]["storage_class"] == storage_class
    {
        references["disk"] = json!({
            "id": disk["id"],
            "unbuffered_read_bps": disk["values"]["unbuffered_read_bps"],
            "unbuffered_read_parallel_bps": disk["values"]["unbuffered_read_parallel_bps"],
            "warm_read_bps": disk["values"]["warm_read_bps"],
            "warm_read_parallel_bps": disk["values"]["warm_read_parallel_bps"],
            "durable_write_bps": disk["values"]["durable_write_bps"],
        });
    }
    for lane in ["metadata", "db"] {
        let entry = volume.map_or(&Value::Null, |volume| {
            &lanes[lane][volume.to_ascii_lowercase()]
        });
        if entry.is_null()
            || !same_machine(entry, environment)
            || entry["values"]["storage_class"] != storage_class
        {
            continue;
        }
        references[lane] = match lane {
            "metadata" => json!({
                "id": entry["id"],
                "first_pass_entries_per_s": entry["values"]["first_pass_entries_per_s"],
                "warm_entries_per_s": entry["values"]["warm_entries_per_s"],
            }),
            _ => json!({
                "id": entry["id"],
                "insert_rows_per_s": entry["values"]["insert_rows_per_s"],
                "update_rows_per_s": entry["values"]["update_rows_per_s"],
                "delete_rows_per_s": entry["values"]["delete_rows_per_s"],
            }),
        };
    }
    let hash = &lanes["hash"];
    if !hash.is_null() && same_machine(hash, environment) {
        references["hash"] = json!({
            "id": hash["id"],
            "blake3_all_cores_bps": hash["values"]["blake3_all_cores_bps"],
            "md5_all_cores_bps": hash["values"]["md5_all_cores_bps"],
        });
    }
    references
}

/// The lane ids a row cites, for the baseline's expiry check.
pub fn ids(references: &Value) -> Value {
    let mut ids = json!({});
    for (lane, entry) in references.as_object().into_iter().flatten() {
        if let Some(id) = entry["id"].as_str() {
            ids[lane] = id.into();
        }
    }
    ids
}

fn ratio(ideal_s: f64, actual_s: f64) -> Option<f64> {
    (ideal_s.is_finite() && actual_s.is_finite() && actual_s > 0.0 && ideal_s >= 0.0)
        .then(|| round(ideal_s / actual_s, 4))
}

/// Add `sol_calibrated` (unclamped, `T_reference / T_actual`) and the
/// reference id to the operation records a reference exists for:
///
/// - `download`: body bytes at the origin's sustained aggregate rate;
/// - `hash`: hashed bytes at the slower of the read lane (page cache for a
///   warm row, the device for an explicitly evicted one) and the matching
///   all-core algorithm lane, over the operation's total hash wall time;
/// - `startup_probe`: one fresh request, since every probe runs at one depth;
/// - `sync_action` on a no-change exit: one fresh index request plus a reused
///   one per further index request;
/// - `quick_scan`: walked entries at the volume's metadata rate;
///
/// Rows without a matching reference keep their records untouched.
pub fn attach(row: &mut Value) {
    let references = row["references"].clone();
    if references.as_object().is_none_or(|map| map.is_empty()) {
        return;
    }
    let network = references["network"]["sustained_bps"].as_f64();
    if let (Some(bps), Some(bytes), Some(actual)) = (
        network.filter(|bps| *bps > 0.0),
        row["download"]["work_bytes"].as_f64(),
        row["download"]["actual_s"].as_f64(),
    ) && let Some(value) = ratio(bytes / bps, actual)
    {
        row["download"]["sol_calibrated"] = value.into();
        row["download"]["reference_id"] = references["network"]["id"].clone();
    }
    let evicted = row["cache_state"] == "evicted";
    // The bound is the best the volume does under any access pattern: the
    // faster of the single-reader and the parallel lane (an NVMe gains from
    // parallel readers, a rotational disk loses to the seeks).
    let candidates: [&str; 2] = if evicted {
        ["unbuffered_read_bps", "unbuffered_read_parallel_bps"]
    } else {
        ["warm_read_bps", "warm_read_parallel_bps"]
    };
    let (read_lane, read) = candidates
        .iter()
        .filter_map(|lane| references["disk"][*lane].as_f64().map(|bps| (*lane, bps)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(("", None), |(lane, bps)| (lane, Some(bps)));
    let (algorithm, cpu) = match row["hash"]["algorithm"].as_str() {
        Some("blake3") => (
            "blake3",
            references["hash"]["blake3_all_cores_bps"].as_f64(),
        ),
        Some("md5") => ("md5", references["hash"]["md5_all_cores_bps"].as_f64()),
        _ => ("unknown", None),
    };
    let bound = match (read, cpu) {
        (Some(read), Some(cpu)) => Some(read.min(cpu)),
        (read, cpu) => read.or(cpu),
    };
    if row["cache_state"] != "cold"
        && algorithm != "unknown"
        && let (Some(bps), Some(bytes), Some(actual)) = (
            bound.filter(|bps| *bps > 0.0),
            dotted(row, "breakdown.run_metrics.hash_work_bytes").as_f64(),
            dotted(row, "breakdown.run_metrics.hash_total_s").as_f64(),
        )
        && bytes > 0.0
        && !row["hash"].is_null()
        && let Some(value) = ratio(bytes / bps, actual)
    {
        row["hash"]["sol_calibrated"] = value.into();
        row["hash"]["reference_id"] = json!({
            "disk": references["disk"]["id"],
            "hash": references["hash"]["id"],
            "read_lane": read_lane,
            "algorithm": algorithm,
        });
    }
    // Quick scan: entries the fingerprint walks enumerated at the volume's
    // metadata rate (first pass for a cold or evicted row, warm otherwise).
    let first_pass = references["metadata"]["first_pass_entries_per_s"].as_f64();
    let warm = references["metadata"]["warm_entries_per_s"].as_f64();
    let entry_rate = if matches!(row["cache_state"].as_str(), Some("cold" | "evicted")) {
        first_pass
    } else {
        match (first_pass, warm) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    };
    if let (Some(rate), Some(entries), Some(actual)) = (
        entry_rate.filter(|rate| *rate > 0.0),
        row["quick_scan"]["entries"].as_f64(),
        row["quick_scan"]["actual_s"].as_f64(),
    ) && entries > 0.0
        && let Some(value) = ratio(entries / rate, actual)
    {
        row["quick_scan"]["sol_calibrated"] = value.into();
        row["quick_scan"]["reference_id"] = references["metadata"]["id"].clone();
    }
    let fresh = references["latency"]["fresh_request_s"].as_f64();
    let reused = references["latency"]["reused_request_s"].as_f64();
    if let (Some(fresh), Some(actual)) = (fresh, row["startup_probe"]["actual_s"].as_f64())
        && let Some(value) = ratio(fresh, actual)
    {
        row["startup_probe"]["sol_calibrated"] = value.into();
        row["startup_probe"]["reference_id"] = references["latency"]["id"].clone();
    }
    // A no-change action: the pipeline exited early, or it completed a
    // recheck without fetching a graph or moving a byte.
    let outcome = row["sync_action"]["outcome"].as_str().unwrap_or_default();
    let no_change = outcome.starts_with("early-exit")
        || (outcome == "completed"
            && row["download"].is_null()
            && matches!(
                row["remote_refresh"]["outcome"].as_str(),
                Some("skipped_clean" | "graph_unchanged")
            ));
    if no_change
        && let (Some(fresh), Some(actual)) = (fresh, row["sync_action"]["actual_s"].as_f64())
    {
        let further = row["remote_refresh"]["index_requests"]
            .as_f64()
            .map_or(0.0, |n| (n - 1.0).max(0.0));
        let ideal = fresh + reused.unwrap_or(fresh) * further;
        if let Some(value) = ratio(ideal, actual) {
            row["sync_action"]["sol_calibrated"] = value.into();
            row["sync_action"]["reference_id"] = references["latency"]["id"].clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calibration() -> Value {
        let env = json!({"cpu":"9950X3D","os":"Windows 11","memory_gb":96,"origin":"a3.example.test:8080"});
        json!({"lanes":{
            "network":{"id":"network-1","environment":env,"values":{"origin":"a3.example.test:8080","sustained_bps":118_000_000.0,"per_connection_bps":1_500_000.0}},
            "latency":{"id":"latency-1","environment":env,"values":{"origin":"a3.example.test:8080","fresh_request_s":0.080,"reused_request_s":0.040}},
            "disk":{"s:":{"id":"disk-1","environment":env,"values":{"storage_class":"ssd","volume":"S:","unbuffered_read_bps":3_000_000_000.0,"warm_read_bps":9_000_000_000.0,"durable_write_bps":2_000_000_000.0}}},
            "hash":{"id":"hash-1","environment":env,"values":{"blake3_all_cores_bps":20_000_000_000.0,"md5_all_cores_bps":8_000_000_000.0}},
            "metadata":{"s:":{"id":"metadata-1","environment":env,"values":{"storage_class":"ssd","volume":"S:","first_pass_entries_per_s":300_000.0,"warm_entries_per_s":280_000.0}}},
            "db":{"s:":{"id":"db-1","environment":env,"values":{"storage_class":"ssd","volume":"S:","insert_rows_per_s":70_000.0,"update_rows_per_s":50_000.0,"delete_rows_per_s":126_000.0}}}
        }})
    }

    #[test]
    fn lanes_are_selected_by_machine_origin_class_and_volume() {
        let env = json!({"cpu":"9950X3D","os":"Windows 11","memory_gb":96,"origin":"a3.example.test:8080"});
        let selected = select(&calibration(), &env, "ssd", Some("s:"));
        assert_eq!(selected["network"]["id"], "network-1");
        assert_eq!(selected["disk"]["id"], "disk-1");
        assert_eq!(ids(&selected)["hash"], "hash-1");
        let other_origin = select(
            &calibration(),
            &json!({"cpu":"9950X3D","os":"Windows 11","memory_gb":96,"origin":"loopback"}),
            "ssd",
            Some("S:"),
        );
        assert!(other_origin["network"].is_null());
        assert!(other_origin["latency"].is_null());
        assert_eq!(other_origin["hash"]["id"], "hash-1");
        assert!(select(&calibration(), &env, "hdd", Some("S:"))["disk"].is_null());
        assert!(select(&calibration(), &env, "ssd", Some("D:"))["disk"].is_null());
        let other_cpu =
            json!({"cpu":"other","os":"Windows 11","memory_gb":96,"origin":"a3.example.test:8080"});
        assert!(
            select(&calibration(), &other_cpu, "ssd", None)
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn calibrated_ratios_name_their_reference_and_stay_unclamped() {
        let env = json!({"cpu":"9950X3D","os":"Windows 11","memory_gb":96,"origin":"a3.example.test:8080"});
        let mut row = json!({
            "cache_state":"warm",
            "references": select(&calibration(), &env, "ssd", Some("S:")),
            "download":{"work_bytes":4_331_121_846u64,"actual_s":39.5},
            "hash":{"actual_s":0.05,"algorithm":"blake3"},
            "breakdown":{"run_metrics":{"hash_work_bytes":4_331_121_846u64,"hash_total_s":0.47}},
            "startup_probe":{"actual_s":0.092},
            "sync_action":{"outcome":"early-exit-skip","actual_s":0.45},
            "remote_refresh":{"index_requests":2},
            "quick_scan":{"actual_s":0.021,"entries":3000},
            "db_purge":{"rows_affected":63000,"txn_s":1.0},
            "db_persist":{"rows_affected":5000,"write_time_ms":200.0}
        });
        attach(&mut row);
        assert_eq!(row["download"]["sol_calibrated"], 0.9292);
        assert_eq!(row["download"]["reference_id"], "network-1");
        // 4.33 GB at min(9 GB/s warm read, 20 GB/s CPU) = 0.481 s ideal over 0.47 s.
        assert_eq!(row["hash"]["sol_calibrated"], 1.0239);
        assert_eq!(row["hash"]["reference_id"]["read_lane"], "warm_read_bps");
        assert_eq!(row["startup_probe"]["sol_calibrated"], 0.8696);
        // fresh 0.080 + one reused 0.040 over 0.45 s.
        assert_eq!(row["sync_action"]["sol_calibrated"], 0.2667);
        // 3000 entries at the better of 300k and 280k entries/s over 21 ms.
        assert_eq!(row["quick_scan"]["sol_calibrated"], 0.4762);
        assert_eq!(row["quick_scan"]["reference_id"], "metadata-1");
        // 63k rows at 126k rows/s over a 1 s transaction.
        assert!(row["db_purge"]["sol_calibrated"].is_null());
        assert!(row["db_persist"]["sol_calibrated"].is_null());

        let mut evicted = row.clone();
        evicted["cache_state"] = "evicted".into();
        attach(&mut evicted);
        assert_eq!(
            evicted["hash"]["reference_id"]["read_lane"],
            "unbuffered_read_bps"
        );
        assert_eq!(evicted["hash"]["sol_calibrated"], 3.0717);

        let mut md5 = row.clone();
        md5["hash"]["algorithm"] = "md5".into();
        attach(&mut md5);
        assert_eq!(md5["hash"]["reference_id"]["algorithm"], "md5");
        assert_eq!(md5["hash"]["sol_calibrated"], 1.1519);

        let mut first_pass = row.clone();
        first_pass["cache_state"] = "cold".into();
        first_pass["hash"]
            .as_object_mut()
            .unwrap()
            .remove("sol_calibrated");
        first_pass["hash"]
            .as_object_mut()
            .unwrap()
            .remove("reference_id");
        attach(&mut first_pass);
        assert!(first_pass["hash"]["sol_calibrated"].is_null());

        let mut none = json!({"download":{"work_bytes":1,"actual_s":1.0}});
        attach(&mut none);
        assert!(none["download"]["sol_calibrated"].is_null());
    }
}
