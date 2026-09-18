# Foxy Speed-of-Light Performance Convention

For every crucial operation Foxy names the work it had to do, a reference
time for that work, and the elapsed time it actually took, and it records
those three things so the ratio can be recomputed from a log alone. The
reference is the "light". The discipline is borrowed from speed-of-light
engineering (compare against the best the hardware and the algorithm could
do, not against last release), applied with one rule this document exists to
enforce: **every displayed ratio says which reference it compares against**.
A hardware bound, a measured best case, and consistency against a run's own
peak are different questions, and one percentage cannot stand for all three.

Numbers live in `conventions/SPEED_OF_LIGHT_MEASUREMENTS.md` (status table,
baseline registry, dated experiments, archived tracking rows). This file
owns definitions, equations, operation cards, the log grammar and the rules
for collecting and comparing.

Reading order: reference kinds -> work and time definitions -> units ->
operation index -> equations -> operation cards -> logging contract ->
collection and comparison rules.

## Resource trade policy

Foxy optimizes user-visible time: the seconds between a click and a
trustworthy result, and the seconds an update or a check takes. Memory and
disk footprint are the resources it deliberately spends to get there, and
that trade is the accepted one, in both directions:

- A change that makes hashing, checking, downloading, patching, refreshing
  or startup faster is welcome even when it holds more memory (bigger
  buffers, a warmer cache, a larger tree kept resident) or uses more disk
  (staging blobs, a larger database, kept fingerprints). The extra footprint
  is the price, not a regression.
- A change that lowers memory or disk footprint is never accepted when it
  makes any of those operations slower. Footprint is not a reason to shrink
  a buffer, drop a cache or serialize work.
- Footprint still has guardrails: the process must stay usable on the
  machines Foxy targets, growth must saturate rather than continue without
  bound (a leak is a bug, not a trade), and every time optimization records
  its peak and retained memory so the price is known. The M1 lanes report
  and explain; they do not gate. In the test kit a footprint move is
  `advisory-regression` / `advisory-improvement` with a `memory-advisory`
  flag and never decides a verdict; the time and correctness metrics do.

Phase 5 candidate "memory attribution" is therefore low priority and
pursued only for evidence of unbounded growth or a renderer or allocator
cost that is paid without any speed in return.

---

## Reference kinds

| Kind | Formula | Log / UI label | What it can and cannot say |
| --- | --- | --- | --- |
| Modeled lower bound | `T_lb / T_actual` | `metric_kind=modeled_bound`, "vs bound" | A lower bound under stated resource, algorithm and correctness assumptions. A configured bandwidth cap is one such bound (a policy ceiling); it is not necessarily the bottleneck |
| Calibrated estimate | `T_reference / T_actual` | test-kit `sol_calibrated` beside a `reference_id` (no in-app emitter; the app never guesses a device constant) | Measured sustainable capacity of this machine or path. Not a proof of maximum physical capacity; can be exceeded by a better implementation |
| Best measured | `T_best / T_actual` for identical useful work | "versus best measured" (saved benchmarks, test-kit baselines) | Empirical. Can exceed one when the candidate beats the frozen best |
| Peak consistency | `R_average / R_peak_window` of the same run | `metric_kind=peak_consistency`, "peak consistency" | Whether the run held its own peak. Can be high while the whole run is slow; a degraded peak hides a regression |
| Nominal | ratio against an uncalibrated device constant | "vs nominal" (download `disk_*` keys) | A reading aid. May exceed one on a warm run because logical bytes are counted, not device traffic |
| None | | `metric_kind=none`, `sol=na`, "reference missing" | Only the actual time is meaningful; compare with best measured |

For a valid physical bound and the same required work,
`T_lb <= T_optimum <= T_best_measured`. A best observed run is therefore never
"the physical limit", and a ratio above one against a claimed bound is
evidence about the bound (or the timer scope), not a 100% achievement. The
line keeps it as `sol_raw` with `reference_status=above_bound`; the legacy
`sol` field stays clamped to `[0, 1]` for old readers.

## Work and time

For every operation record, name these separately:

- `W_useful`: the required result (unique verified output bytes, selected
  files checked, a complete remote freshness verdict).
- `W_actual,r`: work actually consumed on resource `r`, including repeated
  hashes, retried downloads, patch staging and fallback attempts.
- `W_min,r`: the minimum resource work under the chosen correctness contract
  and algorithm family.
- `T_action`: user action to trustworthy terminal result, preparation and
  finalization included.
- `T_stage`: an explicitly named stage inside the action.

```text
execution_efficiency   = lower_bound(W_actual,r) / T_actual
useful_work_efficiency = lower_bound(W_min,r)    / T_action
work_amplification_r   = W_actual,r / W_min,r     (only when W_min,r > 0)
```

A downloader that retries every byte can show excellent transfer throughput
while doing twice the necessary work; a hash pass can get faster per byte by
re-reading warm data while the user's check gets slower. Track work and time
both. When the minimum work is zero, report absolute excess counters rather
than a ratio.

Do not demand identical physical I/O between implementations: avoiding a
redundant read is the point. Demand identical selected scope and verified
outcomes, then explain the reduced resource work. Distinguish unique
completed files from per-batch processing counts.

**Service time is not wall time.** Sums of parallel worker durations are
occupancy totals; the union of their intervals is occupied wall time; action
start to finish additionally includes gaps. A saved benchmark's folded
`actual_s` per operation is a service sum over that operation's batches and
can exceed the record's elapsed time; nothing may divide one by the other and
call it a share.

## Units (E0)

Machine records use bytes and integer nanoseconds. Presentation formats
`MB = 1,000,000 bytes`, `MiB = 1,048,576 bytes`, `Mbps = 1,000,000 bit/s`.

| Log text | Actual unit | Conversion |
| --- | --- | --- |
| `... MB/s` in the download report and `Download sample:` | MiB/s | `bytes / 1024^2 / s` |
| `... Mb/s` (`Download avg speed over last 30s`) | decimal megabits/s | `bytes / 125,000 / s` |
| Download limiter setting (`Mbps`) | decimal megabits/s | `cap_bytes_per_sec = mbps * 125,000` |
| `SOL` lines (`work_bytes`, `*_bps`, `actual_ns`) | raw bytes, bytes/s, ns | none; use these for arithmetic |
| ISP plan "500 Mbit" | decimal megabits/s | `* 125,000 -> bytes/s` |

Never convert an ambiguous historical number silently: keep its original text
and mark the normalized value unknown until the raw counters are recovered.
`actual_s` carries three decimals, so sub-millisecond work rounds to zero
there; `actual_ns` and `actual_bps` are computed from the unrounded duration.
Record what "complete" means for a timer (buffered writes accepted, files
promoted, durable) when it is not obvious from the operation card.

