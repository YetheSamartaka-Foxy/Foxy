# Performance hypotheses

Entries are append-only during an optimization loop. Close one with its ledger
row; do not delete rejected ideas.

| Status | Claim | Primary metric | Expected direction | Cheapest case |
| --- | --- | --- | --- | --- |
| accepted | A wave-aligned range grid plus a per-file ceiling equal to the global budget removes the straggler wave that is the download tail | `download.sol` | higher | `perf-redownload-small-ssd` |
| accepted | The download stage waits out the checkpoint and sampler sleeps before reading their stop flags | `summary.download_stage_ms` | lower | `perf-redownload-small-ssd` |
| accepted | Largest-file-first within a mod, and half as many concurrent large files, shorten the makespan tail | `download.sol` | higher | `perf-redownload-small-ssd` |
| accepted | The largest range chunk bounds the download tail, because a chunk in flight when the queue empties runs alone at one connection's rate | `download.sol` | higher | `perf-redownload-small-ssd` |
| rejected | More than 96 concurrent range requests buys aggregate throughput | `download.sol` | higher | probe against the reference origin |
| open | Tune per-file range count and global range budget against RTT | `download.sol` | lower | delta scattered |
| open | Coalesce small writes without increasing retries | `download.sol`, `breakdown.disk` | lower | delta scattered |
| open | Reuse TLS connections more effectively | `breakdown.network.permit_wait_s` | lower | force redownload |
| open | Tune patch copy buffer for spinning disks | `summary.total_ms` | lower | delta single-entry HDD |
| open | Improve persistent quick-scan cache key hit rate | `quick_scan.actual_s` | lower | touch-only quick check |
| open | Use BLAKE3 mmap rayon for large SSD files | `hash.sol` | lower | SSD recheck |
| open | Replace manual compaction with `VACUUM INTO` after the large probe | DB compaction elapsed | lower | large bloated DB probe |
| rejected | Retune Turso 0.7 bulk-write chunks | DB write elapsed | lower | remote refresh |
| accepted | Pooling tuned connections and caching their compiled statements cuts DB time | DB write elapsed | lower | `perf-db-refresh-main` |
| rejected | Turso 0.7 MVCC can beat WAL for Foxy's metadata rebuild | DB write elapsed and correctness | lower, no failures | `perf-db-refresh-main` |
| rejected | MVCC wins once the per-call `journal_mode` pragma is no longer paid (retest on the pooled build) | `summary.total_ms` | lower | `perf-db-refresh-main` |
| accepted | `DB_WRITE_GATE` above 1 is worth ~18% on the metadata refresh | `summary.total_ms` | lower | `perf-db-refresh-main` |
| open | Improve hash profile auto-selection by storage class | `hash.sol` | lower | SSD/HDD recheck pair |
| closed | Cover the deferred 433k-row `subfiles` bulk insert with a payload-bearing origin | DB write elapsed | n/a, coverage gap | `perf-db-parts-bulk` |
| accepted | Sorting the deferred part buffer into index key order speeds the bulk insert | deferred insert elapsed | lower | `perf-db-parts-bulk` |
| rejected | Dropping and rebuilding the `subfiles` indexes around the download-overlapped flush pays | deferred insert elapsed | lower | `bench_subfiles_index_cost` |
| rejected | Turso 0.7 MVCC wins on the one workload where Foxy's writes dominate | deferred insert elapsed | lower | `perf-db-parts-bulk` |
| accepted | Drop `idx_subfiles_file_id_data_order` and let the ordered reads sort per file | deferred insert elapsed | lower | `perf-db-parts-bulk` |
| accepted | Buffer per-mod file upserts into one batched `INSERT ... ON CONFLICT`, then map ids | `file upsert` calls | lower | `perf-db-parts-bulk` |
| rejected | Hand the metadata refresh's in-memory tree to the incremental hasher instead of reloading 433k parts | `select subfiles` elapsed | remove | `perf-db-parts-bulk-profiled` |
| open | Sort the 433k-part reload in process instead of with `ORDER BY` | `select subfiles` elapsed | lower | `bench_bulk_read_path` |
| open | Replace the correlated subqueries in `final_progress_flush` with an `UPDATE ... FROM` join | `update download_target_file` elapsed | lower | `perf-db-parts-bulk-profiled` |
| open | Move the skip checks in `tree_hash_bootstrap` and `existing_graph_queue_rebuild` ahead of the work they discard | stage elapsed | lower | `perf-db-parts-bulk-profiled` |
| open | Run single-statement seam writes in autocommit instead of BEGIN/statement/COMMIT | seam statement count | lower | `perf-db-parts-bulk-profiled` |
| open | Skip the per-mod patch-table deletes when the repository has no patch rows | `delete download_patch_op` calls | remove | `perf-db-parts-bulk-profiled` |
| open | Extend filesystem profiling to stat, exists, read_dir and buffered flush | profile `neither` column | shrink | `perf-db-parts-bulk-profiled` |

