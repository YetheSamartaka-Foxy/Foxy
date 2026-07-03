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
| B1 | Network downlink `R_net` (bytes/s) | Speedtest/iperf3, or `peak_1s_bps` from a large unthrottled download run | 115,099,474 bytes/s (109.8 MiB/s), from 2026-06-13 `peak_1s_bps` |
| B2 | Disk sequential read `R_disk_r` | `winsat disk -seq -read -drive C`, or max `throughput` among `Hash profile auto benchmark sample:` lines | _fill in_ |
| B3 | Disk sequential write `R_disk_w` | `winsat disk -seq -write -drive C`, or `disk: ... p95` from `-- DOWNLOAD REPORT --` | _fill in_ |
| B4 | RTT to repo server `RTT` | `ping <repo-host>`, or debug `Fetched response body for .../repo.json (... download=...)` - for a tiny payload, download ≈ RTT | _fill in_ (ICMP to `a3.tfrod.cz` timed out on 2026-06-13; use debug fetch timing) |
| B5 | Quick-scan stat rate (entries/s) | `addons_per_s` from `SOL op=quick_scan` on a clean, warm-cache run - record best ever as the light | 2,462 addons/s, from 2026-06-13 best clean scan; re-record after persistent-cache fix |
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
   at the end of a run → queue drain starvation; check fair-share range cap
   growth (`metrics.rs::current_per_file_range_cap`).

**Levers**: per-file range count and global range budget, range size vs RTT
(E7), write coalescing, TLS connection reuse, limiter ramp parameters.

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

### O8 - Startup to first sync verdict

- **Work**: first frame (UI) + per-repo remote probe + quick scan (O4/O6 per repo).
- **Light** (E5): first frame is render-bound (~tens of ms); the sync verdict
  light is `max` over repos of O6 light (probes run concurrently) - startup
  must not serialize per-repo work.

**Cross-check (A8):** startup logs carry `Startup remote checksum probe ...`
lines and the per-repo pipeline summaries; wall time from `Logger initialized`
to the last repo's clean verdict, compared against `max(O6 light)`. The sync
convention requires startup work after first paint - any frame stall during
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
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.072 s | 1,324.4 addons/s | 2,462 addons/s (B5 self) | 0.538 | addon hash + DB load | TFR Main startup quick scan |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.167 s | 245.6 addons/s | self_baseline | na | missing-file diff | TFR_40K before download: 41 addons updated, 1,515 files missing |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.158 s | 260.2 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.154 s | 265.5 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.153 s | 268.8 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | download | 22.56 GiB | 222.671 s | 103.73 MiB/s | 112.92 MiB/s (peak_1s) | 0.919 | network path | 1,515 full downloads, 1,515 files, no retries |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | download pipeline | 22.56 GiB | 247.54 s | 93.31 MiB/s | self_baseline | na | post-download tail | download 240.68 s, hash_finalize 17.75 s, DB writes 14.84 s |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | hash finalize tail | 22.56 GiB | 15.15 s | 1,524.6 MiB/s | self_baseline | na | rollup or persistence tail | all 1,515 files incrementally hashed during download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | download DB checkpoint | 3,583 rows | 1.79 s | 2,002 rows/s | self_baseline | na | SQLite progress persistence | 36 batches, avg_batch=49.6ms |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 41 addons | 0.040 s | 1,019.4 addons/s | 2,462 addons/s (B5 self) | 0.414 | addon hash + DB load | TFR_40K clean after download |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 41 addons | 0.042 s | 972.4 addons/s | 2,462 addons/s (B5 self) | 0.395 | addon hash + DB load | TFR_40K remote skip verification |
| 2026-07-03 | 1.0.0 | 9950X3D desktop | no-change remote skip | repo.json + foxy_addons | 0.21 s | 1 clean verdict | self_baseline | na | RTT + quick verify | TFR_40K clean skip after download |
| | | | | | | | | | | |

Workflow rules:

1. **Measure before optimizing.** A perf PR must cite log lines (or this
   table) for before/after; "feels faster" doesn't merge.
2. **Fix the largest `1/sol` first.** Sort by headroom × frequency of the
   operation, not by what is fun to optimize.
3. **Never regress the flagships silently.** O1 (download throughput) and
   O6 (no-change latency) ratios may only drop with a written rationale here.
4. **Re-baseline on hardware/ISP/server changes** - old lights are lies.
5. **Alert thresholds**: investigate `sol < 0.85` for limiter-capped downloads,
   `< 0.6` for mixed-resource ops (hash), and any `self_baseline` rate below
   half its recorded best.

## Logging requirements for new code

- Every new operation that moves bytes, walks directories, or fans out
  requests must emit one `SOL op=<name>` line at info level via
  `utils::speed_of_light::sol_line` with enough keys to recompute its ratio
  from the log alone (work, elapsed, and the light when one is knowable).
- SOL grammar is append-only: add keys, never rename/remove them.
- Keep debug-level detail (per-second samples, per-request timings) debug;
  the info-level SOL line is the contract this document depends on.
- Sanitize URLs/paths per existing logging rules; SOL lines are not exempt.