## Operation index

| Id | Operation | Emitter | Reference available in-app |
| --- | --- | --- | --- |
| O1 | Full-file download | `download_files/orchestrator.rs` | limiter cap (modeled) or same-run peak (consistency); nominal disk on rotational destinations |
| O2 | Delta patch | `SOL op=delta_patch` action and `SOL op=delta_patch_stage` spans in `download_files/orchestrator.rs` | none; byte savings only |
| O3 | Content hashing and ordered tree verification | `calculate_hashes/scheduling.rs` | none (self baseline); saved benchmarks derive a same-run peak across homogeneous batches |
| O4 | Quick scan | `quick_scan/diff.rs` | none (self baseline) |
| O5 | Remote metadata refresh | `SOL op=remote_refresh` in `tasks/remote_repository.rs` | none in-app (self baseline); the kit calibrates the no-change branch through O6 |
| O6 | No-change sync | `SOL op=sync_action` with an `early-exit-*` outcome, `sync_pipeline/summary.rs` (every pipeline exit emits one) | none in-app; test-kit `sol_calibrated` against B4 |
| O7 | Turso persistence | `SOL op=db_persist` in `sync_pipeline/hashing.rs` (per sync action); `db:` line of the download report, `SQLite sync metrics:` (legacy name) | none |
| O8 | Startup to settled verdict | `ui/app/runtime/startup_sync.rs`; probe stage in `quick_scan/worker.rs`; app update check in `tasks/app_update/spawn.rs` | none (self baseline) |
| M1 | Resident footprint | `foxy-testkit` memory lane | empty-app baseline (best measured) |
Sub-operations without an emitter of their own (download preparation and
finalization are readable as `sync_action` `stage_*_s` keys; hash profile
calibration as `label=auto_benchmark_sample`
hash batches; DB purge and
maintenance, GUI interaction under load, cancellation and resume, space
switch, idle app) get an id only when their contract and emitter exist.
Keep the O1-O8/M1 anchors; never renumber historical records.

---

## Equations

`W` = work, `R` = rate, `T` = time. E1 and E2 are implemented in
`src/core/utils/speed_of_light.rs`; the rest are models for reading and for
building references, each with the conditions under which it holds.

| # | Equation | Holds when | Does not say |
| --- | --- | --- | --- |
| E1 | `T_service_lb = W_min / R_capacity` | One homogeneous resource at a sustained rate; the access pattern is part of the rate (sequential cold bytes/s cannot normalize cached reads, random part reads or row counts) | Anything about tiny requests, metadata, seeks, setup or dependency chains; add measured minimum latencies only where they are serial with the work |
| E2 | `sol_raw = T_ideal / T_actual = R_actual / R_light`; `sol = clamp(sol_raw, 0, 1)` | Both rates use the same work and time scope; `T_actual > 0` and both times finite | A ratio above one is a mismatch (bytes, stale capacity, timer scope) or a better empirical baseline, never an achievement; zero-work operations need a latency model or `na`, not a 0% throughput score |
| E3 | `H = 1 / sol`; `gap_s = T_actual - T_reference`; `slower_percent = 100 * (T_actual / T_best - 1)` | Read as distance to the named reference | Achievable speedup. Rank work by plausible user-visible seconds saved times frequency, evidence strength and cost, not by `1/sol`. Use Amdahl `1 / ((1 - f) + f / s)` only with `f` the affected serial critical-path fraction |
| E4 | `T_resource_lb = max_r(service_demand_r)`; `T_dependency_lb = longest_required_path`; `T_lb = max(T_resource_lb, T_dependency_lb)` | Feasible overlap on independent resources with compatible capacities | Do not add the two bounds (they may cover the same work). One shared disk models read and write jointly (`W_read/R_read + W_write/R_write`) or with a measured mixed-I/O rate. CPU, network and disk bytes are different demands |
| E5 | Serial stages sum; fully overlappable stages on independent resources bound by their maximum; `n` identical items through dedicated pipeline stages: `sum(s_i) + (n - 1) * max(s_i)` | Stated structure; real Foxy batches differ in size, share disks and contain barriers, so use event spans and dependencies | `T_actual - T_lb` is unexplained gap, not removable overhead: it includes protocol work the model omitted, shared resources, uncertainty and stragglers |
| E6 | `T_ideal = D * RTT + W / R_net`; homogeneous non-pipelined requests at one level with concurrency `C` and occupancy `L`: `ceil(N / C) * L` | `D` counts dependent round trips of a stated graph (connection setup, TLS, redirects and application dependencies included or excluded explicitly); reused HTTP, fresh HTTP and HTTPS are different graphs | Parallel requests are not free: they share bandwidth, server workers, connection limits, client permits and parse CPU. A 433k-part metadata rebuild has parse and persistence work whatever its HTTP depth |
| E7 | Per lane with chunk `B`, latency `L`, per-lane rate `r`: `request_service_s = L + B / r`; `R_aggregate <= min(R_path, C * B / request_service_s)`; `C_needed >= ceil(R_target * request_service_s / B)` | `r` calibrated at the relevant concurrency; one request in flight per lane | The older `C_min = R_target * RTT / chunk_bytes` is a latency-hiding heuristic, not a sufficient connection count. BDP describes in-flight bytes, which socket windows can supply without `C` connections |
| E8 | `T_service = T_fixed + sum(hit_cost_i) + sum(miss_cost_j)`, mapped onto the actual schedule | Entries enumerated, metadata calls, directories traversed, suspicious files, sampled bytes and hit/miss reasons are recorded | Equal addon counts do not imply equal filesystem work; addons/s is not stat entries/s. Cache reuse must stay safe across restart, local drift, remote change, same-URL separate folders and space switching |
| E9 | Aggregate throughput is a measured curve `R(C, B, protocol, origin, cache_state)` | Independent lanes with a stable per-lane ceiling and enough demand | `R_agg = min(R_link, C * R_conn)` with one universal `R_conn` (B7 shows 1.6 MB/s at C=1, 1.15 at 48, 0.88 at 96). Keep the 96-versus-192 no-gain finding for its origin, not as a universal rule |
| E10 | Tail: `T_tail >= max_i(remaining_bytes_i / rate_ceiling_i)` and `T_tail >= sum_i(remaining_bytes_i) / R_link_ceiling` | Verified rate ceilings; queue-empty, last-network-byte and terminal timestamps recorded | `chunk_bytes / R_conn` bounds one residual transfer from above under fixed rates, not the minimum tail. Track tail duration separately from throughput deficit; chunk size, ordering and fair scheduling all affect it. Ramp is not categorically outside client control (reuse, dispatch delay, request preparation, concurrency); TCP congestion control (RFC 5681) sets a floor for a given path |
| E11 | `M_process(t) = M_base(t) + M_state(t) + M_work(t) + M_unattributed(t)`; `M_peak = max_t`; `M_retained` = declared statistic over a fixed quiet window | Disjoint buckets sampled at a stated rate with sample count and gaps recorded | `M_ideal = floor + state + op` as a physical minimum: an empty process includes runtime, allocator, config and engine costs; summing separately observed peaks overstates a simultaneous peak; a renderer, allocator or state representation choice moves the baseline itself. Private commit (`PrivateUsage`) and working set are not interchangeable |

