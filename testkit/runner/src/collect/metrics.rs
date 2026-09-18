use serde_json::{Value, json};

use super::event_lines;

pub fn run_metrics(text: &str) -> Value {
    let mut result = json!({"files":null,"bytes":null,"download_retries":null,"download_retried_files":0,"db_write_time_ms":null,"lock_retries":null,"total_backoff_ms":null,"elapsed_ms":null,"permit_wait_ms_total":null,"write_calls_total":null,"write_failures_total":null,"write_retries_total":null,"checkpoint_total_s":null,"hash_work_bytes":0,"hash_total_s":0.0,"tree_verify_runs":0,"fs_watcher_starts":0,"prepared_queue_reuses":0,"pipeline_outcome":null,"failed_pipelines":0,"content_refresh_runs":0,"content_refresh_files_sampled":0,"hash_source_segments_files":0,"hash_source_reread_files":0,"hash_batches_after_download":0,"final_hash_flush_files":0,"download_large_files_limit":null,"download_small_files_limit":null,"download_patch_applies_limit":null,"first_download_start_ms":null,"patch_fallbacks":0,"patch_applies":0,"patch_range_requests":0,"patch_gap_bytes":0,"patch_copy_bytes":0,"patch_cancelled":0});
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
    let pattern = regex::Regex::new(r"TOTAL DOWNLOAD total:[^\n]* retries=(\d+)").unwrap();
    if let Some(c) = pattern.captures(text) {
        result["download_retries"] = c[1].parse::<u64>().unwrap().into();
    }
    let pattern = regex::Regex::new(r"Retry recovery:[^\n]* total_retried_files=(\d+)").unwrap();
    result["download_retried_files"] = event_lines(text)
        .filter_map(|line| pattern.captures(line))
        .filter_map(|captures| captures[1].parse::<u64>().ok())
        .sum::<u64>()
        .into();
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
    // Bytes the operation actually read to hash, summed over every hash run,
    // and the redundant-work counters the sync-path cases assert on. These
    // are counts, so an operation that hashed nothing reports 0, not null.
    let pattern =
        regex::Regex::new(r"SOL op=hash work_bytes=(\d+)|SOL op=hash [^\n]*? work_bytes=(\d+)")
            .unwrap();
    let hash_work_bytes: u64 = event_lines(text)
        .filter_map(|line| pattern.captures(line))
        .filter_map(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().parse::<u64>().unwrap_or(0))
        .sum();
    result["hash_work_bytes"] = hash_work_bytes.into();
    // Wall time of every hash run in the operation, summed. The `hash` SOL
    // record on a row is only the last run, which on a download is one
    // arbitrary batch; verdicts gate on this total instead.
    let hash_secs = regex::Regex::new(r"SOL op=hash actual_s=([\d.]+)").unwrap();
    let hash_total_s: f64 = event_lines(text)
        .filter_map(|line| hash_secs.captures(line))
        .filter_map(|c| c[1].parse::<f64>().ok())
        .sum();
    result["hash_total_s"] = ((hash_total_s * 1000.0).round_ties_even() / 1000.0).into();
    result["tree_verify_runs"] =
        count_lines(text, "Quick scan triggering targeted tree-hash verify").into();
    result["fs_watcher_starts"] = count_lines(text, "Starting filesystem watcher").into();
    result["patch_fallbacks"] = count_lines(text, "Delta patch fallback for file_id=").into();
    result["patch_applies"] = count_lines(text, "Delta patch applied successfully:").into();
    result["patch_cancelled"] = count_lines(text, "Delta patch cancelled for file_id=").into();
    // Locality of the patch fetch: range requests issued and the bytes
    // over-fetched to coalesce them, plus the bytes copied from the source.
    for (key, pattern) in [
        (
            "patch_range_requests",
            r"Parallel delta blob download: [^\n]*? requests=(\d+)[^\n]*? chunk_requests=(\d+)",
        ),
        (
            "patch_gap_bytes",
            r"Parallel delta blob download: [^\n]*? gap_bytes=(\d+)",
        ),
        (
            "patch_copy_bytes",
            r"Delta patch apply completed: [^\n]*? copy_bytes=(\d+)",
        ),
    ] {
        let pattern = regex::Regex::new(pattern).unwrap();
        let total: u64 = event_lines(text)
            .filter_map(|line| pattern.captures(line))
            .filter_map(|c| {
                c.get(1)
                    .and_then(|value| value.as_str().parse::<u64>().ok())
                    .map(|value| {
                        value
                            + c.get(2)
                                .and_then(|extra| extra.as_str().parse::<u64>().ok())
                                .unwrap_or(0)
                    })
            })
            .sum();
        result[key] = total.into();
    }
    result["prepared_queue_reuses"] =
        count_lines(text, "Reusing confirmation-prepared download queue").into();
    // The sync pipeline's own verdict. A check that ends in `failed-*` still
    // clears the busy reason and returns a summary, so without this a case
    // could pass on a sync the user would have seen fail.
    let pattern = regex::Regex::new(r"Pipeline summary: op=\S+ mode=\S+ outcome=(\S+)").unwrap();
    let outcomes: Vec<&str> = event_lines(text)
        .filter_map(|line| pattern.captures(line))
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    result["pipeline_outcome"] = outcomes.last().map_or(Value::Null, |o| (*o).into());
    result["failed_pipelines"] = (outcomes
        .iter()
        .filter(|o| o.starts_with("failed") || o.starts_with("cancelled"))
        .count() as u64)
        .into();
    // Where the hash bytes came from and when they were paid. The refresh
    // counters prove the content-hash pass reused the fingerprints the hash
    // pass took while the file was in the page cache; the source counters
    // prove patched files were recorded from their apply segments; the
    // after-download counter proves hashing overlapped the transfer instead of
    // running as a tail; the limits pin the profile the destination selected.
    let refresh = regex::Regex::new(
        r"Content-hash baseline refreshed: .*?files_hashed=(\d+)/\d+ files_reused_from_hash_pass=(\d+)",
    )
    .unwrap();
    let mut refresh_runs = 0u64;
    let mut refresh_sampled = 0u64;
    for c in event_lines(text).filter_map(|line| refresh.captures(line)) {
        refresh_runs += 1;
        let hashed: u64 = c[1].parse().unwrap_or(0);
        let reused: u64 = c[2].parse().unwrap_or(0);
        refresh_sampled += hashed.saturating_sub(reused);
    }
    result["content_refresh_runs"] = refresh_runs.into();
    result["content_refresh_files_sampled"] = refresh_sampled.into();
    let sources = regex::Regex::new(
        r"Incremental hash sources: .*?hash_source=segments files=(\d+) hash_source=reread files=(\d+)",
    )
    .unwrap();
    let (mut segments, mut reread) = (0u64, 0u64);
    for c in event_lines(text).filter_map(|line| sources.captures(line)) {
        segments += c[1].parse::<u64>().unwrap_or(0);
        reread += c[2].parse::<u64>().unwrap_or(0);
    }
    result["hash_source_segments_files"] = segments.into();
    result["hash_source_reread_files"] = reread.into();
    let mut download_done = false;
    let mut batches_after = 0u64;
    for line in event_lines(text) {
        if line.contains("Download stage completed:") {
            download_done = true;
        } else if download_done
            && line.contains("Starting incremental hash for completed download batch")
        {
            batches_after += 1;
        }
    }
    result["hash_batches_after_download"] = batches_after.into();
    let flush =
        regex::Regex::new(r"Flushing final incremental hash batch after download: .*?files=(\d+)")
            .unwrap();
    result["final_hash_flush_files"] = event_lines(text)
        .filter_map(|line| flush.captures(line))
        .map(|c| c[1].parse::<u64>().unwrap_or(0))
        .sum::<u64>()
        .into();
    let limits = regex::Regex::new(
        r"Download resource profile: .*?limits large_files=(\d+) small_files=(\d+) .*?patch_applies=(\d+)",
    )
    .unwrap();
    if let Some(c) = event_lines(text).find_map(|line| limits.captures(line)) {
        result["download_large_files_limit"] = c[1].parse::<u64>().unwrap_or(0).into();
        result["download_small_files_limit"] = c[2].parse::<u64>().unwrap_or(0).into();
        result["download_patch_applies_limit"] = c[3].parse::<u64>().unwrap_or(0).into();
    }
    result["first_download_start_ms"] =
        first_download_start_ms(text).map_or(Value::Null, Value::from);
    result
}