## Closed entries

### accepted: kill the download tail (grid, ceiling, ordering, stage quanta)

The steady state was already at the link ceiling; every lost second was a ramp
the client cannot influence and a 10-12 s straggler tail it can. Five changes,
warm download 45.11 s -> 40.40-40.60 s on NVMe and 67.6 s -> 53.8 s on spinning
media, same bytes and files in every row, independent oracle clean. A sixth
change then took `RANGE_CHUNK_TARGET` from 8 MiB to 2 MiB, which cut the
remaining tail deficit from 1.1-2.1 s to 0.25-0.54 s and left the run
ramp-bound at 39.8 s on NVMe and 48.4 s on spinning media. Full
write-up, per-step ledger table, and the probe numbers behind
"chunk size is free, connections are not" in
`ledger/perf-redownload-small-ssd.notes.md`.

### rejected: more than 96 concurrent range requests

The reference origin gives ~1.2-1.6 MB/s per connection, so aggregate is bought
with connections - but only up to the path ceiling of 118 MB/s, which 96
connections already reach. 192 pre-established connections plateau at the same
117.5 MB/s and ramp on the same curve. `MAX_ACTIVE_RANGE_REQUESTS` stays 96;
the per-file ceiling was raised to meet it instead.


### rejected: hand the in-memory tree to the incremental hasher

The `select subfiles` that reloads 433 248 part rows mid-download (1.644 s, 15%
of a force-redownload) looked redundant: `Tree::load` merges the deferred part
buffer, so the two `pre-download` loads read zero rows and already held every
part in memory. Reusing one of them would have removed the read entirely.

It would also have silently corrupted `subfiles`. The merged parts carry
`synthetic_id = parts.len() + 1`, a positional counter, because the rows do not
exist yet and have no ids to carry. `persist_part_checksums` writes with
`WHERE subfiles.id = v.id`. On a force-redownload the freshly populated table
holds rowids in the same numeric range, so those updates would have landed on
real rows and written the wrong checksums without raising an error.

Independently, on a force-redownload there is no tree to hand over: the bootstrap
is skipped and `bootstrap_tree_for_content_hash` stays `None`. The reload is the
first tree in the download phase and the first one holding real row ids.

The read stays. `bench_bulk_read_path` then showed the cost is the query shape
rather than the row count: 0.522 s unfiltered and unordered, 0.977 s with the
`ORDER BY`, 1.617 s with the 864-element `IN` list as well, against 1.644 s in
the app. Sorting in process instead costs 0.065 s, which is the replacement
hypothesis. Full reasoning in `db-improvements-2.md` item 1.


### closed: the deferred 433k-row `subfiles` insert is now covered

`perf-db-parts-bulk` builds a synthetic origin whose PBO entries become manifest
parts, so 433 248 part rows cost 42 MB of payload instead of the 87 GB the real
repository would move (`foxy-testkit synthetic --pbo-entries`). The measurement
this exposes: that single insert is 5.9 s of a 10.4 s `force-redownload`, runs
overlapped with the download, and finishes within 70 ms of it, so it is on the
critical path rather than hidden under it. Full write-up in
`ledger/perf-db-parts-bulk.notes.md`.

### accepted: sort the deferred part buffer into index key order

Both `subfiles` indexes lead with `file_id` and the buffer arrives in mod
completion order. `bench_subfiles_index_cost` puts fully shuffled arrival at
10.52 s against 4.84 s in key order for the same 433 248 rows, 2.2x.

At application level the win is much smaller, because the real buffer is already
grouped by mod and by file and only the mod-level interleaving was out of order:
the flush goes from 5.86-5.93 s to 5.53-5.64 s (n=6 over two complete runs,
disjoint ranges) and the operation from a 10 430 ms median to 10 124-10 357 ms.
Kept because it is three lines and cannot regress.

### rejected: drop and rebuild the `subfiles` indexes around the flush

The sibling `flush_deferred_part_inserts_with_local_state` uses this strategy, so
it was the obvious thing to copy. `bench_subfiles_index_cost` says not to:

| arm | total | insert | rebuild |
| --- | --- | --- | --- |
| both indexes live, key order | 4.84 s | 4.62 s | n/a |
| indexes dropped, key order | 4.63 s | 1.31 s | 3.07 s |

The rebuild costs 3.07 s to avoid 3.53 s of maintenance, a 4% net win, and it
would need the exclusive barrier for the whole flush, which on this path overlaps
the download and would stall its progress writes. Not worth the trade.

### rejected: MVCC wins on the one workload where Foxy's writes dominate

The third and most direct test of the MVCC question, and the one the engine bench
most favoured: `bench_mvcc_concurrent_writers` is 7.7x *faster* under MVCC, so a
433k-row insert overlapped with an active download was the best remaining case
for it. It loses by more than anywhere else measured: 23.0-23.3 s against
11.1-11.2 s at every gate size from 1 to 8, and the gap is entirely the one
insert (16.28-16.73 s against 5.86-5.93 s).

Foxy's largest write is not concurrent. It is one transaction on one connection
issuing 1 693 sequential chunks, which is the shape
`bench_mvcc_write_degradation` measures at 3.3x slower, not the shape MVCC is
good at. Zero retries and zero write failures in all eight configurations.

### accepted: drop `idx_subfiles_file_id_data_order`

Schema v25. Unique `(file_id, path)` stays. `Tree::load` and patch-plan reloads
already sort in process.

Ledger: `perf-db-parts-bulk`, run `20260909T180347Z-1a958354`, WAL gate 4,
release, CLI, dirty tree so no `--accept`. Combined with the batched file
upsert below.

| metric | before (sort-only, n=6 warm) | after (this run) |
| --- | --- | --- |
| deferred insert wall | 5.53-5.64 s | 4.33 / 4.54 / 4.32 s |
| operation elapsed | 10.56 / 10.85 s (median 10.71 s) | cold 8.82 s; warm 8.78 / 8.50 s |

About 1.1 s off the insert against the bench's 1.41 s. The remaining unique
index is the constraint.

### accepted: batched file upsert across mods

HTTP fetch stays per-mod and parallel. After join, one
`upsert_file_rows_batch` then parallel apply. Same run as above:

| metric | before | after |
| --- | --- | --- |
| `file upsert` calls | ~96 | 4 |
| batched upsert wall | 0.18-0.28 s across 96 statements | 30-31 ms for 96 mods |

Together the two changes take ~2.5 s off the ~10.7 s warm checker. Zero flags,
864 files, 433 248 parts.

### rejected: Turso 0.7 MVCC can beat WAL for Foxy's metadata rebuild

Ledger: `perf-db-refresh-main.jsonl`, runs `20260908T2018*` through
`20260908T2021*`, six warm iterations per variant, release, CLI harness, 96-mod
manifest mirror on loopback (3738 files, 433 063 parts).

| Variant | `remote-refresh` median | `wipe-db` median |
| --- | --- | --- |
| `wal` gate 1 | 2.077 s | 0.222 s |
| `mvcc` gate 1 | 2.308 s (+11%) | 0.356 s (+61%) |
| `wal` gate 4 | 1.663 s | 0.225 s |
| `mvcc` gate 4 | 1.744 s | 0.359 s |

MVCC is slower in wall clock at every gate size, and the exclusive purge is ~60%
slower under it regardless of the gate. The engine-level picture is much better
than it was on 0.6 (`bench_mvcc_write_degradation` 3.3x slower but no longer
quadratic; `bench_mvcc_concurrent_writers` 7.7x *faster*), but Foxy cannot
collect the concurrency win while `DB_WRITE_GATE` is 1. Full write-up in
`turso-improvement.md` section 2.3.

Two defects had to be fixed before this was measurable at all, and both are
worth keeping regardless of the verdict: `transaction_exclusive` used
`BEGIN CONCURRENT` for DDL under MVCC (every `wipe-db` failed), and the CLI sync
dropped its cancel sender immediately, racing every sync against an
already-ready cancellation.

### accepted: `DB_WRITE_GATE` above 1 is worth ~18% on the metadata refresh

Spun out of the MVCC work rather than planned. Swept gates 1, 2, 3, 4, 6 and 8
against both engines on `perf-db-refresh-main`, then re-tested on two more
workloads before changing the default to `min(4, cpus)`.

Metadata rebuild (96 mods, 433k parts), warm medians:

| gate | WAL elapsed | WAL permit_wait | MVCC elapsed | MVCC permit_wait |
| --- | --- | --- | --- | --- |
| 1 | 2.032 s (n=10) | 10.0 s | 2.265 s | 12.4 s |
| 2 | 1.755 s | 5.0 s | 1.969 s | 5.3 s |
| 3 | 1.679 s | 3.7 s | 1.806 s | 3.1 s |
| 4 | 1.659 s (n=10) | 2.8 s | 1.718 s | 1.8 s |
| 6 | 1.575 s | 1.7 s | 1.680 s | 0.2 s |
| 8 | 1.614 s | 1.1 s | 1.659 s | 0.004 s |

Gate 1 and gate 4 ranges are disjoint at n=10 (1.958-2.096 against
1.610-1.766), so the 18% is real rather than noise. The curve is flat from 4 to
8 while `db_write_time_ms` keeps climbing (925 ms at gate 4, 2 195 ms at gate 8),
which is the convoy moving from the gate into `conn.execute` - the reason the
default is capped at 4 rather than 8.

The win is confined to write-heavy work, and saying so is the point:

- File-count-heavy download (4 832 files, 21 MB): 23.083 s at gate 1 against
  22.622 s at gate 4 (n=8). Only 2% of the median, but gate 1's range reaches
  28.0 s while gate 4 stays inside 22.56-22.72, so the gate buys tail latency.
- Real 4 GB download on spinning media: 68.3 s against 65.7 s medians (n=4) with
  heavily overlapping ranges. Not separable from server variance; the gate waits
  are ~0.5 s of a ~68 s run. No regression, no win.

Safety: zero lock retries, zero write retries and zero write failures at every
gate size in every configuration measured, in both engines. The fan-out pressure
valve in `mod_task_limit` (which halves fan-out at 24 lock retries) never
engaged. The purge is unaffected - it holds `DB_EXCLUSIVE` and its timings are
identical across gate sizes. Correctness was re-verified at the new default by
the delta-patch case and its independent `b3sum` oracle: 3 744 parts byte-exact,
identical patch results.

Not covered, and the reason to keep `FOXY_DB_WRITE_GATE` as an escape hatch: the
87 GB / 66k-row hash-persist workload that motivated the original gate of 1
(`after_turso_regression_analysis2.md`) is not reproduced by any case here.

### accepted: pooling tuned connections and caching their compiled statements

The seam opened a fresh tuned connection for every `execute`/`query` on the 0.5
spike's "connections are ~16 us" note. On 0.7.2 a `Database::connect()` builds a
new pager, blocking-reads page 1, opens and closes a read transaction and
deep-clones the schema, and the engine's compiled-statement cache lives on the
connection, so a per-call connection also throws away every compiled program.

`bench_connection_reuse` (release, 20k-row `subfiles`):

| step | us/call |
| --- | --- |
| raw `db.connect()` | 14.4 |
| `connect_tuned()` (five pragmas on top) | 25.8 |
| small read, fresh tuned connection per call | 61.0 |
| small read, reused connection | 15.0 |
| small read, reused + `prepare_cached` | 5.2 |
| small write, fresh connection per call | 137.4 |
| small write, reused + cached | 20.5 |

Application level, same binary, `FOXY_DB_POOL_IDLE=0` against the default:

| case | connections | DB write time | elapsed |
| --- | --- | --- | --- |
| `perf-db-refresh-main` remote-refresh | 732 opened -> 24 opened / 708 reused | 966 -> 924 ms | 1 576 -> 1 526 ms (n=6, ranges overlap) |
| `perf-db-writes-synthetic` | 997 opened -> 21 opened / 976 reused | 2 244 -> 2 051 ms | 21.61 -> 21.60 s |

The separable win is on the bulk path, where the same statement shape repeats:
the deferred 4 832-row part flush goes from 56.7-62.3 ms (n=6, pre-change) to
49.9-52.0 ms (n=3), non-overlapping, and `bench_cached_chunk_knee` puts the
66 336-row insert at 15.6 us/row uncached against 11.6 us/row cached.

**Wall clock on both cases is unchanged**, and saying so is the point: 732
connections times ~56 us of avoided overhead is ~41 ms of a ~1.5 s operation.
This is a CPU and allocation win that matters where the database dominates (the
87 GB / 66k-row hash persist), not a wall-clock win on anything the kit can
currently reproduce.