Download deficit against a fixed reference:
`deficit_s = integral(1 - R(t) / R_reference) dt = T - W / R_reference`, over
measured interval widths. With a valid fixed ceiling and matching byte domain
it decomposes additively into ramp, plateau and tail. Do not clip negative
intervals silently (they expose a poor reference or a burst allowance), and
define the phase boundaries before comparing runs. A token bucket with burst
`B0` gives `T >= max(0, (W - B0) / R_cap)`; record burst and dynamic cap
changes rather than treating a short transfer as steady state.

---

## Operation cards

Each card: (1) operation and user-visible completion, (2) reference kind and
required baseline, (3) best-case comparison and current measurements,
(4) required work and scope, (5) dependencies and assumptions, (6) counters
and timer boundaries with producers, (7) diagnostics, candidates and
correctness gates.

### O1 - Full-file download and the complete update action

1. **Completion**: every selected file verified and promoted, database
   finalized, completion visible. The transfer stage is a sub-span.
2. **Reference**: with a limiter, the cap (modeled bound, policy). Without,
   the run's own peak window (peak consistency). Independently calibrated
   path throughput (B1) is the reference for a physical statement and is not
   emitted in-app.
3. **Comparisons**: saved benchmarks compare against the reproducible median
   of compatible prior frozen records and retain the fastest compatible prior
   run as secondary evidence. Compatibility includes operation and initial
   state, cache preparation, repository instance, origin/payload fingerprint,
   storage/build/algorithm/diagnostics, useful work, metric/model version,
   timer scope and reference IDs. The candidate is never part of its own
   reference set, so a genuine improvement may exceed 100%. The test kit uses
   accepted baseline medians. Status: measurements file section 1.
4. **Work**: selected output bytes (`full_bytes`), successful response-body
   bytes credited to files (`credited_bytes`), the shared transfer counter
   (`work_bytes`, an application counter, not TCP/IP traffic with headers),
   `expected_bytes`, `range_retries`. `work_bytes` and `credited_bytes` differ
   under retries, resume and fallback; neither is verified output bytes. The
   completion line's `avg_speed` uses credited bytes.
5. **Dependencies**: queue preparation -> transfer (ranges in parallel, shared
   permits and link) -> on-arrival hashing overlapped -> verification and
   promotion -> database finalization. `R_allowed = min(R_cap, R_path, other
   capacities)`: a cap above disk or server capacity does not make a low
   cap-relative ratio wasted bandwidth. On rotational destinations the disk
   is the likelier bound; the `disk_*` keys give a nominal reading only.
6. **Counters and timers**: `SOL op=download` at the end of the transfer
   stage (`actual_s` is the stage, not the action), `outcome`,
   `mods_succeeded/failed/cancelled`, `peak_1s_bps` with `peak_window_s`,
   `delta_savings_bytes`, `destination_storage`, `op_id`. The sampler
   (`download_files/metrics.rs`) reports byte delta over the measured interval
   width; a window shorter than 0.5 s (immediate wake, final partial interval)
   contributes to the series but never to the peak. The retained series
   splits the stage into `ramp_s` (before the first window at 90% of the
   peak), `plateau_s` and `tail_s` (after the last such window), with
   `ramp_deficit_bytes` and `tail_deficit_bytes` the bytes each fell short
   of the peak rate; `Download shape:` lists the ramp windows with their
   active files and ranges. `-- DOWNLOAD REPORT --`
   carries per-file and per-range percentiles, permit waits and DB checkpoint
   figures. `download_stage_ms` includes joining background tasks; new tasks
   in the path must wait on interruptible primitives, never a bare sleep.
7. **Diagnostics**: read `ramp_s`, `plateau_s` and `tail_s` against a fixed
   reference before touching anything; a plateau on the path ceiling with a
   poor ratio is a ramp or tail problem, and the deficit bytes say which. A
   resumed run after a cancellation must not refetch a file that still exists
   at full length and matches its persisted content fingerprint: the
   `prepared_queue_prune` stage counts the files the reused queue dropped.
   `permit_wait` high -> fair-share starvation; range latency percentiles high -> ranges too small for the RTT
   (E7); disk averages near the device rate -> disk-bound (E4). Gates: same
   verified payload (oracle), no hidden retry or memory growth, `outcome=completed`.

### O2 - Delta patch

1. **Completion**: each patched file equals the remote file (segment
   verification and final payload oracle), with automatic fallback to a full
   download when plan validation or apply fails.
2. **Reference**: none. `max(network, disk)` across files is an optimistic
   resource bound, not the dependency chain: the orchestrator
   (`delta_patch/orchestrator.rs`) finishes a file's insert blob before
   applying that file.
3. **Comparisons**: byte and elapsed savings are established by the tracked
   same-initial-state control: the patch and full-file arms repair the same four
   mismatched files and finish with independently verified equivalent payloads.
4. **Work**: `byte_savings = 1 - fetched_unique_insert_bytes / full_output_bytes`
   (`delta_savings_bytes / full_bytes` on the download line), network
   amplification including failed attempts, source-copy reads, insert-blob
   writes and reads, output writes, verification reads, checksum CPU, seeks
   and fallback work, with a note on which hit the device and which the page
   cache.
5. **Dependencies**: preflight -> blob fetch/staging -> apply (capped by
   `patch_applies`) -> promotion -> verification -> finalization/persistence
   where it gates completion. `W_full / W_net` predicts speedup only when network bytes
   dominate both complete actions. The blob fetch draws on the same global
   range budget and chunk size as full downloads (`PatchRequestBudget`):
   ops that fit a request coalesce into runs, ops larger than one travel as
   parallel chunks written straight into the blob and are verified from the
   blob afterwards, so a plan of a few huge inserts still uses the whole
   budget. The kit sums the requests (`patch_range_requests`), the bytes
   over-fetched to coalesce them (`patch_gap_bytes`) and the source copy
   bytes (`patch_copy_bytes`) per operation, which is how an adjacent-run
   mutation and a scattered one of the same size are told apart. Against an origin that caps every connection, that is the
   difference between 4 connections per file (measured 25 MB/s, 16-17 s for
   40 files) and the path ceiling (113 MB/s, 6.9-7.5 s, 2026-09-16).
