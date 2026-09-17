//! Cut the parts of the process log that belong to one benchmark: the startup
//! block (machine summary, build, database open) and the frame of the action
//! itself, then pull the structured lines (`SOL`, `PIPELINE SUMMARY`) out of it.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, FixedOffset};

use super::record::BenchmarkStage;

/// Upper bound for the startup block so a long startup quick scan does not
/// drag the whole log into every benchmark.
pub const STARTUP_WINDOW_MS: i64 = 30_000;
pub const STARTUP_MAX_LINES: usize = 500;
/// Slack around the action frame: a little before the spawn (the UI stamps
/// the start before the worker logs) and more after, since the summary is
/// written right as the UI sees `Finished`.
pub const FRAME_SLACK_BEFORE_MS: i64 = 250;
pub const FRAME_SLACK_AFTER_MS: i64 = 1_500;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogSlice {
    pub startup: Vec<String>,
    pub frame: Vec<String>,
}

impl LogSlice {
    pub fn line_count(&self) -> usize {
        self.startup.len() + self.frame.len()
    }

    /// The slice as one file: startup block, a marker, then the action frame.
    pub fn render(&self, header: &str) -> String {
        let mut out = String::new();
        out.push_str("# ");
        out.push_str(header);
        out.push('\n');
        out.push_str("# ---- startup ----\n");
        for line in &self.startup {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("# ---- action ----\n");
        for line in &self.frame {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Timestamp of a log line in unix milliseconds, `None` for continuation
/// lines (multi-line records) or anything not in the logger's format.
pub fn line_timestamp_ms(line: &str) -> Option<i64> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    let stamp = &rest[..end];
    DateTime::<FixedOffset>::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%.f %:z")
        .or_else(|_| DateTime::<FixedOffset>::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%.f %z"))
        .ok()
        .map(|at| at.timestamp_millis())
}

/// Select the startup block and the action frame from `lines`, both given as
/// inclusive unix millisecond ranges. Continuation lines follow the record
/// they belong to.
pub fn slice_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    startup: (i64, i64),
    frame: (i64, i64),
) -> LogSlice {
    let mut slice = LogSlice::default();
    let mut in_startup = false;
    let mut in_frame = false;
    for line in lines {
        if let Some(at) = line_timestamp_ms(line) {
            in_startup =
                at >= startup.0 && at <= startup.1 && slice.startup.len() < STARTUP_MAX_LINES;
            in_frame = at >= frame.0 && at <= frame.1;
        }
        if in_startup {
            slice.startup.push(line.to_owned());
        }
        if in_frame {
            slice.frame.push(line.to_owned());
        }
    }
    slice
}

/// How far a file's first "Logger initialized" stamp may sit from the process
/// start and still be this process's file.
const OWN_LOG_START_TOLERANCE_MS: i64 = 1_000;

/// Whether a log file belongs to the process that started at `process_start_ms`.
/// Every process opens its file with a `Logger initialized` record, so a file
/// whose first record is that line at another time is another process (a CLI
/// run against the same config dir); a file that starts mid-stream is a
/// rotation continuation and is kept.
pub fn file_belongs_to_process(first_line: &str, process_start_ms: i64) -> bool {
    if !first_line.contains("Logger initialized") {
        return true;
    }
    line_timestamp_ms(first_line)
        .is_some_and(|at| (at - process_start_ms).abs() <= OWN_LOG_START_TOLERANCE_MS)
}

/// Every line this process has logged, in order: its own `foxy_r*.log` files
/// (the current one plus rotations), skipping files of other Foxy processes
/// that share the folder.
pub fn gather_log_lines(logs_dir: &Path, process_start_ms: i64) -> Vec<String> {
    let mut files: Vec<(i64, Vec<String>)> = fs::read_dir(logs_dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("foxy_r") && name.ends_with(".log"))
        })
        .filter(|path| {
            path.metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|age| age.as_millis() as i64 >= process_start_ms - 1_000)
        })
        .filter_map(|path| {
            let text = fs::read_to_string(&path).ok()?;
            let mut lines = text.lines();
            let first = lines.next()?;
            if !file_belongs_to_process(first, process_start_ms) {
                return None;
            }
            let first_at = line_timestamp_ms(first)?;
            let mut out = vec![first.to_owned()];
            out.extend(lines.map(str::to_owned));
            Some((first_at, out))
        })
        .collect();
    files.sort_by_key(|(first_at, _)| *first_at);
    files.into_iter().flat_map(|(_, lines)| lines).collect()
}

