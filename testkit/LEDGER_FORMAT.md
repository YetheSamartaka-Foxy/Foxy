# Test kit ledger format

Each measured operation appends one compact JSON object to
`ledger/<case-id>.jsonl`. Rows include run identity, Git/build identity, case
hash, harness, cache state, database mode, write-gate size, connection-pool idle
override, work counters, summary timings, telemetry percentiles, process memory,
parsed SOL records, attribution breakdown, flags, and a verdict.

`db_pool_idle` records `FOXY_DB_POOL_IDLE` when it was set, and is null for a
shipping-default run (idle limit 6). Comparisons never cross it, so an A/B run
against a non-default pool cannot be mistaken for history.

The raw artifacts remain in `runs/<case-id>/<run-id>/`. Derived values can be
recomputed from `summary-<iteration>-<op>.json`, `collected-*.json`,
`operation-*.log`, `sol.jsonl`, and `breakdown-*.json`;
`foxy-testkit replay <run-dir>` does exactly that and diffs the result against
the recorded row.

Baselines are explicit and variant-specific:

```
ledger/<case-id>.<harness>.<build>.<database-mode>.gate-<n>.baseline.json
```

`--accept` refuses a dirty tree, invalid rows, case-hash changes, and fewer than
two warm samples. A baseline stores the accepted Git SHA, case hash, sample
size, warm medians, and tolerances.

`breakdown.run_metrics` grows over time (`hash_work_bytes`, `tree_verify_runs`,
`fs_watcher_starts`, `prepared_queue_reuses` were added on 2026-09-14; the
hash-source, refresh, overlap, limit and first-download counters listed in
`CASE_FORMAT.md` on 2026-09-16). The regression verdict gates on
`breakdown.run_metrics.hash_total_s` (wall time summed over every hash run of
the operation) rather than on `hash.actual_s`, which is only the last run: on a
download row that is one arbitrary page-cache batch of tens of milliseconds,
and a 12 percent band over it flagged scheduler noise as a confirmed
regression. `hash.actual_s` stays on the row; a baseline accepted before
2026-09-16 has no `hash_total_s` median, so that metric is simply not compared
until the baseline is re-accepted. A
metric the recorded row never had is not a replay difference: `replay`
re-derives the row from the retained log, so a counter added later simply
appears on the rebuilt row, while a key the rebuilt row lost is still reported.

`memory` carries the process footprint the runner sampled around the operation
(see `CASE_FORMAT.md`). It is null for rows recorded before the memory lane
existed, which is what keeps `replay --all` byte-identical over them. The raw
sample series stays in the run directory and is not part of the row.

Default regression tolerances are 8 percent for SOL ratios, 12 percent for
elapsed/stage durations (and a duration must also move by at least 50 ms;
a percent band alone over a millisecond-scale metric flags scheduler jitter), 15 percent for peak and retained private commit,
50 percent for commit growth, and zero for correctness counters. Lower is better for
durations. Higher is better for SOL ratios, throughput, savings, and rates.
A regression or improvement must exceed tolerance in two complete runs before
the runner emits a confirmed verdict; the first occurrence is `candidate`.

Rows from iteration 0 are marked `cold` and excluded from warm medians. With
`warmup:true`, the warmup pass is not ledgered and every recorded pass is
`warm`. Comparisons never cross case hash, harness, build kind, database mode,
write-gate size, or storage class.

The runner marks rows invalid for failed assertions, a detected DB wipe,
incomplete expected payload, a failed independent oracle, runaway bytes, or an
unconfirmed effective database mode. It adds `work-conservation` when a timing
improves while the corresponding byte/file/part work counter decreases.

## Case hashes

`case_hash` is SHA-256 over the resolved case serialized as
[RFC 8785 JCS](https://www.rfc-editor.org/info/rfc8785/), so the hash is
reproducible from a published specification rather than from whatever the kit
happens to emit. Rows written by the PowerShell kit used a different
canonicalization and were rewritten in place by `foxy-testkit migrate-ledger`
from `ledger/legacy-case-inputs.json`; the pre-migration ledgers are kept under
`ledger/archive/pre-rust/`. Rows whose legacy hash was not in the mapping were
left untouched and reported, because those come from a case that has since
changed, which is exactly what `case_hash` exists to keep separate.

## Metric names

Two unrelated things have been called `total_ms`. Keep them apart:

| name | what it is | where |
| --- | --- | --- |
| `summary.total_ms` | the measured operation's elapsed time | ledger row, `foxy-testkit report` |
| `database.write_time_ms` | app `db_write_time_ms`: every write category's gated transaction window, summed | ledger row |
| `txn_ms` | one write category's gated transaction window | app log only, not ledgered |
| `permit_wait_ms` | time queued on the write gate before the window opens | both |

`database.write_time_ms` and `txn_ms` are **not** row cost and **not** comparable
across write-gate sizes. Turso has a single internal writer, so above gate 1 the
waiters block inside `conn.execute` without surfacing `Busy`, and that queue time
lands in the window rather than in `permit_wait_ms`. The same 3 738-row
`addon_files insert` measures ~72 ms at gate 1 and 570-1 150 ms at gate 8. Gate 1
is the uncontended reference. This is why comparisons never cross write-gate size
and why every row records `db_write_gate`.

Renamed on 2026-09-09: the per-category log field `total_ms` became `txn_ms`, and
both it and `db_write_time_ms` now print `write_gate=`. **Only the names changed,
not the measurements**, so rows and baselines recorded before that date stay
comparable with rows recorded after it. Logs captured before the rename say
`total_ms` in the `SQLite write category metrics` lines; the ledger field names
were never affected.