6. **Counters**: `Parallel delta blob download: ... requests= chunk_requests=
   bytes= elapsed= speed= requests_cap= run_max_bytes=` for the network leg;
   `Delta patch applied successfully: ... preflight= download= apply_wait=
   apply= verify_promote=` per file; `patch plan`, `falling back` lines with
   reasons; `hash_source=segments` versus `reread` in `Incremental hash
   sources:`. An apply-time fallback (`Delta patch fallback for file_id=`,
   counted as `patch_fallbacks`) is the path a silently changed source
   takes: the test kit reaches it with a mutation that preserves size and
   modification time, so no fingerprint retires the plan first. Sum the per-file `download` spans against the stage elapsed to
   see how much of the stage the blob fetch occupies.
7. **Candidates**: source locality and apply scheduling on evicted HDD
   sources (the apply cap, not the fetch, bounds the rotational lane now),
   blob-fetch latency for tiny plans. A time-based patch admission policy
   would need an explicit policy revision, validated estimates and a
   same-state full-download comparison; low network consistency alone is not
   a reason. Never weaken plan validation, ordered hashes or fallback to
   improve a ratio: a chunked op that fails its blob checksum falls back to a
   full download like any other failed patch.

### O3 - Content hashing and ordered tree verification

1. **Completion**: every selected file's parts hashed with the required
   algorithm and the ordered rollups persisted; a first check additionally
   includes profile calibration.
2. **Reference**: none in-app (`self_baseline`). Saved benchmarks derive a
   same-run peak across batches only when the batches share one `label`;
   calibration trials and the production pass are not comparable and get no
   ratio. Cold data compares against matched disk access plus algorithm CPU;
   warm data against memory bandwidth and CPU. B6 is the compute term and is
   combined with matched B2 read capacity; neither alone is a complete bound.
3. **Comparisons**: best measured by `hash_files_total` and
   `hash_parts_total`; test-kit `breakdown.run_metrics.hash_total_s` (summed
   over every batch) is the gated metric, never the last batch's `hash.actual_s`.
4. **Work**: `work_bytes` = payload bytes hashed in the batch. Distinguish
   useful payload, benchmark trial bytes (`label=auto_benchmark_sample`),
   repeated verification, layout and sampled fingerprint bytes.
5. **Dependencies**: file/part concurrency under `hash_scheduler_limits`;
   the run ends when the slowest file ends (`file_elapsed_max_s`).
6. **Counters and timers**: `SOL op=hash` per batch with
   `timer_scope=batch_wall`; `blocking_elapsed_s` (summed blocking-task
   elapsed, reads and scheduling included, not CPU service) and
   `permit_wait_s` (summed semaphore wait). `compute_s` and `wait_s` are the
   same two numbers under their legacy names; the old reading of them as CPU
   time and I/O wait was wrong. `Hash part run metrics:` carries the metadata,
   layout and straggler breakdown; `Hash profile auto benchmark sample:` the
   calibration trials.
7. **Candidates**: straggler splitting and scoped trees. Cross-run profile
   reuse is closed unless calibration costs seconds: safe reuse needs storage,
   workload and build invalidation and a stale choice costs more than the
   current NVMe trial. Calibration itself hashes every sample
   byte exactly once: each candidate profile trials its own disjoint group of
   the sample (`Hash profile auto benchmark sample: ... group=i/n`), dealt
   round-robin from the part-heaviest files so the groups carry like work,
   and a sample too small to feed every profile trials the first ones only.
   Before 2026-09-16 every profile re-hashed the same sample, so the second
   and third trials read the first trial's page cache and the selection
   measured cache warmth (10 GB/s on NVMe) rather than the profile. Gates:
   ordered rollups and hashes unchanged, `missing_files=0`, same selected
   scope, `hash_work_bytes` equal to the payload on a first check.

### O4 - Quick scan

1. **Completion**: a trustworthy clean/updates verdict for the selected
   addons with pending updates preserved.
2. **Reference**: none. The lower bound is cache lookup plus necessary
   enumeration/stat work, required DB reads and any sampled reads (up to
   128 KiB per suspicious file), scheduled at the actual concurrency. DB
   validation and targeted escalation are required work, not overhead.
3. **Comparisons**: B5's addons/s is `legacy`; re-baseline by entry count,
   cache state and scope before quoting a percentage. Read the operation's
   own `actual_s`, not the test-kit `elapsed_s`: the stale-check lane's
   0.445 s runner elapsed is driver round trips around a 0.021 s scan
   (2026-09-16), so there is no fixed overhead to remove there.
4. **Work**: `addons_total`, `addons_hashed`, `cache_hits_shared`,
   `cache_hits_persistent`, `deep_scan_files`; record selected addon and entry
   counts so disabled scope, shared folders or one touched file cannot
   masquerade as a speedup.
5. **Assumptions**: a clean verdict with deep-scan work is not proof of false
   suspicion (timestamps can change while content stays equal); diagnose
   invalidations by reason and scope.
6. **Counters**: `SOL op=quick_scan` with `outcome`; `Quick scan summary:`
   names the check that triggered escalation; `Quick scan timings:` (debug)
   splits `db_load`, `addon_hash`, `file_fallback`, `tree_verify`.
7. **Gates**: no part-range reads, no file-row loads for clean addons, no
   full-tree loads on the no-change path (sync convention); the outdated but
   untouched path hashes zero payload on the second check.

### O5 - Remote metadata refresh

1. **Completion**: the repository's remote tree is current and persisted.
2. **Reference**: none in-app (`self_baseline`). Model network dependency
   depth and bounded fan-out, then parse and DB service on the real critical
   path; do not score a large refresh against RTT alone. The test kit gives
   the no-change branch a calibrated latency estimate through the action
   record (O6); a rebuilt graph has no reference yet (parse and persist
   dominate and B5/DB references are still open).
3. **Comparisons**: `SOL op=remote_refresh` (`outcome`, `index_requests`,
   `manifest_requests`, `mods`, `files`, `parts`, `response_bytes`, the
   summed `fetch_sum_s` / `parse_sum_s` / `persist_sum_s` service times and
   `fan_out_wall_s`) against the same case's history; the `PIPELINE SUMMARY`
   row `remote_repository` is the same span.
4. **Work**: remote probe, changed-manifest fetch, parse/decode, tree
   construction, persistence; total versus changed manifest count and rows.
   `outcome` names the branch: `skipped_clean` (checksums match, nothing
   fetched beyond the index), `graph_unchanged` (index fetched, stored graph
   reused), `rebuilt` (manifests fetched and persisted), `failed`.
