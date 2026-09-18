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

`--accept` refuses a dirty tree, non-release builds, invalid rows, case-hash
changes, incompatible operation profiles, and fewer than five successful warm
or explicitly evicted samples per lane. A version 2 baseline stores the
accepted Git SHA, case and origin identity, calibration references, complete
run and operation compatibility, sample size, medians, min/max spread, terminal
and oracle outcomes, and tolerances. Older baseline files remain readable but
produce `rebaseline-required` instead of silently comparing.

`derived_schema_version` identifies the contract used to derive a row from its
retained artifacts. Rows without it are version 1, shared aggregation is
version 2, and typed delta-patch stage spans are version 3. `replay` reports changes
while rebuilding an older version as migrations; it reports any change within
the current version as a difference and exits non-zero.

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
until the baseline is re-accepted. A metric added by a newer derived schema is
a replay migration. Additions, removals and changed values within the same
schema remain replay differences.

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
`warm`. An operation that follows an `evict-cache` step in the same iteration
is marked `evicted` instead, whatever its iteration number (since 2026-09-16;
before that such rows were `warm` after iteration zero). Evicted rows form
their own lane: medians and baselines key them as `<op>@evicted`, and a
comparison between a warm and an evicted lane is refused rather than
averaged. Comparisons never cross case hash, harness, build kind, database
mode, write-gate size, storage class, or cache lane.

Medians are grouped by operation label when the case gives one (`label`,
otherwise `op`), so two `quick-check` steps with different initial states are
compared each against its own counterpart.

A baseline accepted since 2026-09-16 also stores the run `profile` (harness,
build kind, database mode, write gate, pool policy, storage class) and each
operation's `cache_state`. `compare` validates that profile before any metric:
a mismatch yields the verdict `profile-mismatch` with `profile-mismatch:<key>`
flags, and a baseline recorded without a profile yields `rebaseline-required`
with `baseline-profile-missing`. A current lane the baseline never measured
(`baseline-missing-op:<key>`) or a lane recorded under another cache state
(`cache-lane-mismatch:<key>`) is likewise reported instead of passing
unmeasured. Older baselines therefore need a fresh `--accept` on a clean
revision; their numbers are still readable in the file.

Each row carries `environment` (CPU brand, OS family and major version, total
memory rounded to 8 GiB, origin host or `loopback`); a baseline accepted since
2026-09-16 stores it under `profile.environment`, and `compare` retires the
baseline with `rebaseline-required` and `environment-changed:<key>` flags when
any of them differs. This is an expiry rule, not a comparison key: rows from
another machine or origin never inherit this machine's reference.