/// `key=value` tokens of every `SOL op=...` line in the frame.
pub fn parse_sol_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<BTreeMap<String, String>> {
    lines
        .into_iter()
        .filter_map(|line| {
            let start = line.find("SOL op=")?;
            let map = parse_sol_body(&line[start + "SOL ".len()..]);
            (!map.is_empty()).then_some(map)
        })
        .collect()
}

/// The SOL grammar: space-separated `key=value` pairs, where a value may be
/// double-quoted to carry spaces. The last occurrence of a repeated key wins,
/// a token without `=` is skipped, and values keep their raw text so integer
/// counters never round-trip through floating point here.
pub fn parse_sol_body(body: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut rest = body.trim_start();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = &rest[..eq];
        if key.is_empty() || key.contains(char::is_whitespace) {
            rest = rest
                .find(char::is_whitespace)
                .map_or("", |at| rest[at..].trim_start());
            continue;
        }
        let after = &rest[eq + 1..];
        let (value, remainder) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (&quoted[..end], &quoted[end + 1..]),
                None => (quoted, ""),
            }
        } else {
            match after.find(char::is_whitespace) {
                Some(end) => (&after[..end], &after[end..]),
                None => (after, ""),
            }
        };
        map.insert(key.to_owned(), value.to_owned());
        rest = remainder.trim_start();
    }
    map
}

/// The SOL lines a saved record owns: lines that carry an `op_id` belong to
/// the record only when it matches the record's own operation id; lines
/// without one (hash batches, quick scans, startup) are kept on the strength
/// of the time frame alone. A record without an operation id keeps everything
/// in its frame, as before ownership existed.
pub fn owned_sol_lines(
    lines: Vec<BTreeMap<String, String>>,
    operation_id: Option<&str>,
) -> Vec<BTreeMap<String, String>> {
    let Some(operation_id) = operation_id else {
        return lines;
    };
    lines
        .into_iter()
        .filter(|line| line.get("op_id").is_none_or(|op_id| op_id == operation_id))
        .collect()
}

/// One `PIPELINE SUMMARY` table: the operation it belongs to and its rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PipelineSummary {
    pub operation_id: String,
    pub stages: Vec<BenchmarkStage>,
}

/// The summary of the action itself: the last `repo-sync-*` table in the
/// frame, or the last table of any kind when no repository sync ran.
pub fn parse_pipeline_summary<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Option<PipelineSummary> {
    let tables = parse_pipeline_tables(lines);
    tables
        .iter()
        .rev()
        .find(|table| table.operation_id.starts_with("repo-sync-"))
        .or_else(|| tables.last())
        .cloned()
}

/// Every summary table in `lines`. The logger writes the table as one record,
/// which lands in the file as a single line with the rows separated by runs
/// of spaces; in memory it is still multi-line. Both forms tokenize the same
/// way once the lines from the header to `TOTAL` are joined.
pub fn parse_pipeline_tables<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<PipelineSummary> {
    let mut tables = Vec::new();
    let mut pending: Option<String> = None;
    for line in lines {
        if let Some(at) = line.find("PIPELINE SUMMARY:") {
            pending = Some(line[at..].to_owned());
        } else if let Some(buffer) = pending.as_mut() {
            buffer.push_str("  ");
            buffer.push_str(line);
        }
        if let Some(buffer) = pending.as_ref()
            && buffer.contains("TOTAL")
            && let Some(table) = parse_pipeline_table_text(buffer)
        {
            tables.push(table);
            pending = None;
        }
    }
    tables
}