5. **Assumptions**: fetch count proportional to changed addons (a refresh
   that fetches every manifest for one changed addon is a scope bug); zero,
   one, many and all changed addons are different cases. The `*_sum_s`
   fields are service sums over parallel tasks, never wall time.
6. **Counters**: the `remote_refresh` line; debug `Fetched response body for
   ...` per fetch.
7. **Gates**: only changed scope fetched and persisted where allowed.

### O6 - No-change sync

1. **Completion**: current remote metadata checked, the local drift contract
   satisfied, stored state usable, a clean verdict surfaced.
2. **Reference**: the terminal record is `SOL op=sync_action` with an
   `early-exit-*` outcome (`self_baseline` in-app). The test kit attaches
   `sol_calibrated` = (one fresh B4 request plus one reused request per
   further index request) / `actual_s`, citing the latency lane it used.
   "One GET plus O(1) DB reads" is one fast-path condition, not every clean
   operation; `2 * RTT + 50 ms` is a service budget, not a physical identity;
   there is no unexplained constant term.
3. **Comparisons**: `sync_action` (`op_id`, `mode`, `outcome`, `stages`, one
   `stage_<name>_s` per pipeline stage) against the same case's history; the
   `Pipeline summary: op=... outcome=... elapsed=` line is the same span.
4. **Work**: remote requests (`remote_refresh.index_requests`), local
   validation scope, and whether local metadata validation was required or
   already validly completed.
5. **Counters**: the `stage_*_s` keys name any stage that ran when the sync
   convention says it must not (DB churn, tree loads, content refresh on the
   clean path).
6. **Gates**: exact clean contract; no metadata rebuild or hash merely because
   a cache key changed.

### O7 - Turso persistence

1. **Completion**: rows durably written under the engine's configured
   synchronous mode.
2. **Reference**: none for the mixed action counters. The DB calibration lane
   records keyed inserts, updates and deletes separately, while `db_persist`
   and `db_purge` currently combine statement kinds; applying one lane to the
   total would fabricate a ratio. `N_txn * t_fsync` does not describe a commit:
   `connect_tuned` (`tasks/db_turso.rs`) sets `synchronous=NORMAL`,
   so model actual sync events, WAL bytes, index work, checkpoint work and the
   single serial writer against the pinned engine, not SQLite estimates.
3. **Comparisons**: same engine version, journal mode, synchronous mode,
   write gate, pool policy, schema version, row/index shape and initial DB
   state; the WAL/MVCC decision and gate figures are in the measurements file
   and `conventions/CORE_CONVENTIONS.md`.
4. **Work**: rows and statements per batch (`db:` line), scanned versus
   affected rows (a delete affecting zero rows can scan many), WAL bytes.
5. **Assumptions**: `db_write_time_ms` and `txn_ms` are gated transaction
   windows with queue time charged inside them above gate 1; never a
   percentage of wall time, never compared across gate sizes.
6. **Counters**: `SOL op=db_persist` once per sync action (`op_id`, `mode`,
   `outcome`, `write_time_ms`, `rows_affected` (rows the engine changed
   across every write statement of the action), `permit_wait_ms`,
   `write_calls`, `write_committed`, `write_failed`, `lock_retries`,
   `backoff_ms`, `categories`, `write_gate`, `conn_opened`, `conn_reused`;
   `actual_s` is the action wall time the windows sit inside), `db: checkpoint_batches=
   rows= statements= total= avg_batch=`, `SQLite sync metrics:` (legacy name;
   the engine is Turso), `SQLite write category metrics` (`txn_ms`,
   `write_gate=`).
7. **Gates**: unchanged durability contract, no live-database edits, sibling
   repository instances preserved on purge.

### O8 - Startup to settled verdict

1. **Completion**: a painted window, then a freshness verdict for every
   configured repository. Distinguish first painted frame, first interaction,
   first repository verdict and all-repository settlement; `SOL op=startup`
   emits at settlement, which can include queued rechecks.
2. **Reference**: none in-app (`self_baseline`); the logged timeline cannot
   yield a physical ratio without a compatible renderer and network baseline.
   For branches that genuinely start together,
   `max(T_paint_branch, T_verdict_branch) + T_required_join`, with branch
   start offsets and contention; never paint plus a probe that overlaps it.
3. **Comparisons**: the disputed 74% and the corrected arithmetic are in the
   measurements file.
4. **Work**: launch/setup, renderer initialization, background eligibility and
   probes, local validation, queued rechecks, UI presentation. An unknown or
   timed-out repository is not a successful verdict; report answered, unknown,
   changed and failed counts (`SOL op=startup_probe`).
5. **Timers**: `first_frame_s` and `dispatch_s` are offsets from launch;
   `eligibility_s` and `verdict_s` are durations from dispatch; they cannot be
   added blindly.
6. **Counters**: `SOL op=startup` (`repos`, `quick_scan_repos`, `eligible`,
   `prevalidated`, `remote_changed`, `rechecks` and the four timers);
   `Quick scan preflight timings:` per repository.
7. **Invariant**: startup work must not block first paint; a frame stall
   during probes is a regression regardless of ratios. One slow or offline
   host must not hold the others: the test kit's in-process origin takes a
   `delay_ms` impairment and an `unreachable` repository entry, so a startup
   graph with a fast host, a 3 s host and a closed port is a case, not a
   story (`perf-startup-adverse-origins`).

### M1 - Resident footprint

1. **Completion**: a fixed quiet window after the operation.
2. **Reference**: the empty-app baseline by renderer backend and process
   state (B8) is best measured, not a floor; loaded-state and per-operation
   deltas are separate lanes.
3. **Comparisons**: peak, retained, growth and long-run growth per operation;
   pair every time optimization with peak memory and UI responsiveness.
4. **Work**: the state the operation must hold (repository list and
   settings, rows an in-flight read is building, the glyph atlas, device
   objects).
5. **Assumptions**: E11 above. The minimum-over-quiet-window retention metric
   is a lower envelope; keep it for history and add median and end-of-window
   values with the window length. A 100 ms sampler misses short peaks: record
   sample count and gaps, and never subtract a lifetime high-water mark to
   invent an operation peak.
6. **Counters**: `memory.peak_private_bytes`, `retained_private_bytes`,
   `growth_private_bytes`, `transient_private_bytes` from `foxy-testkit`;
   `agent-gui memory --textures` for Foxy's own buckets and the atlas.
7. **Gates**: same useful work (bytes, files, parts); a footprint improvement
   that moves less required work is not an improvement. Per the resource
   trade policy above, these lanes are advisory: a higher footprint beside a
   faster operation is the accepted price, a lower footprint beside a slower
   one is a regression of the operation, and only unbounded growth is a
   defect in its own right.

---

## The SOL log line

