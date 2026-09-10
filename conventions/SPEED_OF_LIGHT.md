# Foxy Speed-of-Light Performance Convention

This document applies Jensen Huang's "speed of light" (SoL) engineering method
to Foxy: for every crucial operation, define the fastest physically possible
execution time (the "light"), measure the actual time from app logs, and track
the ratio between them. We do not compare against last release or against
competitors - we compare against physics, and the ratio tells us exactly how
much headroom remains and which resource is the bottleneck.

> "What's the absolute fastest this could be done if nothing stood in the way
> but the laws of physics?" - the benchmark NVIDIA judges itself against.

Sources:
- [Beyond Performance: Jensen Huang's "Speed of Light" Engineering Secret](https://www.youtube.com/shorts/XtpBPktj3uA)
- [Achieve Light Speed Like Nvidia](https://howardyu.substack.com/p/achieve-light-speed-like-nvidia-welcome)
- [Speed of Light Management](https://www.game-changer.net/2025/06/17/speed-of-light-management-why-most-companies-are-designed-to-fail-slowly/)

The method, adapted to Foxy:

1. Decompose the app into its crucial operations (O1–O8 below).
2. For each, write the light equation from first principles: bytes moved,
   round trips required, stat calls required - nothing else is allowed to
   count as "necessary work".
3. Instrument the code so a single greppable log line per operation carries
   the measured work and elapsed time (`SOL op=...` lines).
4. Compute the SoL ratio, find the bottleneck, fix the largest gap, repeat.

---

## Universal equations

These are implemented in `src/core/utils/speed_of_light.rs` (E1, E2) and
used by the cross-check algorithms below. `W` = work, `R` = rate, `T` = time.

| # | Equation | Meaning |
| --- | --- | --- |
| E1 | `T_ideal = W / R_light` | Fastest possible time for work `W` at the limiting rate. |
| E2 | `sol = T_ideal / T_actual = R_actual / R_light` | SoL ratio, clamped to `[0, 1]`. `1.0` = running at the speed of light. |
| E3 | `H = 1 / sol` | Headroom factor: "this operation can be `H`× faster before physics objects." |
| E4 | `T_ideal = max_r(W_r / R_r)` | Multi-resource (roofline) rule: when resources overlap (network + disk + CPU), the ideal time is set by the single slowest resource; `bottleneck = argmax_r`. |
| E5 | `T_ideal_chain = Σ_i T_ideal(stage_i)` for serial stages; `max_i` for overlappable stages | Pipeline rule. Everything in `T_actual − T_ideal_chain` is orchestration overhead, not work. |
| E6 | `T_ideal = D × RTT + W / R_net` | Latency-bound request chains: `D` = depth of *dependent* (sequential) round trips. Requests at the same depth are free to run in parallel. |
| E7 | `C_min = R_target × RTT / chunk_bytes` | Concurrency needed to saturate a link (bandwidth-delay product / Little's law). Below `C_min` parallel ranges, the link physically cannot be filled. |
| E8 | `T = N_miss × C_miss + N_hit × C_hit` | Cache law (quick scan): cost is dominated by misses; a "fast" path with a broken cache key is a slow path. |
| E9 | `R_agg = min(R_link, C × R_conn)` | Per-connection law: when a server shapes each connection to `R_conn`, aggregate is bought with concurrency `C` and nothing else, until the path ceiling `R_link` binds. Tuning a single stream is wasted work. |
| E10 | `T_tail ≈ chunk_bytes / R_conn` | Tail law: once the work queue is empty the link is carried by whatever chunks are still in flight, and the last one runs alone at one connection's rate. The largest chunk size, not the scheduler, bounds the tail. |

### E0 - Unit rules (read first, errors here invalidate every ratio)

| Log text | Actual unit | Conversion |
| --- | --- | --- |
| `... MB/s` (download report, hash metrics, samples) | MiB/s | `bytes / 1024² / s` |
| `... Mb/s` (`Download avg speed over last 30s`) | decimal megabits/s | `bytes / 125 000 / s` |
| Download limiter setting (`Mbps`) | decimal megabits/s | `cap_bytes_per_sec = mbps × 125 000` |
| `SOL` lines (`work_bytes`, `*_bps`) | raw bytes, bytes/s | none - use these for math |
| ISP plan "500 Mbit" | decimal megabits/s | `× 125 000 → bytes/s` |

---

## The SOL log line

Every crucial operation emits one info-level line built by
`utils::speed_of_light::sol_line`. Grammar (stable, append-only - never rename
or remove keys; parsers depend on it):

```text
SOL op=<name> actual_s=<secs> [work_bytes=<n> actual_bps=<n>]
    [light_bps=<n> ideal_s=<secs>] sol=<0.000–1.000|na>
    light_src=<limiter_cap|peak_1s|self_baseline> [key=value ...]
```

- `light_src=limiter_cap` - the user's bandwidth cap is the light (exact ceiling).
- `light_src=peak_1s` - the best 1-second sample of the same run is the light
  (demonstrated capacity of the whole path: server + network + disk).
- `light_src=self_baseline` + `sol=na` - no absolute light is computable
  in-app; trend the rate against the best previously recorded run (the Huang
  fallback: best demonstrated performance is the light until physics says
  otherwise).

Currently emitted lines:

| Line | Where | Extra keys |
| --- | --- | --- |
| `SOL op=download` | end of every download run (`download_files/orchestrator.rs`) | `files`, `peak_1s_bps`, `delta_savings_percent` |
| `SOL op=hash` | every part-hash run (`calculate_hashes/scheduling.rs`) | `label`, `files`, `parts`, `compute_s`, `wait_s` |
| `SOL op=quick_scan` | every quick scan, clean or dirty (`quick_scan/diff.rs`) | `repo`, `addons_total`, `addons_hashed`, `cache_hits_shared`, `cache_hits_persistent`, `deep_scan_files`, `addons_per_s`, `outcome` |
| `SOL op=startup` | once per launch, when the last repository has a sync verdict (`ui/app/runtime/startup_sync.rs`) | `repos`, `quick_scan_repos`, `eligible`, `prevalidated`, `remote_changed`, `rechecks`, `first_frame_s`, `dispatch_s`, `eligibility_s`, `verdict_s` |
| `SOL op=startup_probe` | the startup `repo.json` probe stage (`quick_scan/worker.rs`) | `repos`, `answered`, `changed` |
| `SOL op=app_update_check` | every app update check (`tasks/app_update/spawn.rs`) | `op_id`, `mode`, `outcome` |

Logs live in `%APPDATA%\Foxy\logs\foxy_rCURRENT.log` (rotated files alongside).
Default file level is info. Debug-only cross-check lines (1 Hz `Download
sample:`, `Quick scan timings:`, `Fetched response body ...`) require starting
Foxy with `RUST_LOG="warn,Foxy=debug,foxy=debug"`.

Extraction one-liner (PowerShell):

```powershell
Select-String -Path "$env:APPDATA\Foxy\logs\foxy*.log" -Pattern 'SOL op=' |
  ForEach-Object {
    $row = @{}
    foreach ($kv in ($_.Line -split ' ' | Where-Object { $_ -match '=' })) {
      $k, $v = $kv -split '=', 2; $row[$k] = $v
    }
    [pscustomobject]$row
  } | Format-Table op, actual_s, work_bytes, actual_bps, sol, light_src
```

---

## Device baselines (the "lights")

Fill this table once per machine (and re-measure after hardware/ISP changes).
Ratios computed against someone else's baseline are meaningless.

| # | Baseline | How to measure | Value (this machine) |
| --- | --- | --- | --- |
| B1 | Network downlink `R_net` (bytes/s) | Speedtest/iperf3, or `peak_1s_bps` from a large unthrottled download run | 118,387,677 bytes/s (112.9 MiB/s), from 2026-09-10 `peak_1s_bps` against the reference origin; six consecutive runs land inside 118.07-118.39 MB/s, so this is the path ceiling, not a lucky sample |
| B2 | Disk sequential read `R_disk_r` | `winsat disk -seq -read -drive C`, or max `throughput` among `Hash profile auto benchmark sample:` lines | _fill in_ |
| B3 | Disk sequential write `R_disk_w` | `winsat disk -seq -write -drive C`, or `disk: ... p95` from `-- DOWNLOAD REPORT --` | _fill in_ |
| B4 | RTT to repo server `RTT` | `ping <repo-host>`, or debug `Fetched response body for .../repo.json (... download=...)` - for a tiny payload, download ≈ RTT | **40 ms** to the reference origin (2026-09-10, dependency-free `TcpStream` connect, median of 7: 39.2-50.0 ms). A `repo.json` GET costs 83 ms on a fresh connection (2 x RTT: handshake + request) and 42 ms on a kept-alive one (1 x RTT); the body is 1 062 bytes, so payload is not a term. ICMP to this host times out - measure with a TCP connect, not `ping` |
| B5 | Quick-scan stat rate (entries/s) | `addons_per_s` from `SOL op=quick_scan` on a clean, warm-cache run - record best ever as the light | 2,462 addons/s, from 2026-06-13 best clean scan; re-record after persistent-cache fix |
| B7 | Per-connection rate `R_conn` (bytes/s) | Fetch one large range over a single connection and divide; repeat at several concurrency levels to confirm it is flat. Needed for E7, E9 and E10 | ~1.6 MB/s at C=1, ~1.15 at C=48, ~0.88 at C=96 against the reference origin (2026-09-09). Server-imposed, not a client property |
| B6 | Hash compute rate `R_hash` | `work_bytes / compute_s` from `SOL op=hash` (pure aggregated hash time, I/O excluded); BLAKE3 is multi-GB/s multicore, MD5 ≈ 0.5–0.7 GB/s per stream | ≈1,234,800,000 bytes/s (1.15 GiB/s), warm 2026-06-13 hash run; re-measure cold |

Reference physics, for sanity checks: NVMe read 2–7 GB/s, SATA SSD ≈ 550 MB/s,
HDD ≈ 80–200 MB/s; NTFS warm-cache stat ≈ 10⁴–10⁵ entries/s, cold ≈ 10³;
1 Gbps link = 125 MB/s = 119.2 MiB/s.

---

## Crucial operations

For each operation: the work definition, the light equation, the cross-check
algorithm (exact log lines to read), and the levers that close the gap.

### O1 - Full-file download (flagship throughput path)

- **Work** `W` = wire bytes actually transferred (`work_bytes` in `SOL op=download`,
  equals `bytes_transferred` in `Download stage completed:`).
- **Light** (E4): `R_light = min(R_net, R_server_egress, R_disk_w)`. With a
  user limiter set, the limiter cap is the light by definition.
- **Computed in-app**: `sol` in `SOL op=download`. With no limiter, light is
  `peak_1s` - the ratio then measures *consistency* (did we hold our own peak
  the whole run?), while `peak_1s_bps / B1` separately measures whether the
  path (server included) can fill the pipe at all.

**Cross-check algorithm (A1):**

1. Read `SOL op=download`: `sol`, `actual_bps`, `peak_1s_bps`, `light_src`.
2. `sol < 0.85` with `light_src=limiter_cap` → we waste a capped link; look at
   `-- DOWNLOAD REPORT --`:
   - `network: ... permit_wait=` high → concurrency/fair-share starvation;
   - `ranges network: p50_latency/p95_latency` high → too-small ranges for the
     RTT, check E7: ranges per file must satisfy `C_min = R_light × RTT / range_bytes`;
   - `disk: ... avg` near B3 → disk-bound, not network-bound (E4 bottleneck flip);
   - `db: checkpoint ... total=` significant vs `total: elapsed=` → persistence
     stealing run time.
3. `peak_1s_bps ≪ B1 × ~0.9` → bottleneck is upstream (server egress or
   per-connection limits); more local tuning cannot help (that *is* the light).
4. Tail behavior: `Download avg speed over last 30s` rolling samples dropping
   at the end of a run → the run is tail-bound. Read `max_range` from
   `-- DOWNLOAD REPORT --` and apply E10: the tail cannot be shorter than
   `max_range / R_conn` (B7) no matter what the scheduler does.

**Split the deficit before touching anything.** Integrate the per-second
telemetry (`summary-N-<op>.json` in a test kit run, or the debug `Download
sample:` lines) against B1 and separate it into three buckets. They have
different owners and only one of them is ours:

| bucket | signature | owner |
| --- | --- | --- |
| ramp | first ~3 samples climbing to the ceiling | the path. Congestion control, identical with 96 or 192 pre-established connections. Not client-addressable |
| plateau | samples between ramp and tail | ours, but it has been at the ceiling since 2026-09-10; a dip here is a real regression |
| tail | last samples decaying to zero | ours, and bounded by E10 |

A run whose plateau sits on B1 has no throughput problem, whatever its `sol`
says. Chasing `sol` without this split leads to tuning the steady state, which
on this path is already at the speed of light.

**Levers**, in the order they actually pay:

1. **Chunk ceiling** (E10). The single largest lever once the plateau is at the
   ceiling. `RANGE_CHUNK_TARGET` 8 MiB -> 2 MiB took the tail deficit from
   1.1-2.1 s to 0.25-0.54 s.
2. **Concurrency to the last byte** (E9). Keep the global range budget busy
   until the run ends: a wave-aligned chunk grid, a per-file ceiling equal to
   the global budget, largest-file-first ordering, and few enough concurrent
   large files that the budget is not oversubscribed.
3. **Range size vs RTT** (E7), write coalescing, TLS connection reuse, limiter
   ramp parameters.

**Not a lever**: raising `MAX_ACTIVE_RANGE_REQUESTS` past the point where
`C × R_conn` reaches `R_link` (E9). Against this origin 96 connections already
reach 118 MB/s and 192 plateau at the same rate.

**Measuring the light directly.** When it matters whether the client or the
path is at fault, measure the path with something that shares no code with
Foxy: a dependency-free `TcpStream` range reader at several concurrency levels
and chunk sizes. That is how B1 and B7 above were established, and how "chunk
size is free at high concurrency" (117.5-117.6 MB/s at 1, 2 and 8 MiB chunks
with 96 connections) was settled before the chunk ceiling was lowered. The
build recipe is in `testkit/ledger/perf-redownload-small-ssd.notes.md`.

**Stage timers are not download time.** `download_stage_ms` includes joining
the background progress-checkpoint, mod-progress and telemetry-sampler tasks.
Any of those that sleeps on a timer and only then reads its stop flag quantizes
the whole stage to its own period, which reads as download cost and is not.
Before 2026-09-10 this contributed a fixed 5 s and 1 s quantum. New background
tasks in the download path must wait on an interruptible primitive
(`timeout(delay, notify.notified())` or a `watch` change), never a bare
`sleep`.

### O2 - Delta patch (download less than the file)

- **Work**: `W_net` = insert bytes fetched, `W_out` = full output file written,
  `W_copy` = bytes copied from the old local file.
- **Light** (E4/E5): `T_ideal = max(W_net / R_net, (W_copy / R_disk_r) + (W_out / R_disk_w))`.
- **Efficiency identity**: `savings = 1 − W_net / W_full` - reported directly as
  `delta_savings_percent` in `Download stage completed:` and `SOL op=download`.

**Cross-check (A2):** `Parallel delta blob download: ... bytes= elapsed= speed=`
gives the network leg; compare with `R_net`. The apply leg is disk-bound; if a
patch run's `avg_speed` (in output bytes per second) exceeds the link rate,
delta is winning - the effective speedup over full download is
`W_full / W_net` capped by the disk term. A patch plan is only valid when
planned bytes < full bytes (sync convention invariant 8); fallbacks are logged
with reasons - count `fallback` occurrences per run; every fallback pays both
the planning cost and the full download.

### O3 - Tree hash verification (disk + CPU)

- **Work** `W` = `hashed_bytes` (in `SOL op=hash` as `work_bytes`, and in
  `Hash part run metrics:`).
- **Light** (E4): `R_light = min(R_disk_r, R_hash_effective)`. BLAKE3 multicore
  is normally faster than any disk → expect disk-bound: `sol ≈ (W/T) / B2`.
  Legacy Swifty MD5 part checksums are sequential per stream → per-file light
  is `min(R_disk_r, ~0.6 GB/s)`; only file-level parallelism recovers it.

**Cross-check (A3):**

1. `R_actual = work_bytes / actual_s` from `SOL op=hash`.
2. Light = max `throughput` among `Hash profile auto benchmark sample:` lines
   in the same log (in-run measured device light), else B2/B6 roofline.
3. `sol = R_actual / light`.
4. Decompose the gap with the same line's extras and `Hash part run metrics:`:
   - schedule loss = `wait_s / (compute_s + wait_s)` (semaphore starvation);
   - `metadata_sum`, `layout_sum` → non-hash overhead;
   - `Hash timing distribution: ... >=1s= >=5s=` and `Hash slow file:` →
     stragglers (E5: the run ends when the slowest file ends);
   - `missing_files > 0` → the run hashed less than expected; do not compare
     against full-repo expectations.

**Levers**: file/part concurrency limits (`hash_scheduler_limits`), I/O profile
(auto benchmark already picks one - trust it), straggler splitting, avoiding
re-hash of clean files (scoped trees).

### O4 - Quick scan (the "be fast when nothing changed" path)

- **Work**: directory enumeration + one stat per entry (addon folder
  fingerprints are metadata-only: name, size, mtime, ctime, readonly - see
  `utils/content_hash.rs::calculate_addon_folder_content_hash`). Deep-scan
  fallback adds ≤ 128 KiB sampled read per suspicious file (16 KiB × 8 slots,
  `quick_scan/content_hash.rs`).
- **Light** (E8): `T_ideal = N_entries / R_stat + Σ_suspicious min(size, 128 KiB) / R_disk_r`,
  with `N_entries` counted only for cache-missed addons. A perfect cached clean
  scan approaches `T_ideal ≈ N_addons × C_hit` - microseconds per addon.

**Cross-check (A4):** from `SOL op=quick_scan`:

1. Clean runs (`outcome=clean`): track `addons_per_s`; the best value ever
   recorded on this machine is the light (B5). Alert when below `0.5 × B5`.
2. Cache health: `addons_hashed / addons_total` should be ~0 on consecutive
   clean runs. Rising `addons_hashed` with `cache_hits_persistent=0` means the
   persistent cache key went volatile (the exact failure the existing
   `persistent addon hash cache produced zero hits` warning flags) - E8 says
   this silently multiplies cost by `C_miss / C_hit`.
3. `deep_scan_files > 0` on a clean-disk run → false suspicion; find which
   check (size/missing/content) triggered it in `Quick scan summary:`.
4. Phase decomposition (debug): `Quick scan timings:` shows `db_load`,
   `addon_hash`, `file_fallback`, `tree_verify` - only `addon_hash` is
   physics; the rest is overhead to drive toward zero.

**Forbidden by the sync convention** (these would change the light equation -
treat as bugs): part-range reads in quick scan; file-row loads for clean
addons; full-tree loads on the no-change path.

### O5 - Remote metadata refresh (RTT-bound)

- **Work**: the dependent fetch chain. Depth `D`: `repo.json` (1) → changed
  addons' manifests (2, parallel within level) → part lists (3, when needed).
- **Light** (E6): `T_ideal = D × RTT + Σ bytes / R_net`. For metadata, bytes
  are small - RTT dominates; the light for "N changed addons" is ≈ `2–3 × RTT`,
  *not* `N × RTT` (level-parallel fetches).

**Cross-check (A5):** `PIPELINE SUMMARY` table row `remote_repository` (and
`Recheck stats: mods=, files=, parts=, elapsed_total=`):
`sol = (D × B4 + payload/B1) / stage_elapsed`. With debug enabled, each
`Fetched response body for ... (N bytes ... download=...)` line gives per-fetch
reality. The sync convention requires fetch count ∝ changed addons - a refresh
that fetches every manifest when one addon changed is a scope bug, visible as
fetch-line count ≫ changed-addon count.

### O6 - No-change sync (flagship latency path)

The most frequent user-visible operation: startup/recheck when nothing changed.

- **Work**: one `repo.json` GET + one checksum compare + O(1) DB reads.
- **Light** (E6): `T_ideal ≈ RTT + repo_json_bytes / R_net + ~1 ms`.

**Cross-check (A6):** `Pipeline summary: op=... outcome=... elapsed=` for
clean-outcome runs: `sol = (B4 + payload/B1) / elapsed`. Practical target:
clean recheck within `2 × RTT + 50 ms`. Anything beyond that is orchestration
(DB churn, tree loads, content-hash refresh) that the sync convention says must
not run on this path - the `PIPELINE SUMMARY` stage rows name the offender
directly.

### O7 - SQLite persistence

- **Work**: rows upserted in a run (`db:` line of `-- DOWNLOAD REPORT --`,
  `-- DATABASE METRICS SUMMARY --`, `SQLite sync metrics:`).
- **Light**: batched transaction rate - `T_ideal ≈ N_txn × t_fsync + rows / R_row`
  where `t_fsync` ≈ 1–10 ms (device-dependent) and `R_row` ≈ 10⁵–10⁶ rows/s for
  prepared batched inserts. The dominant term is transaction count, not rows.

**Cross-check (A7):** `db: checkpoint_batches= rows= ... avg_batch=` →
`rows_per_batch` low + many batches = paying `t_fsync` per few rows.
`SQLite sync metrics: lock_retries= total_backoff_ms=` - backoff is pure
overhead (no physics in a retry); a healthy run shows ~0. `db_write_time_ms`
vs operation `elapsed_ms` gives the persistence share of the run.

`db_write_time_ms`, and the per-category `txn_ms` it sums, are gated transaction
windows, not row cost. Turso has one internal writer, so above a write gate of 1
the waiters block inside `conn.execute` without surfacing `Busy` and that queue
time is charged here rather than to `permit_wait_ms`: one frozen refresh reports
~300 ms at gate 1 and ~2 180 ms at gate 8 for identical work. Both lines carry
`write_gate=`; never compare either number across gate sizes, and take gate 1 as
the uncontended reference. Before 2026-09-09 the per-category field was named
`total_ms`; the measurement is unchanged, only the name.

Never compare a WAL `db_write_time_ms` to an MVCC one except through
`foxy-testkit compare` on the same case, gate, harness, and build. Shipping
journal is WAL (`FOXY_DB_MVCC` unset). On Turso 0.7.2 / Foxy 1.2.0 / gate 4
(2026-09-09, NVMe): MVCC is worse on persistence and not faster on wall clock.
Small redownload (217 files) write time 125 ms WAL vs 210 ms MVCC, elapsed
~51 s either way. Big redownload (3738 files, ~92 GB) write time
11.6 s WAL vs 37.4 s MVCC (~3x), elapsed 801 s vs 805 s (network-bound; O1
drowns O7). Recheck after that download is ~0.45 s on both. Engine benches
where MVCC looks better (flat write-gate curve, 16 concurrent writers) do
not show up in these sync ratios. Full matrix and how to re-run:
`conventions/CORE_CONVENTIONS.md` (WAL vs MVCC).

### O8 - Startup to first sync verdict

The second flagship latency path after O6: every launch pays it, and the user is
looking at the window while it runs.

- **Work**: reach a painted window, then answer "is anything out of date?" for
  every configured repository.
- **Light** (E5, two serial stages that cannot overlap):
  `T_ideal = T_paint + max_repos(2 x RTT + repo_json_bytes / R_net)`.
  - `T_paint` is the platform's window + graphics-device creation. It is not
    Foxy code and is measured, not assumed: 520-540 ms on this machine
    (eframe/wgpu enumerating 8 adapters across Vulkan, DX12 and GL).
  - The verdict term is O6's light per repository, and `max` rather than `sum`
    because probes run at the same depth (E6). It is `2 x RTT`, not `1 x RTT`:
    a cold repository connection pays a TCP handshake before the GET.
- **Computed in-app**: `SOL op=startup` carries the whole timeline, so the ratio
  is recomputable from one line.

**Cross-check (A8):**

1. Read `SOL op=startup`: `first_frame_s`, `dispatch_s`, `eligibility_s`,
   `verdict_s`, `actual_s`.
2. `first_frame_s` minus the renderer floor is Foxy's own pre-paint cost. It
   must be near zero. Anything else there is work that does not gate the first
   frame and belongs on a thread.
3. `verdict_s` against `2 x RTT` (B4) is the verdict ratio.
   `SOL op=startup_probe` isolates the network leg; `answered < repos` means the
   probe budget elapsed and those repositories' remote freshness is unknown.
4. `Quick scan preflight timings:` localizes a slow eligibility stage to a
   specific repository and a specific query.

**Levers**, in the order they paid on 2026-09-10:

1. **Nothing blocking on the paint path.** The startup system summary
   (sysinfo + drive + GPU + antivirus enumeration, 466 ms) and the editor
   mission scan (267 ms, 3.7 s cold) both ran inside `Foxy::new` and the first
   frame. Neither gates the window. Moving both to threads took first frame from
   1 259-1 342 ms to 530-561 ms.
2. **Start the network probe before the window exists.** The probe is two round
   trips of pure latency and touches no UI. Running it after first paint stacks
   it on top of renderer initialization; starting it in `Foxy::new` hides it
   underneath, and the verdict lands almost as soon as the frame does.
   `verdict_s` 128-142 ms -> 49-51 ms.
3. **No aggregate where a boolean is wanted.** The preflight decided two
   booleans with a `COUNT`/`SUM` over every part row in the repository - 1.03 s
   on a 141k-part repository, and it gated all eleven repositories' verdicts
   because the plan waits for the slowest. `LIMIT 1` probes, skipped entirely
   when a higher level already decided the answer, give the same verdicts.
4. **Bound the probe stage, not just each request.** A probe that outlives its
   own timeout would otherwise hold every other repository indefinitely. The
   stage budget sits above the per-request timeout, never below it: a shorter
   stage would trade a rare hang for routinely abandoning slow-but-live
   servers, and remote freshness lost is worse than a slow launch.

**Not a lever**: `T_paint`. Restricting the graphics backend would cut adapter
enumeration, but that trades a startup fraction for a compatibility risk on
other people's machines, and it is not Foxy code.

**Invariant**: startup work must not block first paint. A frame stall during
probes is a regression regardless of ratios.

---

## Tracking table

Append one row per measured run that matters (release validation, perf work,
or any "this feels slow" report). Date, app version, machine label, then the
numbers straight from the logs.

| Date | Version | Machine | Op | Work | T_actual | R_actual | Light (src) | sol | Bottleneck | Action |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| _2026-06-13_ | _1.0.0_ | _example_ | download | 8.2 GiB | 612 s | 14.3 MiB/s | 15.0 MiB/s (limiter) | 0.95 | limiter | none - at light |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.210 s | 456.1 addons/s | 2,462 addons/s (B5 self) | 0.185 | no cache hits, DB load | persistent cache empty; track cache-key fix |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.071 s | 1,356.0 addons/s | 2,462 addons/s (B5 self) | 0.551 | addon hash + DB load | cache_hits_persistent=0 |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.074 s | 1,305.9 addons/s | 2,462 addons/s (B5 self) | 0.530 | addon hash + DB load | cache_hits_persistent=0 |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.069 s | 1,399.2 addons/s | 2,462 addons/s (B5 self) | 0.568 | addon hash + DB load | best in this log, still under B5 |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.072 s | 1,324.2 addons/s | 2,462 addons/s (B5 self) | 0.538 | addon hash + DB load | cache_hits_persistent=0 |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.071 s | 1,353.6 addons/s | 2,462 addons/s (B5 self) | 0.550 | addon hash + DB load | cache_hits_persistent=0 |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | hash benchmark Conservative | 737.6 MiB | 0.362 s | 2,037.9 MiB/s | 6,003.6 MiB/s (same-run best) | 0.339 | profile limits | Balanced wins on this run |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | hash benchmark Balanced | 737.6 MiB | 0.123 s | 6,003.6 MiB/s | 6,003.6 MiB/s (same-run best) | 1.000 | at same-run light | selected profile |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | hash benchmark Aggressive | 737.6 MiB | 0.126 s | 5,859.4 MiB/s | 6,003.6 MiB/s (same-run best) | 0.976 | near same-run light | no action |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | hash selected remaining | 20.60 GiB | 6.571 s | 3,209.5 MiB/s | 6,003.6 MiB/s (benchmark best) | 0.535 | file mix, stragglers | 1086 files, max file 2.410 s |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | repository DB purge | 440,492 rows | 144.54 s | 3,047 rows/s | self_baseline | na | SQLite delete | zero-row part delete took 78.59 s; subfile delete took 59.59 s |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | deferred part insert | 433,016 rows | 107.30 s | 4,035 rows/s | self_baseline | na | SQLite insert with live indexes | 1,692 batches of 256; biggest sync cost |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | remote refresh rebuild | 3,738 files | 125.73 s | 29.7 files/s | self_baseline | na | DB persistence | 110.84 s DB write time; tree_hash_bootstrap 118.29 s |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | no-change remote skip | repo.json + foxy_addons | 0.27 s | 1 clean verdict | self_baseline | na | RTT + quick verify | repeat clean skip after rebuild |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.072 s | 1,324.4 addons/s | 2,462 addons/s (B5 self) | 0.538 | addon hash + DB load | large-repo startup quick scan |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.167 s | 245.6 addons/s | self_baseline | na | missing-file diff | 41-addon repo before download: 41 addons updated, 1,515 files missing |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.158 s | 260.2 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.154 s | 265.5 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.153 s | 268.8 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | download | 22.56 GiB | 222.671 s | 103.73 MiB/s | 112.92 MiB/s (peak_1s) | 0.919 | network path | 1,515 full downloads, 1,515 files, no retries |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | download pipeline | 22.56 GiB | 247.54 s | 93.31 MiB/s | self_baseline | na | post-download tail | download 240.68 s, hash_finalize 17.75 s, DB writes 14.84 s |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | hash finalize tail | 22.56 GiB | 15.15 s | 1,524.6 MiB/s | self_baseline | na | rollup or persistence tail | all 1,515 files incrementally hashed during download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | download DB checkpoint | 3,583 rows | 1.79 s | 2,002 rows/s | self_baseline | na | SQLite progress persistence | 36 batches, avg_batch=49.6ms |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 41 addons | 0.040 s | 1,019.4 addons/s | 2,462 addons/s (B5 self) | 0.414 | addon hash + DB load | 41-addon repo clean after download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 41 addons | 0.042 s | 972.4 addons/s | 2,462 addons/s (B5 self) | 0.395 | addon hash + DB load | 41-addon repo remote skip verification |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | no-change remote skip | repo.json + foxy_addons | 0.21 s | 1 clean verdict | self_baseline | na | RTT + quick verify | 41-addon repo clean skip after download |
| 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 WAL | 9950X3D NVMe | force-redownload | 217 files | 51.17 s | 86.4 MB/s | peak_1s | 0.79 | network | perf-redownload-small-ssd warm; db_write 125 ms |
| 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 MVCC | 9950X3D NVMe | force-redownload | 217 files | 51.35 s | 86.3 MB/s | peak_1s | 0.78 | network | same case; db_write 210 ms (+68%); elapsed no-difference |
| 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 WAL | 9950X3D NVMe | force-redownload | 92.2 GB / 3738 files | 801 s | 116.1 MB/s | peak_1s | 0.97 | network | perf-redownload-big-ssd cold; db_write 11.6 s |
| 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 MVCC | 9950X3D NVMe | force-redownload | 92.2 GB / 3738 files | 805 s | 115.4 MB/s | peak_1s | 0.97 | network | same case; db_write 37.4 s (~3x); elapsed no-difference |
| 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 WAL | 9950X3D NVMe | recheck | large repo after redownload | 0.44 s | 1 clean | self_baseline | na | RTT + quick verify | big-ssd cold recheck |
| 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 MVCC | 9950X3D NVMe | recheck | large repo after redownload | 0.45 s | 1 clean | self_baseline | na | RTT + quick verify | same; not faster than WAL |
| 2026-09-09 | 1.2.0 pre-tail-work | 9950X3D NVMe | download | 4.33 GB / 217 files | 45.11 s | 96.0 MB/s | 117.8 MB/s (peak_1s) | 0.815 | tail | perf-redownload-small-ssd warm. Plateau already at the ceiling; 2.1 s ramp + 10-12 s tail |
| 2026-09-10 | 1.2.0 post-tail-work | 9950X3D NVMe | download | 4.33 GB / 217 files | 39.79 s | 108.8 MB/s | 118.4 MB/s (peak_1s) | 0.922 | ramp | same case, same bytes/files. Tail deficit 0.25-0.54 s; run is now ramp-bound |
| 2026-09-09 | 1.2.0 pre-tail-work | 9950X3D HDD | download | 4.33 GB / 217 files | 65.12 s | 66.5 MB/s | peak_1s | 0.563 | tail | perf-redownload-small-hdd warm median 67.6 s |
| 2026-09-10 | 1.2.0 post-tail-work | 9950X3D HDD | download | 4.33 GB / 217 files | 44.45 s | 97.4 MB/s | peak_1s | 0.825 | tail + disk | same case; warm median 48.4 s. Spinning media is noisy, read medians |
| 2026-09-10 | probe (no Foxy code) | 9950X3D NVMe | path ceiling | 96 conns, 1/2/8 MiB chunks | 20 s each | 117.5-117.6 MB/s | 118.4 MB/s | ~0.99 | path | chunk size is free at full concurrency; 192 conns plateau identically |
| 2026-09-10 | 1.2.0 pre-startup-work | 9950X3D NVMe | startup | 11 repos, 10 probed | 1.500 s | 1 verdict | 0.62 s (paint 0.54 + 2xRTT 0.08) | 0.41 | pre-paint blocking work | perf-startup-arma3-live warm median. first_frame 1.30 s, verdict 0.135 s |
| 2026-09-10 | 1.2.0 post-startup-work | 9950X3D NVMe | startup | 11 repos, 10 probed | 0.660 s | 1 verdict | 0.59 s (paint 0.54 + 2xRTT 0.08 overlapped) | 0.89 | renderer init | same case. first_frame 0.54 s, verdict 0.050 s; probe now overlaps paint |
| 2026-09-10 | 1.2.0 post-startup-work | 9950X3D NVMe | startup_probe | 10 repo.json | 0.092 s | 10 probes | 0.080 s (2 x B4) | 0.87 | RTT | all probes at one depth; 12 ms client overhead |
| 2026-09-10 | probe (no Foxy code) | 9950X3D NVMe | repo.json GET | 1 062 B | 0.083 s | fresh conn | 0.080 s (2 x B4) | 0.96 | RTT | keep-alive repeat 0.042 s = 1 x RTT; payload is not a term |
| 2026-09-10 | 1.2.0 | 9950X3D NVMe | app_update_check | 1 manifest | 0.085 s | 1 check | 0.080 s (2 x B4) | 0.94 | RTT | `SOL op=app_update_check`, outcome=up_to_date |
| | | | | | | | | | | |

Workflow rules:

1. **Measure before optimizing.** A perf PR must cite log lines (or this
   table) for before/after; "feels faster" doesn't merge.
2. **Fix the largest `1/sol` first.** Sort by headroom × frequency of the
   operation, not by what is fun to optimize.
3. **Never regress the flagships silently.** O1 (download throughput), O6
   (no-change latency) and O8 (startup) ratios may only drop with a written
   rationale here.
4. **Re-baseline on hardware/ISP/server changes** - old lights are lies.
5. **Alert thresholds**: investigate `sol < 0.85` for limiter-capped downloads,
   `< 0.6` for mixed-resource ops (hash), and any `self_baseline` rate below
   half its recorded best.
6. **Split before you tune (O1).** For downloads, decompose the deficit into
   ramp, plateau and tail (A1) before proposing a change. A plateau at B1 with
   a bad `sol` is a tail problem, and the fix is E10, not scheduler tuning.

## Logging requirements for new code

- Every new operation that moves bytes, walks directories, or fans out
  requests must emit one `SOL op=<name>` line at info level via
  `utils::speed_of_light::sol_line` with enough keys to recompute its ratio
  from the log alone (work, elapsed, and the light when one is knowable).
- SOL grammar is append-only: add keys, never rename/remove them.
- Keep debug-level detail (per-second samples, per-request timings) debug;
  the info-level SOL line is the contract this document depends on.
- Sanitize URLs/paths per existing logging rules; SOL lines are not exempt.