fn parse_pipeline_table_text(text: &str) -> Option<PipelineSummary> {
    let tokens: Vec<&str> = text
        .split("  ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    let operation_id = tokens
        .iter()
        .find_map(|token| token.strip_prefix("op="))
        .and_then(|rest| rest.split_whitespace().next())?
        .to_owned();
    let is_rule = |token: &str| token.chars().all(|c| c == '-' || c == '=');
    let parse_secs = |token: &str| token.strip_suffix('s').and_then(|v| v.parse::<f64>().ok());
    let header_end = tokens
        .iter()
        .position(|token| token.starts_with("Details"))?;
    let mut stages = Vec::new();
    let mut index = header_end + 1;
    while index < tokens.len() {
        let token = tokens[index];
        if token == "TOTAL" {
            break;
        }
        if is_rule(token) {
            index += 1;
            continue;
        }
        let Some(seconds) = tokens.get(index + 1).and_then(|next| parse_secs(next)) else {
            index += 1;
            continue;
        };
        let details = tokens
            .get(index + 2)
            .filter(|next| next.contains('='))
            .map(|next| (*next).to_owned())
            .unwrap_or_default();
        index += if details.is_empty() { 2 } else { 3 };
        stages.push(BenchmarkStage {
            name: token.to_owned(),
            seconds,
            details,
        });
    }
    Some(PipelineSummary {
        operation_id,
        stages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = "[2026-09-15 11:24:23.602582 +02:00] INFO  [Foxy::core] hello";

    #[test]
    fn parses_logger_timestamps() {
        let at = line_timestamp_ms(LINE).expect("timestamp");
        // 09:24:23.602 UTC
        assert_eq!(at, 1_789_464_263_602);
        assert_eq!(line_timestamp_ms("  continuation"), None);
        assert_eq!(line_timestamp_ms("[not a stamp] x"), None);
    }

    #[test]
    fn slice_keeps_continuations_with_their_record() {
        let base = line_timestamp_ms(LINE).unwrap();
        let lines = [
            "[2026-09-15 11:24:23.602582 +02:00] INFO  [a] startup".to_owned(),
            "[2026-09-15 11:24:30.000000 +02:00] INFO  [a] later".to_owned(),
            "[2026-09-15 11:25:00.000000 +02:00] INFO  [a] frame start".to_owned(),
            "  continued".to_owned(),
            "[2026-09-15 11:26:00.000000 +02:00] INFO  [a] after frame".to_owned(),
        ];
        let slice = slice_lines(
            lines.iter().map(String::as_str),
            (base, base + 10_000),
            (base + 36_000, base + 40_000),
        );
        assert_eq!(slice.startup.len(), 2);
        assert_eq!(slice.frame, vec![lines[2].clone(), lines[3].clone()]);
        assert!(
            slice
                .render("h")
                .contains("# ---- action ----\n[2026-09-15 11:25:00")
        );
    }

    #[test]
    fn parses_sol_lines() {
        let lines = ["[x] INFO [m] SOL op=download actual_s=20.000 work_bytes=104857600 sol=0.5"];
        let sol = parse_sol_lines(lines);
        assert_eq!(sol.len(), 1);
        assert_eq!(sol[0]["op"], "download");
        assert_eq!(sol[0]["work_bytes"], "104857600");
    }

    #[test]
    fn sol_grammar_handles_quotes_equals_utf8_large_integers_and_duplicates() {
        let map = parse_sol_body(
            "op=hash label=\"two words\" note=a=b name=\u{e9}t\u{e9} bytes=18446744073709551615 malformed=1.2.3 dup=1 dup=2 stray k=",
        );
        assert_eq!(map["label"], "two words");
        assert_eq!(map["note"], "a=b");
        assert_eq!(map["name"], "\u{e9}t\u{e9}");
        assert_eq!(map["bytes"], "18446744073709551615");
        assert_eq!(map["bytes"].parse::<u64>().unwrap(), u64::MAX);
        assert_eq!(map["malformed"], "1.2.3");
        assert_eq!(map["dup"], "2");
        assert_eq!(map["k"], "");
        assert!(!map.contains_key("stray"));
        let unterminated = parse_sol_body("op=x label=\"open ended");
        assert_eq!(unterminated["label"], "open ended");
    }

    #[test]
    fn owned_sol_lines_drop_other_operations_but_keep_unowned_lines() {
        let lines = parse_sol_lines([
            "SOL op=download actual_s=1 op_id=repo-sync-0002",
            "SOL op=app_update_check actual_s=0.1 op_id=app-update-check-0001",
            "SOL op=download actual_s=2 op_id=repo-sync-0003",
            "SOL op=hash actual_s=1 work_bytes=5",
        ]);
        let owned = owned_sol_lines(lines.clone(), Some("repo-sync-0002"));
        let ops: Vec<&str> = owned.iter().map(|l| l["op"].as_str()).collect();
        assert_eq!(ops, ["download", "hash"]);
        assert_eq!(owned[0]["actual_s"], "1");
        assert_eq!(owned_sol_lines(lines, None).len(), 4);
    }

    #[test]
    fn parses_single_line_pipeline_summary_and_prefers_repo_sync() {
        let lines = [
            "[x] INFO [m] =====  PIPELINE SUMMARY: QuickScan [completed]  op=quick-scan-0002 repo=0 repositories -----  Stage       Duration  Details -----  TOTAL          0.00s  outcome=completed =====",
            "[x] INFO [m] =====  PIPELINE SUMMARY: RemoteRefreshOnly [prepared]  op=repo-sync-0004 repo=https://e/ -----  Stage    Duration  Details -----  create_context   0.00s    quick_local_verify   0.03s  mods=22, scoped=true  remote_repository   0.18s    -----  TOTAL   0.37s  outcome=prepared =====",
        ];
        let summary = parse_pipeline_summary(lines).expect("summary");
        assert_eq!(summary.operation_id, "repo-sync-0004");
        let names: Vec<&str> = summary.stages.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["create_context", "quick_local_verify", "remote_repository"]
        );
        assert_eq!(summary.stages[1].seconds, 0.03);
        assert_eq!(summary.stages[1].details, "mods=22, scoped=true");
        assert_eq!(summary.stages[2].details, "");
    }

    #[test]
    fn parses_multi_line_pipeline_summary() {
        let lines = [
            "[x] INFO [m] ==========",
            " PIPELINE SUMMARY: Download [completed]",
            " op=repo-sync-0003 repo=https://example.test/",
            " ----------",
            " Stage         Duration  Details",
            " ----------",
            " remote-refresh    1.25s  files=12 bytes=3",
            " download         20.00s  ",
            " ----------",
            " TOTAL            21.25s  outcome=completed",
            " ==========",
        ];
        let summary = parse_pipeline_summary(lines).expect("summary");
        assert_eq!(summary.operation_id, "repo-sync-0003");
        assert_eq!(summary.stages.len(), 2);
        assert_eq!(summary.stages[0].name, "remote-refresh");
        assert_eq!(summary.stages[0].details, "files=12 bytes=3");
        assert_eq!(summary.stages[1].seconds, 20.0);
        assert!(parse_pipeline_summary(["nothing here"]).is_none());
    }

    #[test]
    fn own_log_files_are_told_apart_from_cli_runs() {
        let start = line_timestamp_ms(LINE).unwrap();
        assert!(file_belongs_to_process(
            "[2026-09-15 11:24:24.000000 +02:00] INFO  [l] Logger initialized",
            start
        ));
        assert!(!file_belongs_to_process(
            "[2026-09-15 11:30:00.000000 +02:00] INFO  [l] Logger initialized",
            start
        ));
        assert!(file_belongs_to_process(
            "[2026-09-15 11:30:00.000000 +02:00] INFO  [l] rotation continues",
            start
        ));
    }

    #[test]
    fn gather_reads_only_this_process_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let start = line_timestamp_ms(LINE).unwrap();
        std::fs::write(
            dir.path().join("foxy_rCURRENT.log"),
            "[2026-09-15 11:24:23.700000 +02:00] INFO  [l] Logger initialized\n[2026-09-15 11:24:25.000000 +02:00] INFO  [l] two\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("foxy_r2026-09-15_11-25-00.log"),
            "[2026-09-15 11:25:00.000000 +02:00] INFO  [l] Logger initialized\n[2026-09-15 11:25:01.000000 +02:00] INFO  [l] cli\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("foxy_r2026-09-15_11-26-00.log"),
            "[2026-09-15 11:26:00.000000 +02:00] INFO  [l] rotated three\n",
        )
        .unwrap();
        let lines = gather_log_lines(dir.path(), start);
        let messages: Vec<&str> = lines
            .iter()
            .map(|l| l.rsplit("[l] ").next().unwrap())
            .collect();
        assert_eq!(messages, vec!["Logger initialized", "two", "rotated three"]);
    }
}