Every crucial operation emits one info-level line built by
`utils::speed_of_light::sol_line`. Grammar (stable, append-only: add keys,
never rename or remove them; test-kit regexes are position-sensitive on the
`SOL op=<name> actual_s=` prefix and on `work_bytes=`):

```text
SOL op=<name> actual_s=<secs> [work_bytes=<n> actual_bps=<n>]
    [light_bps=<n> ideal_s=<secs>] sol=<0.000..1.000|na>
    light_src=<limiter_cap|peak_1s|self_baseline>
    actual_ns=<n> sol_raw=<ratio|na>
    metric_kind=<modeled_bound|peak_consistency|none>
    reference_status=<ok|missing|above_bound|invalid_actual>
    metric_version=2 [key=value ...]
```

Keys start with an ASCII letter and continue with ASCII letters, digits, `_`,
`.` or `-`. Unknown keys are accepted so the grammar remains append-only.
Values may be double-quoted to carry spaces; inside quotes, `\"` represents a
quote and `\\` represents a backslash. The last occurrence of a repeated key
wins. Repeating a reserved contract key (`op`, the timing/work/reference keys
shown above, or the parser-owned status keys) also marks the record malformed.
Unterminated quotes or escapes, unsupported escapes, invalid keys, bare tokens
and characters after a closing quote are malformed. Both parsers retain all
fields they can recover and append `parse_status=malformed` plus a stable,
comma-separated `parse_error`; valid legacy records do not gain either field.
Integer counters are printed and parsed as integers. Legacy meaning of `sol`
(clamped) is preserved; `sol_raw` keeps the unclamped ratio and `actual_ns` the
unrounded duration. `metric_version=1` lines (before 2026-09-16) lack the four
appended keys; parsers treat them as `metric_kind` inferred from `light_src` and
`sol_raw` recomputed from `ideal_s / actual_s` when both are present.

| Line | Extra keys |
| --- | --- |
| `SOL op=download` (end of the transfer stage) | `files`, `peak_1s_bps`, `delta_savings_percent`, `destination_storage`, `op_id`, `outcome` (`completed`, `failed`, `cancelled`), `mods_succeeded`, `mods_failed`, `mods_cancelled`, `full_bytes`, `delta_savings_bytes`, `expected_bytes`, `credited_bytes`, `range_retries`, `peak_window_s`, and once a plateau window exists `ramp_s`, `plateau_s`, `tail_s`, `ramp_deficit_bytes`, `tail_deficit_bytes`; on rotational destinations `disk_bytes`, `disk_light_bps`, `disk_ideal_s`, `disk_sol`, `disk_light_src=nominal_hdd_sequential`, `disk_sol_raw`, `disk_reference_status=nominal` |
| `SOL op=delta_patch` (one aggregate action artifact) | `op_id`, `parent_op_id`, `span_id`, monotonic `start_offset_ns`/`end_offset_ns`, attempts, successful patched files, fallbacks, cancellations, requests, retries, useful output, unique insert, actual received, source-copy and staging bytes, planning/fetch/apply/promote/verify/finalize service times, their compatible `verify_promote` total, byte-conservation status, terminal outcome and `timer_scope=action_wall` |
| `SOL op=delta_patch_stage` (one typed per-file stage span) | `record_kind=stage`, `op_id`, `parent_op_id`, attempt `parent_span_id`, unique `span_id`, `stage_id` (`planning`, `fetch`, `apply`, `promote`, `verify`, `finalize`), `file_id`, monotonic `start_offset_ns`/`end_offset_ns`, stage outcome and `timer_scope=stage_wall` |
| `SOL op=hash` (every part-hash batch) | `label`, `files`, `parts`, `compute_s`, `wait_s` (legacy names), `blocking_elapsed_s`, `permit_wait_s`, `file_elapsed_max_s`, `missing_files`, `profile`, `algorithm` (`blake3`, `md5`, `mixed`, `unknown`), `timer_scope=batch_wall`, `outcome` (`completed`, `cancelled`), `op_id` (when run inside an action) |
| `SOL op=quick_scan` | `repo`, `addons_total`, `addons_hashed`, `cache_hits_shared`, `cache_hits_persistent`, `deep_scan_files`, `entries` (directory entries the fingerprint walks enumerated), `addons_per_s`, `outcome`, `op_id` (the owning sync action or quick-scan sweep) |
| `SOL op=remote_refresh` (every remote metadata refresh) | `outcome` (`skipped_clean`, `graph_unchanged`, `rebuilt`, `failed`), `index_requests`, `manifest_requests`, `mods`, `files`, `parts`, `response_bytes`, `fetch_sum_s`, `parse_sum_s`, `persist_sum_s`, `fan_out_wall_s`, `timer_scope=action_wall`, `op_id` |
| `SOL op=sync_action` (every pipeline exit) | `op_id`, `mode`, `outcome` (the `PIPELINE SUMMARY` outcome: `completed`, `early-exit-clean`, `early-exit-skip`, `cancelled`, `failed-*`, ...), `stages`, `timer_scope=action_wall`, one `stage_<name>_s` per stage |
| `SOL op=db_persist` (every sync action) | `op_id`, `mode`, `outcome` (`completed`, `early_exit`), `write_time_ms`, `rows_affected`, `permit_wait_ms`, `write_calls`, `write_committed`, `write_failed`, `lock_retries`, `backoff_ms`, `categories`, `write_gate`, `conn_opened`, `conn_reused`, `timer_scope=action_wall` |
| `SOL op=db_purge` (every repository or addon purge) | `op_id`, `kind` (`repository`, `addon`), `outcome`, `steps` (statements), `rows_affected`, `txn_s`, `checkpoint_s`, `timer_scope=action_wall` |
| `SOL op=space_switch` (every runtime game-space switch) | `op_id`, `outcome`, `drain_s` (queued saves landing in the old space), `reset_s`, `reload_s`, `repositories`, `timer_scope=action_wall` |
| `SOL op=startup` | `repos`, `quick_scan_repos`, `eligible`, `prevalidated`, `remote_changed`, `rechecks`, `first_frame_s`, `dispatch_s`, `eligibility_s`, `verdict_s`, `outcome=settled`, `op_id` |
| `SOL op=startup_probe` | `repos`, `answered`, `changed`, `unknown`, `first_answer_s`, `last_answer_s` (offsets of the first and last branch to answer, so one slow host reads as the gap between them), `outcome` (`complete`, `partial`), `op_id` (shared with `startup`) |
| `SOL op=app_update_check` | `op_id`, `mode`, `outcome` |