Each row also carries `diagnostics` (`profile` when the case ran with
`FOXY_PROFILE`, else `none`; a profile key, so profiled and unprofiled rows
never compare), `origin_checksum` (the `checksum` the origin published at run
start; a baseline accepted under another one is refused with
`origin-changed`, the same verdict as a changed case hash) and `references`:
the calibration lanes from `testkit/ledger/calibration.json`
(`foxy-testkit calibrate`) that match the row's machine, origin, storage
class and repository volume, by id and the numbers the ratios use. From them
`build_row` derives `download.sol_calibrated` (body bytes at the origin's
sustained aggregate rate over the transfer's `actual_s`),
`hash.sol_calibrated` (hashed bytes at the slower of the disk read lane and
the matching all-core algorithm lane, BLAKE3 or MD5, over `hash_total_s`; the
read lane is the faster of the single-reader and parallel page-cache lanes
for a warm row and of the device lanes for an explicitly evicted one; a cold
iteration-0 row without an `evict-cache` is unrated because its cache state
is unknown),
`startup_probe.sol_calibrated` (one fresh request) and, on a no-change exit,
`sync_action.sol_calibrated` (one fresh index request plus a reused one per
further index request), each beside a `reference_id`. These are calibrated
estimates: unclamped, and above one when the reference was not a bound for
that run. A baseline stores the ids it cited; recalibrating retires it with
`reference-changed:<lane>`. Rows without a matching lane keep their records
untouched, and `replay` reproduces the ratios because the selected
references travel with the row.

Memory metrics (`memory.*`) are advisory (conventions/SPEED_OF_LIGHT.md,
resource trade policy): a move outside tolerance is reported as
`advisory-regression` / `advisory-improvement` with a `memory-advisory` flag
and never sets the run's verdict.

The complete-action records `remote_refresh`, `sync_action`, `db_persist`,
`db_purge` and `space_switch` (added 2026-09-16) sit beside the legacy
per-operation fields with the same last-record shape; `remote_refresh.actual_s`
and `sync_action.actual_s` are compared as durations. `quick_scan.sol_calibrated`
(entries at the B5 metadata rate) joins the calibrated metrics; the DB lane
rides on the row as a reference (`references.db`, insert, keyed-update and
delete rows per second). `db_purge` and `db_persist` receive a ratio at write
gate 1 when their insert, update and delete counters account for every affected
row; records with other affected statement kinds stay unrated. `summary.cancel_quiescent_ms`
is the cancel-to-quiescent latency of a `cancel_after_s` lane, and
`breakdown.run_metrics.patch_fallbacks` / `patch_applies` count apply-time
delta fallbacks and successful applies (2026-09-16).

Added later on 2026-09-16: `db_persist.rows_affected` (rows the engine
changed during the action). Derived schema 5 adds `insert_rows_affected`,
`update_rows_affected`, `delete_rows_affected` and `other_rows_affected` to
the persistence and purge records and derives their gate-1 DB ratios;
`download.ramp_s`, `plateau_s`, `tail_s`, `ramp_deficit_bytes` and
`tail_deficit_bytes` (the transfer stage split around its plateau, present
once a window reached 90% of the peak); `summary.ui_probe` (`samples`,
`frame_ms_max`, `frame_ms_p95_worst`, `fps_min`, `rtt_ms_p50`, `rtt_ms_p95`)
on an operation that ran with `ui_probe_ms`; and
`breakdown.run_metrics.patch_range_requests` / `patch_gap_bytes` /
`patch_copy_bytes` for the locality of a patch fetch and `patch_cancelled`
for attempts a cancel interrupted; `startup_probe.first_answer_s` /
`last_answer_s` for the spread between the fastest and slowest branch of a
startup probe. `sync_action` gains a
`stage_prepared_queue_prune_s` stage when a reused queue dropped files a
cancelled run had already verified.

Derived schema 6 adds the O8 startup dependency accounting:
`startup.dependency_bound_s` is the later of first paint and dispatch plus
verdict duration, `startup.join_s` is the remaining action wall time, and
`startup.dependency_coverage_percent` reports how much of the complete action
that dependency path explains. It is not a physical SoL ratio.

Derived schema 7 adds hash auto-profile validation fields under
`breakdown.run_metrics`: the rotated trial-order index, held-out files, bytes,
time and throughput, selected-sample throughput, generalization ratio, and
whether the remaining workload was large enough to validate independently.

`delta_patch` is the aggregate O2 action record. `delta_patch_stages` retains
every `SOL op=delta_patch_stage` record in log order, with the owning `op_id`,
per-file attempt parent span, unique stage span, stage id, monotonic offsets and
terminal stage outcome. This preserves overlap and identifies the stage where a
fallback or cancellation occurred without mixing stage service time into the
action aggregate.

Each row also carries `sol_aggregate.<op>`: the run count, rated-run count,
summed `actual_s` (service time over every batch, not the action makespan),
interval-union coverage, makespan, summed work, the derived rate, distinct
reference ids/statuses and metric versions, and the set of `outcome` values
with a `completed` flag. These fields use the same `foxy-sol` implementation as
the application benchmark viewer. The per-operation field (`hash`, `download`,
...) keeps the legacy last-record view. SOL records are
parsed from canonical event lines only: a GUI-harness slice echoes every event
once without a timestamp, and those echoes no longer double the aggregate.
Integer counters in SOL and DB key/value records are kept as exact integers
(previously every number was a float; replay treats `100` and `100.0` as equal).

An `expect` entry or a `thresholds` gate with `min`, `max` or `between` fails
when the value is missing, null or not a finite number, instead of treating it
as zero. Declare `"optional": true` on a gate whose metric may legitimately be
absent on some operations.

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