Safety: a connection that is not in autocommit is dropped rather than pooled; a
DDL statement through the seam bumps a schema epoch that retires every pooled
connection, because a cached program is only rechecked against its own
connection's schema snapshot and that snapshot is refreshed only when a
statement is compiled; the purge retires its connection explicitly because it
sets `foreign_keys = OFF`. Covered by `pooled_connection_is_reused_after_release`,
`connection_in_a_transaction_is_not_pooled`, `ddl_retires_pooled_connections`
and `ddl_detection_covers_the_statements_the_seam_runs`. The delta-patch case and
its independent `b3sum` oracle re-verified 217 files / 3 744 parts byte-exact with
identical patch results.

### rejected: retune Turso 0.7 bulk-write chunks

`bulk_write_chunk_rows()` is 256 because per-statement parse and plan cost was
measured as superlinear in row count. With compiled programs cached the curve is
flat: 11.6, 11.6, 11.7, 11.7, 12.0 us/row at 256, 512, 1 024, 2 048 and 4 096
rows per statement (uncached: 15.6, 14.6, 14.5, 14.2, 14.6). The rationale for
256 is now weaker than it was, but no larger chunk is faster, so the value
stays.

### rejected: MVCC wins once the per-call `journal_mode` pragma is no longer paid

Worth retesting because every connect used to issue `PRAGMA journal_mode=mvcc`,
which the pool removes. Re-swept both engines over gates 1, 2, 4 and 8 on the
pooled build, three warm iterations each.

`perf-db-refresh-main`, remote-refresh medians:

| gate | WAL elapsed | WAL write | WAL gate wait | MVCC elapsed | MVCC write | MVCC gate wait |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 864 ms | 300 ms | 10 857 ms | 1 971 ms | 296 ms | 10 452 ms |
| 2 | 1 638 ms | 443 ms | 4 667 ms | 1 614 ms | 327 ms | 4 238 ms |
| 4 | 1 519 ms | 890 ms | 2 859 ms | 1 493 ms | 384 ms | 347 ms |
| 8 | 1 464 ms | 2 180 ms | 821 ms | 1 490 ms | 426 ms | 0.0 ms |

`perf-db-writes-synthetic`, force-redownload medians: WAL 21.76 / 21.63 / 21.70 /
21.86 s and MVCC 22.10 / 21.91 / 21.83 / 21.87 s at the same four gates. Purge
(`wipe-db`) is 211 ms under WAL and 359-369 ms under MVCC at every gate.

The engine claim is now demonstrated at application level: MVCC's write time is
nearly flat as the gate opens while WAL's grows 7x, which is the convoy moving
from the gate into `conn.execute`. It still does not reach wall clock. The
metadata rebuild's floor is ~1.46 s of which the writes are worth ~400 ms
(gate 1 against gate 8), and the rest is 526 ms of remote recheck, 172 ms
attaching 433k deferred part rows to the in-memory tree, and 347 ms of local
path preflight. Zero conflicts, retries or failures in 24 configurations.

Verdict unchanged: MVCC stays off. The case against it is now narrower and
better evidenced - it costs ~70% on the exclusive purge and buys wall clock
nowhere the kit can measure.

### note: per-category `txn_ms` is a function of the write gate

`addon_files insert` is 97 calls and 3 738 rows on the metadata rebuild. Its
reported time is not a property of the statement:

| gate | txn time (three iterations) | us/row |
| --- | --- | --- |
| 1 | 71.7 / 71.2 / 84.7 ms | ~19 |
| 2 | 105.9 / 148.3 / 134.3 ms | ~35 |
| 4 | 377.6 / 300.8 / 257.8 ms | ~69-101 |
| 8 | 1 145.8 / 569.8 / 714.7 ms | ~152-307 |

Same rows, same statement, 8-16x spread. Turso has one internal writer, so past
gate 1 the waiters block inside `conn.execute` rather than surfacing `Busy`, and
that wait is inside the category timer instead of `permit_wait_ms`. Gate 1 is the
uncontended reference at ~19 us/row; at the shipping gate of 4 roughly
70-80% of the reported time is queueing.

Two traps this closes. Comparing a category's time across runs at different gate
sizes measures the gate, not the change. And `bench_addon_files_insert` reports
10.4 us/row for the same statement, which is *lower* than the gate-1 figure
because the bench starts from an empty database - a micro-bench on a fresh table
is not a valid uncontended reference for a populated one.

The log line now prints `txn_ms` rather than `total_ms` and carries
`write_gate=` so the number cannot be read without its context.