Ownership: every line emitted inside an action carries that action's
`op_id` (the sync pipeline stamps its id on the `FoxyContext` it hands to
hashing, quick scans and the remote refresh; the startup timeline and its
probe share `startup_operation_id()`; a quick-scan sweep stamps its own).
A saved benchmark keeps only lines whose `op_id` matches its own operation
id, plus lines without one (a bare CLI hash run, older logs), which are
attributed by time frame. Use opaque ids for correlation and never log
local paths or credentials; references involving game-space state stay
scoped to the active space (no unkeyed process-wide `OnceLock`).

Action records versus stage records: `sync_action`, `remote_refresh`,
`db_persist`, `startup` and `quick_scan` have `timer_scope=action_wall` or
are single spans; `hash` lines are per-batch service spans
(`timer_scope=batch_wall`) whose sum can exceed the action's wall time when
batches overlap a download, and `download` is the transfer stage. Read the
action's makespan from `sync_action.actual_s`, never from a sum of stage
lines.

Logs live in `%APPDATA%\Foxy\logs\foxy_rCURRENT.log` (rotated files
alongside). Debug-only cross-check lines (1 Hz `Download sample:` with
`interval_ms` and `bytes`, `Quick scan timings:`, `Fetched response body ...`)
need `RUST_LOG="warn,Foxy=debug,foxy=debug"` or the "Extended diagnostics
logging" setting (`extended_diagnostics_logging` in `app_settings.json`, also
`foxy settings --extended-diagnostics-logging true`), which also turns on the
per-operation `PROFILE` report, `PROFILE slow db` / `PROFILE slow fs` lines and
the `RESOURCES` line every 10 s while busy. Ask a user reporting a slow check
or update to enable it before capturing a log bundle.

Extraction one-liner (PowerShell):

```powershell
Select-String -Path "$env:APPDATA\Foxy\logs\foxy*.log" -Pattern 'SOL op=' |
  ForEach-Object {
    $row = @{}
    foreach ($kv in ($_.Line -split ' ' | Where-Object { $_ -match '=' })) {
      $k, $v = $kv -split '=', 2; $row[$k] = $v
    }
    [pscustomobject]$row
  } | Format-Table op, actual_s, work_bytes, actual_bps, sol_raw, metric_kind, light_src
```

## Baselines

Definitions and how to measure; values and status are in the measurements
file. Each accepted baseline needs an opaque id, date, machine, storage,
origin and protocol, algorithm, access pattern, cache preparation,
concurrency, burst policy, timer boundary, sample count and distribution,
tool and version, and validity conditions. Re-measure on any relevant
environment change, not only hardware or ISP.

| # | Baseline | How to measure |
| --- | --- | --- |
| B1 | Network body-byte throughput against the same origin and protocol | `foxy-testkit calibrate --lanes network`: 2 MiB range requests over the origin's 16 largest files, at one connection and at Foxy's 96-request budget for `--seconds`; interval-correct 500 ms samples, `sustained_bps` is the median plateau interval (first 2 s dropped), `peak_window_bps` the best interval, `per_connection_bps` the single-connection plateau |
| B2 / B3 | Disk read / write | `calibrate --lanes disk`: one `--disk-mib` file beside the case's repository path; `durable_write_bps` (sequential write through `sync_all`), `unbuffered_read_bps` / `unbuffered_read_parallel_bps` (sequential read with the page cache bypassed, so the device answers; one reader, then one per core over disjoint blocks), `warm_read_bps` / `warm_read_parallel_bps` (buffered re-read of the just-written pages). The faster of the two is the read bound for a hash pass: an NVMe gains from parallel readers, a rotational disk loses to the seeks. Mixed and random lanes are still open. A hash pass is not a disk baseline; the app's write p95 is not an independent ceiling |
| B4 | Latency | `calibrate --lanes latency`: TCP `connect_s`, a `fresh_request_s` (new connection, full `repo.json` body) and a `reused_request_s` (kept-alive connection), medians of ten. One origin's RTT does not describe every repository, so `calibrate --lanes hosts --hosts <url,...>` records the same three figures per host for any URL, HTTPS included (the handshake lands in the fresh request), keyed by host under `lanes.hosts` for a multi-host startup graph |
| B5 | Metadata | `calibrate --lanes metadata`: the repository tree enumerated the way the quick scan walks an addon (`read_dir`, file type, metadata per entry), `first_pass_entries_per_s` and `warm_entries_per_s`; the better of the two is the bound for a warm `quick_scan` row (`entries / rate` over `actual_s`), the first pass for a cold or evicted one. A separate complete clean-scan latency reference is still open |
| B6 | Hash | `calibrate --lanes hash`: compute-only BLAKE3 and MD5 over a 256 MiB in-memory buffer, one thread and all cores. The matched read-and-hash term is the B2 read lane; the row's ratio uses the slower of the two |
| B7 | Concurrency | The B1 lane records the aggregate curve at 1, 8, 24, 48 and `--connections` requests (`curve`), so a per-connection ceiling is read as a curve, never as one constant; `foxy-testkit origin bench` for request latency |
| B8 | App memory and startup | Empty `--config-dir` launch by renderer backend and process state, with distribution |
| DB | Turso workload | `calibrate --lanes db`: 200k `subfiles`-shaped rows with the unique index, 256 per transaction at `synchronous=NORMAL`, one writer, on the case volume (`insert_rows_per_s`, `update_rows_per_s`, `delete_rows_per_s`, checkpoint). Current action counters mix statement kinds, so they do not receive a calibrated ratio until per-kind work is emitted |
| UI | Frame and input latency | No calibration lane; measured beside work instead. A case operation with `ui_probe_ms` polls the agent probe (`agent-gui fps`, which keeps the app repainting) at that cadence and records `summary.ui_probe`: the worst frame interval (`frame_ms_max`) and worst p95 the app reported from its last 240 frames, the lowest smoothed fps, and the probe's own round trip (process spawn included, an upper bound on input latency). Fixed view, window size, renderer and display per row |

`foxy-testkit calibrate --case <case.json>` writes the lanes to
`testkit/ledger/calibration.json` (git-ignored, one entry per lane with an
opaque dated id, the environment fingerprint and the case it was measured
against). Every later run selects the lanes that match its machine, origin,
storage class and volume, keeps them on the row as `references`, and derives
`sol_calibrated` for `download` (B1), `hash` (min of B2 read lane and matching
B6 BLAKE3 or MD5, over `hash_total_s`; iteration-zero rows without explicit
eviction remain unrated), `startup_probe` (B4 fresh request) and the
no-change `sync_action` (B4 fresh plus reused requests), `quick_scan` (B5
entries). DB lanes remain attached as evidence but unrated while action work
mixes statement kinds. A baseline records
the reference ids it was accepted under; recalibrating retires it
(`reference-changed:<lane>`) rather than comparing ratios taken against two
different references.