fn count_lines(text: &str, needle: &str) -> u64 {
    event_lines(text)
        .filter(|line| line.contains(needle))
        .count() as u64
}

/// Milliseconds from the first timestamped event of the slice to the first
/// `Starting download for mod` line: the time an update spent preparing before
/// the first byte moved. Null when the slice has no download start.
fn first_download_start_ms(text: &str) -> Option<u64> {
    let first = event_lines(text).find_map(line_timestamp)?;
    let start = event_lines(text)
        .filter(|line| line.contains("Starting download for mod"))
        .find_map(line_timestamp)?;
    Some((start - first).num_milliseconds().max(0) as u64)
}

fn line_timestamp(line: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let end = line.find(']')?;
    chrono::DateTime::parse_from_str(line.get(1..end)?, "%Y-%m-%d %H:%M:%S%.f %z").ok()
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
            "TOTAL DOWNLOAD total: files=2 bytes=1.5 MiB elapsed=1.0s avg=1.5 MB/s retries=3\nRetry recovery: mod_id=1 all files succeeded on attempt 2 total_retried_files=4\nsqlite: mode=wal lock_retries=2 avg_backoff_ms=1.0 total_backoff_ms=2 db_write_time_ms=3.5 extra=1 elapsed_ms=12\ncalls=3 committed=3 failed=0 retries=2 backoff_ms=0 permit_wait_ms=1.25\ncalls=4 committed=4 failed=1 retries=0 backoff_ms=0 permit_wait_ms=2.25\ncheckpoint_batches=1 rows=2 statements=3 total=0.5s",
        );
        assert_eq!(metrics["bytes"], 1572864.0);
        assert_eq!(metrics["download_retries"], 3);
        assert_eq!(metrics["download_retried_files"], 4);
        assert_eq!(metrics["write_calls_total"], 7.0);
        assert_eq!(metrics["permit_wait_ms_total"], 3.5);
        assert_eq!(metrics["elapsed_ms"], 12.0);
    }
    #[test]
    fn redundant_work_counters_sum_hash_bytes_and_count_lines() {
        let metrics = run_metrics(
            "x SOL op=hash actual_s=1 work_bytes=100 actual_bps=1 sol=na light_src=self_baseline label=a
SOL op=hash actual_s=1 work_bytes=250 label=b
INFO Quick scan triggering targeted tree-hash verify for repo=r files=3
INFO Starting filesystem watcher for 2 paths
INFO Starting filesystem watcher for 2 paths",
        );
        assert_eq!(metrics["hash_work_bytes"], 350);
        assert_eq!(metrics["hash_total_s"], 2.0);
        assert_eq!(metrics["tree_verify_runs"], 1);
        assert_eq!(metrics["fs_watcher_starts"], 2);
        assert_eq!(metrics["prepared_queue_reuses"], 0);
    }

    #[test]
    fn redundant_work_counters_ignore_the_driver_echo_of_timestamped_lines() {
        let metrics = run_metrics(
            "[2026-09-14 08:00:00.000000 +02:00] INFO  [m] SOL op=hash actual_s=1 work_bytes=100 label=a\n[2026-09-14 08:00:01.000000 +02:00] INFO  [m] Quick scan triggering targeted tree-hash verify for repo=r files=3\nSOL op=hash actual_s=1 work_bytes=100 label=a\nQuick scan triggering targeted tree-hash verify for repo=r files=3",
        );
        assert_eq!(metrics["hash_work_bytes"], 100);
        assert_eq!(metrics["tree_verify_runs"], 1);
        let none = run_metrics("unrelated");
        assert_eq!(none["hash_work_bytes"], 0);
        assert_eq!(none["tree_verify_runs"], 0);
        assert!(none["pipeline_outcome"].is_null());
        assert_eq!(none["failed_pipelines"], 0);
    }

    #[test]
    fn pipeline_outcome_is_the_last_verdict_and_failures_are_counted() {
        let metrics = run_metrics(
            "[2026-09-14 08:00:00.000000 +02:00] INFO  [m] Pipeline summary: op=repo-sync-0001 mode=RemoteRefreshOnly outcome=failed-empty-queue repo=r stages=13 elapsed=1.00s\n[2026-09-14 08:00:01.000000 +02:00] INFO  [m] Pipeline summary: op=repo-sync-0002 mode=Download outcome=early-exit-clean repo=r stages=8 elapsed=0.25s\nPipeline summary: op=repo-sync-0001 mode=RemoteRefreshOnly outcome=failed-empty-queue repo=r stages=13 elapsed=1.00s",
        );
        assert_eq!(metrics["pipeline_outcome"], "early-exit-clean");
        assert_eq!(metrics["failed_pipelines"], 1);
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

    #[test]
    fn patch_locality_counters_sum_owned_events() {
        let metrics = run_metrics(
            "Parallel delta blob download: files=2 requests=7 chunk_requests=2 gap_bytes=11\n\
             Delta patch apply completed: file_id=1 copy_bytes=13\n\
             Parallel delta blob download: files=1 requests=3 chunk_requests=1 gap_bytes=5",
        );
        assert_eq!(metrics["patch_range_requests"], 13);
        assert_eq!(metrics["patch_gap_bytes"], 16);
        assert_eq!(metrics["patch_copy_bytes"], 13);
    }
}

#[cfg(test)]
mod hash_source_tests {
    use super::*;

    const SLICE: &str = "[2026-09-16 07:16:57.000000 +02:00] INFO  [m] Download resource profile: pressure=normal total=93 GiB; destination_storage=Hdd limits large_files=12 small_files=48 ranges=96 per_file_ranges=8..96 range_chunk=2097152 patch_applies=2
[2026-09-16 07:16:57.250000 +02:00] INFO  [m] Starting download for mod 3 (3 files, 33 bytes)
[2026-09-16 07:16:58.000000 +02:00] INFO  [m] Starting incremental hash for completed download batch: repo=r mod_id=3 mod=@a files=1 bytes=5
[2026-09-16 07:16:59.000000 +02:00] INFO  [m] Incremental hash sources: repo=r hash_source=segments files=4 hash_source=reread files=1
[2026-09-16 07:17:00.000000 +02:00] INFO  [m] Download stage completed: op=repo-sync-0002 elapsed=3.00s files=5
[2026-09-16 07:17:00.100000 +02:00] INFO  [m] Starting incremental hash for completed download batch: repo=r mod_id=4 mod=@b files=1 bytes=5
[2026-09-16 07:17:00.200000 +02:00] INFO  [m] Flushing final incremental hash batch after download: repo=r files=2 bytes=9
[2026-09-16 07:17:01.000000 +02:00] INFO  [m] Content-hash baseline refreshed: repo=r scope=files total_elapsed=1ms file_hash=1ns file_persist=1ms addon_hash=1ms repos_hashed=1 addons_hashed=3/3 files_hashed=5/5 files_reused_from_hash_pass=4 file_failures=0 addon_failures=0
[2026-09-16 07:17:02.000000 +02:00] INFO  [m] Content-hash baseline refreshed: repo=r scope=repository total_elapsed=1ms file_hash=1ns file_persist=1ms addon_hash=1ms repos_hashed=1 addons_hashed=3/3 files_hashed=10/10 files_reused_from_hash_pass=10 file_failures=0 addon_failures=0
Starting download for mod 3 (3 files, 33 bytes)
Content-hash baseline refreshed: repo=r scope=files total_elapsed=1ms file_hash=1ns file_persist=1ms addon_hash=1ms repos_hashed=1 addons_hashed=3/3 files_hashed=5/5 files_reused_from_hash_pass=4 file_failures=0 addon_failures=0";