A desktop-local benchmark measures this environment. Keep the NVMe and
rotational lanes; add remote or removable storage only with a described
access model, and never classify every removable device as a 110 MB/s disk.

## Collection and comparison rules

1. Freeze a case fingerprint: selected scope, payload and manifest hashes,
   files/parts/bytes, mutation seed and profile, initial local state,
   terminal correctness expectation. A case JSON hash alone cannot detect a
   changed remote repository, so every row also records the origin's
   published `checksum` (`origin_checksum`) and a baseline accepted under
   another one is refused with `origin-changed`.
2. Match machine, storage class, origin and protocol, connection state, cache
   preparation, build profile, harness, diagnostics mode (`diagnostics` is
   `profile` when the case ran with `FOXY_PROFILE`, else `none`), model
   version and timer boundaries; record engine, schema, gate and pool when
   relevant. The test kit persists this profile and the cited reference ids
   with every accepted baseline and refuses a comparison across it.
3. Cache state is preparation evidence, not an iteration number: an operation
   after `evict-cache` is the `evicted` lane whatever the repetition; report
   unknown when a cold state cannot be established.
4. Keep unprofiled performance gates apart from diagnostic profiling (saved
   benchmarks run with extended diagnostics on); measure instrumentation
   overhead with paired same-build runs.
5. At least five comparable successful repetitions per condition, more for
   noisy HDD and network cases and small effects; alternate candidate and
   baseline order when practical. Failures and timeouts stay in the
   reliability result; they are never filtered into a success rate.
6. Report sample count, median, spread and absolute difference. A fixed
   percent tolerance is a practical gate, not statistical proof; a p95 from
   three runs is the maximum.
7. Freeze the best reference before comparing a candidate: keep both the
   fastest valid run and the best reproducible run-set median, and use the
   median for the primary best-measured comparison. Never select the
   candidate's own fastest batch as its physical light.
8. Store outcome and oracle result with every row. Missing counters are
   unknown, never implicit zero; failed, partial, incompatible or unverified
   runs cannot become accepted baselines.

Baseline acceptance keeps the clean-worktree guard. Old records remain
readable and replayable. Each ledger row carries a derived schema version.
Replay reports changes across a schema boundary as intentional migrations and
still treats any change within the current schema as a failing difference.
The app and testkit derive aggregate service time, interval coverage, makespan,
work, reference and outcome fields through the shared `foxy-sol` crate.

## Maintenance

- **Baseline expiry.** An accepted baseline carries the run profile
  (harness, build kind, database mode, write gate, pool policy, storage
  class, diagnostics mode), the environment fingerprint (CPU, OS family and
  major version, memory bucket, origin host), the origin's published
  checksum and the calibration lane ids its ratios cite. `foxy-testkit
  compare` refuses to read a baseline across any of them
  (`profile-mismatch`, `environment-changed`, `origin-changed`,
  `reference-changed`, `rebaseline-required`) instead of quietly comparing
  against other hardware, another payload or another reference. A reference
  that does not exist for a lane is reported as `no lane`, never filled in
  from an unrelated one.
- **Recalibration.** Rerun `foxy-testkit calibrate --case <case>` after a
  hardware, OS, network path or origin change, and at least when a flagship
  lane's `sol_calibrated` drifts without a code change (the reference moved,
  not Foxy). Recalibrating retires the baselines that cited the old lane;
  re-accept them on a clean revision.
- **Flagship recheck.** Any change that can touch download, sync or startup
  paths reruns the O1 and O6 lanes (`perf-redownload-small-ssd`: the force
  redownload and the clean recheck after it) and the O8 lane before it lands:
  `foxy-testkit suite --filter "perf-redownload-small-ssd,perf-startup-arma3-live" --no-build`,
  read with the correctness counters (`files_updated`, `downloaded_bytes`,
  oracle result, `pipeline_outcome`) and the memory guardrails
  (`memory.peak_private_bytes`, `retained_private_bytes`) on the same rows.
- **Curated table.** The current block of the measurements file is generated
  with `foxy-testkit measurements` from the latest valid run per case and its
  accepted baseline; rows are pasted, not typed, and old rows move to the
  archive below it with a status.
- **Cards and routing.** An instrumentation or workflow change updates the
  operation card, the log-line table above and the `AGENTS.md` routing in the
  same change.

## Presentation

Operation and SoL come first in every human-facing view, then the
best-measured comparison, then actual and reference times, then evidence
status; date, hardware, version, ids, work counts and resource breakdown
below. On narrow layouts stack Operation, SoL with its kind, then versus best
as the first three lines. `n/a`, partial coverage, failed outcomes and model
violations stay visible; the headline never selects whichever sub-operation
happens to have a numeric ratio, and a partially rated operation declines a
whole-action percentage. Resource rows (network, disk, CPU, metadata,
persistence) are explanations, not percentages to sum; add a timeline when
overlap matters. The kind is text beside the percentage, readable without
colour or a tooltip; expanded details and comparison controls are keyboard
reachable with visible focus.

## Saved benchmarks

Enabling the "Benchmarks" application setting also switches on extended
diagnostics logging and locks it on (`SettingsViewState::set_benchmarks_enabled`).
Every recheck, quick check, integrity check, update, force redownload and
per-addon download the user starts then ends with a "Save benchmark" prompt.
A saved benchmark is a folder `games/<space>/benchmarks/<id>/` holding
`benchmark.json` (the `core::benchmarks::BenchmarkRecord`: kind, repository,
outcome, build, machine, metrics, `PIPELINE SUMMARY` stage rows, the owned
`SOL` lines of the frame, 1 Hz samples) and `benchmark.log` (the startup block
plus the action's time frame). The `benchmarks` table only indexes those
folders and is rebuilt from them on load, so a database wipe keeps benchmarks
unless "Also delete saved benchmarks" is ticked. The Benchmarks settings tab
lists, filters, compares and exports them (ZIP with the record, `summary.txt`
led by operation, SoL kind and best-measured comparison, the log slice and
chart PNGs). When a benchmark feeds the measurements file, take its `SOL`
rows from `benchmark.json`.

## Logging requirements for new code

- Every new operation that moves bytes, walks directories or fans out
  requests emits one `SOL op=<name>` line at info level via
  `utils::speed_of_light::sol_line`, with enough keys to recompute its ratio
  from the log alone (work, elapsed, the light when knowable, `outcome`, and
  an `op_id` when the operation has one).
- The grammar is append-only; keep per-operation counters in typed Rust data
  before rendering text; do not build a generic tracing framework for this.
- Keep debug-level detail debug; the info-level SOL line is the contract.
- Sanitize URLs and paths per the logging rules; SOL lines are not exempt.
- Update the operation card and the routing in `AGENTS.md` in the same change
  as an instrumentation or workflow contract.