    #[test]
    fn refresh_sources_overlap_and_limits_are_read_from_the_slice() {
        let metrics = run_metrics(SLICE);
        assert_eq!(metrics["content_refresh_runs"], 2);
        assert_eq!(metrics["content_refresh_files_sampled"], 1);
        assert_eq!(metrics["hash_source_segments_files"], 4);
        assert_eq!(metrics["hash_source_reread_files"], 1);
        assert_eq!(metrics["hash_batches_after_download"], 1);
        assert_eq!(metrics["final_hash_flush_files"], 2);
        assert_eq!(metrics["download_large_files_limit"], 12);
        assert_eq!(metrics["download_small_files_limit"], 48);
        assert_eq!(metrics["download_patch_applies_limit"], 2);
        assert_eq!(metrics["first_download_start_ms"], 250);
    }

    #[test]
    fn counters_are_zero_and_limits_null_without_the_lines() {
        let metrics = run_metrics("unrelated");
        assert_eq!(metrics["content_refresh_runs"], 0);
        assert_eq!(metrics["content_refresh_files_sampled"], 0);
        assert_eq!(metrics["hash_source_segments_files"], 0);
        assert_eq!(metrics["hash_batches_after_download"], 0);
        assert_eq!(metrics["final_hash_flush_files"], 0);
        assert!(metrics["download_large_files_limit"].is_null());
        assert!(metrics["first_download_start_ms"].is_null());
    }
}
