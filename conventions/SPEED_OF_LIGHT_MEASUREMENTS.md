# Speed-of-Light Measurements

Machine-specific evidence for `conventions/SPEED_OF_LIGHT.md`: the current
status of every operation, the baseline registry, dated experiments and the
archived tracking rows. The convention owns the definitions, equations,
operation cards and logging contract; this file owns numbers.

Every row here has a date, a source and a status:

| Status | Meaning |
| --- | --- |
| `accepted` | Measured under the current model on a known checkout; usable as a reference for the same case, machine and lane |
| `legacy` | Recorded under the pre-2026-09-16 model (clamped `sol`, nominal-tick sampler, last-batch hash view); numbers preserved verbatim, ratios not comparable with new ones without re-derivation |
| `superseded` | A later accepted row on the same case replaces it as the reference |
| `disputed` | The ratio's arithmetic or reference is under question; the raw timings stand, the percentage does not |

Rates quoted from log text before 2026-09-16 keep their original unit text.
Where a row says `MB/s` from a download report line it is MiB/s
(`1024 * 1024`); rows typed by hand in the September tables used decimal
MB/s. Recover exact values from `work_bytes` and `actual_s` when it matters.

## 1. Status by operation (as of 2026-09-17, checkout `98e45d7`)

`n/a` means the required reference or measurement is missing, not zero
performance. "Reported" ratios keep their original interpretation; the
comparison column says what the number actually compares.

| Operation | SoL and kind | Comparison with best case | Latest documented result | What it establishes |
| --- | --- | --- | --- | --- |
| O1 full download, small NVMe | 95%, calibrated estimate against the evening B1 (`network-20260916-3bc06636`, 114.6 MB/s); same-run peak consistency 92% | Stage 39.5-39.8 s: ramp 3-4 s (244-252 MiB short of the plateau), plateau 34-36 s, tail 1.6-1.9 s (59-70 MiB short); the ramp is the path's (identical at 96 and 192 connections in the calibration lane) | 217 files, 4.33 GB, peak window 118.3 MB/s, oracle pass, Sept 16 `20260916T205020Z` | Steady transfer is at the path plateau; what is left is the path ramp and the last wave, neither a client lever at this origin |
| O1 many tiny files, NVMe | n/a, loopback origin (per-file cost, not throughput) | 822 s shipped, 22.6-32.6 s after the append-only rollback journal (25-36x) | 32,016 x 8 KiB, 293-357k persisted rows in 10.9-14.1 s of write windows, Sept 16 `20260916T203538Z` | The rollback manifest rewrite was quadratic in the file count; the remaining cost is DB rows and 1,000 hash batches |
| O1 one large file, NVMe | Reported peak consistency only (loopback) | 2 GiB in 2.06-2.12 s, 1.0 GB/s, tail 0.03-0.74 s | Sept 16 `20260916T202401Z` | The range scheduler alone reaches the NVMe durable write rate on a loopback path |
| O1 limiter above the path | 42% against a 2000 Mbps cap (modeled bound); 90-91% against B1 | Same 40.6-40.8 s stage as the unlimited lane | Sept 16 `20260916T173645Z` | A cap above the path leaves the B1 ratio as the physical reading; the cap-relative number is policy headroom, not waste |
| O1 full download, small HDD | Reported 63-81%, peak consistency | 45.2-58.2 s overlaps the Sept 10 range of 44-58 s | Sept 16, restored network limits | The 125-137 s rotational-profile regression was reversed; disk and cache effects still need isolation |
| O1 full download, large NVMe | Reported 97%, peak consistency | No matched independent whole-action lower bound | 92.2 GB / 3,738 files in 801 s, WAL, Sept 9 | Historical; not a current-build regression gate without replay and matching metadata |
| O2 delta, 40 files, NVMe | 52-53% calibrated against B1 for the 424 MB it moves; peak window 113.9-116.1 MB/s | 16.2-17.4 s on the shipped build; 6.9-7.2 s now (shared request budget and chunked ops, section 3) | 424 MB transferred in 6.9-7.2 s, 40 patched files, oracle pass, Sept 16 `20260916T152819Z` | The blob fetch was the dominant term; it now fills the link like a full download and the rest of the stage is apply and promotion |
| O2 delta, 40 files, HDD | 12-13% calibrated against B1 (the network is not the bound here) | Fresh payload: 28.4 s on the first cycle, 31.6 s on the second; the 32.3-32.6 s read on the worn payload was payload wear, not code (section 3) | Sept 16 `20260916T164216Z`, oracle pass, 40 patched | Apply-bound (`patch_applies=2` on rotational media); each mutate-and-patch cycle costs seconds on this disk, so HDD delta rows are read per cycle |
| O2 delta, four files | 2-3% calibrated against B1 (5.2 MB; latency- and apply-bound) | 1.66-1.87 s NVMe / 2.75-2.85 s HDD evicted this build versus 1.71-1.91 s / 2.82-3.02 s shipped; full-download A/B for the same initial state still missing | 5.2 MB transferred, 97% byte savings, Sept 16 | Byte savings are established; elapsed speedup is not established by the savings percentage; the request budget change is neutral here |
| O2 delta locality, 8 files | Resource comparison, same B1 reference | Adjacent run: 48 requests, 0 gap bytes, 65 MB fetched, 216 MB copied; scattered: 74 requests, 7 KB gap, 90 MB fetched, 335 MB copied; both 3.3-4.5 s | Sept 16 `20260916T175048Z`, oracle pass | Locality changes the resource cost, not the elapsed, at this size |
| O2 apply-time fallback | n/a, correctness lane | One of four plans fails copy verification against a silently changed source (mtime preserved) and falls back; 29 MB moved | Sept 16 `20260916T203927Z`, `patch_fallbacks=1`, oracle pass | The fallback path is now exercised by a kit lane, not only by unit tests |
| O3 evicted first-check hash, HDD | 81% calibrated against matched B2+B6 | 39.68 s median for 4.33 GB across two explicitly evicted samples | Sept 16 `20260916T184102Z` | Payload hashed once; the earlier 13.8 s benchmark figure was a transcription error |
| O3 first-check hash, NVMe | 67% calibrated on the warm row; iteration-zero row is deliberately unrated because no eviction established a device-cold lane | 4.33 GB hashed once; 1.34 s warm operation row and 1.50 s first pass | Sept 16 `20260916T184731Z` | Disjoint trial groups remove redundant work; held-out profile-choice validation remains open |
| O3 MD5 integrity recheck, NVMe | 135-140% against the B6 MD5 all-core lane (2.3 GB/s over one buffer) | 400 x 1 MiB hashed at 4.3-4.5 GB/s with `algorithm=md5` | Sept 16 `20260916T181455Z` | The single-buffer MD5 lane is a floor for per-file parallel MD5; kept as such |
| O4 outdated, untouched quick check | 3-6% calibrated against B5 (255 entries at the volume's metadata rate over the 21 ms scan) | The scan is not enumeration-bound: entries cost under 1 ms, the rest is cache validation and DB reads, as the model predicted | App `sync_action` 0.023 s (runner elapsed 0.45 s is driver round trips), zero hash bytes, Sept 16 | The repeated expensive recheck was removed; a B5 ratio this low says the remaining cost is the correctness work, not the walk |
| O4 clean quick scan | 21-28% calibrated against B5 on the clean recheck rows; 38-54% on the startup sweep (3,176-4,136 entries) | B5's 2,462 addons/s June figure is `legacy`; the entries-per-second lane replaces it | Sept 16 `20260916T163824Z` | Enumeration is a third to a half of a clean scan; the balance is DB and cache validation |
| O5 metadata refresh | n/a in-app; `SOL op=remote_refresh` now records the branch and work | `graph_unchanged` branch 0.125-0.15 s (two index requests, no manifests); `rebuilt` branch on the force redownload 0.28-0.29 s for 6 mods / 217 files | Sept 16, `perf-tfr-scifi-stale-check-ssd` and `perf-redownload-small-ssd` | The Sept 10 CLI 433k-part figure (1.50-1.53 s) stays the large-refresh reference; parse and persist need their own model before a ratio |
| O6 no-change recheck | 93-94% calibrated against B4 (one fresh plus one reused request, 122 ms) | `sync_action` 0.124-0.139 s app makespan for the clean recheck after the redownload; runner elapsed 0.48-0.51 s | Sept 16 `20260916T152819Z` | The terminal record exists; the clean path is two index requests and nothing else |
| O7 database persistence | n/a, no calibrated Turso bound; `SOL op=db_persist` now records the windows per action | Force redownload: 604 write calls in 16 categories, 125-126 ms of gated windows inside a 7.5 s action (delta lane); WAL 125 ms vs MVCC 210 ms small, 11.6 s vs 37.4 s large (Sept 9) | Sept 16 | WAL remains the measured choice; summed transaction windows are not critical-path wall time |
| O8 startup to settled verdict | n/a for the action; the probe reads 78-86% calibrated against B4 | 566-605 ms total, first frame 434-472 ms, verdict 54-58 ms, 11/11 probes answered | Sept 16 `20260916T153043Z` | The dependency model (paint and probe overlap) still has no bound of its own; the probe is one fresh request away from its reference |
| O8 startup, adverse origins | n/a; probe spread recorded | First frame 378-423 ms; fast branch answers at 86-108 ms, the 3 s host at 3.01-3.02 s, verdict 2.97-2.98 s, offline host `unknown` | Sept 16 `20260916T205421Z`, `outcome=settled` | One slow host delays the settled verdict, never first paint; an offline host is an explicit unknown |
| Cancellation lanes | n/a; quiescence and resumed work | Patch stage: 728-730 ms to quiescent, resume applies the kept plans (438 MB, not 2.44 GB); hash stage, cold HDD: 297-317 ms (26 s shipped); force redownload: 552-561 ms, resume moves everything again by rollback policy | Sept 16 `20260916T204935Z`, `20260916T204815Z`, `20260916T174716Z` | Cancel joins the hash worker, reverts, and clears the reverted baselines; a cancelled patch keeps its plan |
| Responsiveness under work | n/a; frame probe beside the operation | Worst frame 14-20 ms and p95 6.7-7.2 ms under the 4.33 GB download, 13-14 ms under the integrity recheck | Sept 16 `20260916T173824Z` | Throughput is not bought by blocking the UI thread |
| Startup probe / app update check | Probe 78-86% calibrated (B4 fresh request 81 ms); app update check reported 94% modeled | 81 ms reference vs 94-104 ms probe actual | Sept 16 | Plausible for that fresh HTTP path; not universal across HTTPS, reuse and hosts |
| M1 memory, loaded startup | n/a, advisory footprint (resource trade policy) | Empty app 221-239 MB private commit is a baseline, not a proven minimum for loaded state | 456-473 MB peak / 352-369 MB retained, Sept 16 (455-460 / 355-360 on Sept 10 with another seeded configuration) | Reported, not gated: memory is spent for speed on purpose; only unbounded growth would reopen this |
| M1 repeated UI walk | n/a, growth/retention comparison, advisory | About +40 MB retained after the walk, the same saturation as Sept 10 | 404-417 MB peak / 400-412 MB retained, Sept 16 | Consistent with a saturating cache; not proof that longer-run leaks are absent |

## 1a. Current accepted baselines

Generated with `foxy-testkit measurements` from the five accepted version 2
baselines at checkout `98e45d7956326cf902cd31342b4f9b153fe3a425`.

| Date | Case | Operation | SoL (kind) | Lane | Elapsed | Baseline | Samples | Work | Outcome | Build | Run |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2026-09-17 | `perf-redownload-small-ssd` | force-redownload (O1 full-file download) | 95% (calibrated, network B1) | ssd warm | 41.09 s | 41.09 s median of 5 [40.53-41.26] | 5 | 217 files, 4.33 GB, 4.33 GB hashed | completed,completed,completed,completed,completed | 98e45d7 | `20260917T115728Z-3a091b88` |
| 2026-09-17 | `perf-redownload-small-ssd` | recheck (O6 no-change sync) | 89% (calibrated, no-change B4) | ssd warm | 0.46 s | 0.46 s median of 5 [0.44-0.58] | 5 | n/a | ok | 98e45d7 | `20260917T115728Z-3a091b88` |
| 2026-09-17 | `perf-tfr-scifi-recheck-hdd` | recheck-integrity (O3 tree hash verification) | 45% (calibrated, hash B2+B6) | hdd warm | 1.12 s | 1.12 s median of 7 [1.10-1.14] | 7 | 4.33 GB hashed | ok | 98e45d7 | `20260917T114946Z-18571f30` |
| 2026-09-17 | `perf-tfr-scifi-stale-check-hdd` | download (O2 delta patch) | 12% (calibrated, network B1) | hdd warm | 31.54 s | 31.54 s median of 7 [30.73-32.63] | 7 | 40 files, 0.42 GB | completed,completed,completed,completed,completed,completed,completed | 98e45d7 | `20260917T155052Z-18ca3678` |
| 2026-09-17 | `perf-tfr-scifi-stale-check-hdd` | quick-check-stale (O4 quick scan) | 5% (calibrated, metadata B5) | hdd warm | 0.45 s | 0.45 s median of 7 [0.45-0.56] | 7 | n/a | ok | 98e45d7 | `20260917T155052Z-18ca3678` |
| 2026-09-17 | `perf-tfr-scifi-stale-check-hdd` | quick-check-verify@evicted (O4 quick scan) | 68% (calibrated, hash B2+B6) | hdd evicted | 32.24 s | 32.24 s median of 7 [31.56-32.78] | 7 | 2.44 GB hashed | ok | 98e45d7 | `20260917T155052Z-18ca3678` |
| 2026-09-17 | `perf-tfr-scifi-stale-check-hdd` | remote-refresh (O5 remote metadata refresh) | 5% (calibrated, metadata B5) | hdd warm | 0.64 s | 0.64 s median of 7 [0.60-0.69] | 7 | n/a | ok | 98e45d7 | `20260917T155052Z-18ca3678` |
| 2026-09-17 | `perf-startup-arma3-live` | startup (O8 startup) | 84% (calibrated, probe B4) | ssd warm | 3.03 s | 3.03 s median of 5 [3.01-3.11] | 5 | 11 repos | ok | 98e45d7 | `20260917T115028Z-14f33d74` |
| 2026-09-17 | `perf-tfr-scifi-delta-patch-ssd` | download (O2 delta patch) | 3% (calibrated, network B1) | ssd warm | 2.16 s | 2.16 s median of 5 [2.16-2.36] | 5 | 4 files, 0.01 GB, 0.21 GB hashed | completed,completed,completed,completed,completed | 98e45d7 | `20260917T115110Z-2a30d7a0` |

## 1b. Fresh implementation verification archive

The sequential 2026-09-16 risk-weighted suite passed all ten selected cases:
`perf-redownload-small-ssd`, `perf-startup-arma3-live`, both first-check
storage lanes, delta locality and apply fallback, cancel/resume, sibling
identity, adverse origins and game-space switching. The generated current rows
include O1 run `20260916T183343Z`, startup run `20260916T183722Z`, cancellation
run `20260916T183815Z`, locality run `20260916T184014Z`, HDD hash run
`20260916T184102Z`, and NVMe hash run `20260916T184731Z`. The HDD and NVMe
first-check cases warned that the runner did not observe the short DB-wipe busy
marker, so their wipe timing is not used as evidence; payload/oracle gates and
the measured hash lanes passed.

## 1c. Earlier generated lanes (superseded where sections 1a or 1b name a newer run)

Latest valid run per case, one line per operation lane; `@cold` is iteration
zero without a warmup pass, `@evicted` follows an `evict-cache` step. The
baseline column says `legacy (rebaseline)` where the accepted file predates
profile validation and must be re-accepted on a clean revision.

| Date | Case | Operation | SoL (kind) | Lane | Elapsed | Baseline | Samples | Work | Outcome | Build | Run |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2026-09-16 | `perf-memory-arma3-live` | download | 64% (calibrated, no-change B4) | ssd warm | 0.49 s | none | 2 | n/a | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-memory-arma3-live` | download@cold | 63% (calibrated, no-change B4) | ssd cold | 0.45 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-memory-arma3-live` | recheck | 68% (calibrated, no-change B4) | ssd warm | 0.46 s | none | 2 | n/a | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-memory-arma3-live` | recheck@cold | 69% (calibrated, no-change B4) | ssd cold | 0.45 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-memory-arma3-live` | startup | 80% (calibrated, probe B4) | ssd warm | 3.05 s | none | 2 | 11 repos | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-memory-arma3-live` | startup@cold | 84% (calibrated, probe B4) | ssd cold | 3.06 s | none | 1 | 11 repos | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-memory-arma3-live` | ui-walk | n/a | ssd warm | 1.01 s | none | 2 | n/a | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-memory-arma3-live` | ui-walk@cold | n/a | ssd cold | 1.03 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T133825Z-001d5d30` |
| 2026-09-16 | `perf-redownload-small-ssd-limited` | force-redownload | 41% (calibrated, network B1) | ssd warm | 90.47 s | none | 1 | 217 files, 4.33 GB, 4.33 GB hashed | completed | 6cbfc25-dirty | `20260916T163230Z-377aed9c` |
| 2026-09-16 | `perf-redownload-small-ssd-limited` | force-redownload@cold | 40% (calibrated, network B1) | ssd cold | 92.24 s | none | 1 | 217 files, 4.33 GB, 8.66 GB hashed | completed | 6cbfc25-dirty | `20260916T163230Z-377aed9c` |
| 2026-09-16 | `perf-redownload-small-ssd-limited` | recheck | 91% (calibrated, no-change B4) | ssd warm | 0.48 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163230Z-377aed9c` |
| 2026-09-16 | `perf-redownload-small-ssd-limited` | recheck@cold | 92% (calibrated, no-change B4) | ssd cold | 0.51 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163230Z-377aed9c` |
| 2026-09-16 | `perf-redownload-small-ssd-profiled` | force-redownload | 92% (calibrated, network B1) | ssd warm | 41.31 s | none | 1 | 217 files, 4.33 GB, 4.33 GB hashed | completed | 6cbfc25-dirty | `20260916T163549Z-0d23c94c` |
| 2026-09-16 | `perf-redownload-small-ssd-profiled` | force-redownload@cold | 90% (calibrated, network B1) | ssd cold | 41.88 s | none | 1 | 217 files, 4.33 GB, 8.66 GB hashed | completed | 6cbfc25-dirty | `20260916T163549Z-0d23c94c` |
| 2026-09-16 | `perf-redownload-small-ssd-profiled` | recheck | 89% (calibrated, no-change B4) | ssd warm | 0.55 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163549Z-0d23c94c` |
| 2026-09-16 | `perf-redownload-small-ssd-profiled` | recheck@cold | 93% (calibrated, no-change B4) | ssd cold | 0.46 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163549Z-0d23c94c` |
| 2026-09-16 | `perf-redownload-small-ssd` | force-redownload | 90% (calibrated, network B1) | ssd warm | 41.99 s | legacy (rebaseline) | 2 | 217 files, 4.33 GB, 4.33 GB hashed | completed,completed | 6cbfc25-dirty | `20260916T163824Z-04ff2d88` |
| 2026-09-16 | `perf-redownload-small-ssd` | force-redownload@cold | 90% (calibrated, network B1) | ssd cold | 41.97 s | no lane | 1 | 217 files, 4.33 GB, 8.66 GB hashed | completed | 6cbfc25-dirty | `20260916T163824Z-04ff2d88` |
| 2026-09-16 | `perf-redownload-small-ssd` | recheck | 89% (calibrated, no-change B4) | ssd warm | 0.51 s | legacy (rebaseline) | 2 | n/a | rebaseline-required | 6cbfc25-dirty | `20260916T163824Z-04ff2d88` |
| 2026-09-16 | `perf-redownload-small-ssd` | recheck@cold | 94% (calibrated, no-change B4) | ssd cold | 0.46 s | no lane | 1 | n/a | rebaseline-required | 6cbfc25-dirty | `20260916T163824Z-04ff2d88` |
| 2026-09-16 | `perf-space-switch-live` | startup | 72% (calibrated, probe B4) | ssd warm | 3.05 s | none | 1 | 11 repos | ok | 6cbfc25-dirty | `20260916T162951Z-3b90825c` |
| 2026-09-16 | `perf-space-switch-live` | startup@cold | 79% (calibrated, probe B4) | ssd cold | 3.06 s | none | 1 | 11 repos | ok | 6cbfc25-dirty | `20260916T162951Z-3b90825c` |
| 2026-09-16 | `perf-space-switch-live` | switch-to-arma3 | 78% (calibrated, probe B4) | ssd warm | 0.52 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T162951Z-3b90825c` |
| 2026-09-16 | `perf-space-switch-live` | switch-to-arma3@cold | 76% (calibrated, probe B4) | ssd cold | 0.52 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T162951Z-3b90825c` |
| 2026-09-16 | `perf-space-switch-live` | switch-to-reforger | n/a | ssd warm | 0.48 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T162951Z-3b90825c` |
| 2026-09-16 | `perf-space-switch-live` | switch-to-reforger@cold | n/a | ssd cold | 0.45 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T162951Z-3b90825c` |
| 2026-09-16 | `perf-startup-arma3-live` | startup | 74% (calibrated, probe B4) | ssd warm | 3.07 s | none | 2 | 11 repos | ok | 6cbfc25-dirty | `20260916T164052Z-06bbfb10` |
| 2026-09-16 | `perf-startup-arma3-live` | startup@cold | 73% (calibrated, probe B4) | ssd cold | 3.09 s | none | 1 | 11 repos | ok | 6cbfc25-dirty | `20260916T164052Z-06bbfb10` |
| 2026-09-16 | `perf-tfr-scifi-cancel-resume-ssd` | download-resume | 91% (calibrated, network B1) | ssd warm | 40.89 s | none | 1 | 217 files, 4.33 GB, 4.21 GB hashed | completed | 6cbfc25-dirty | `20260916T163019Z-167859f4` |
| 2026-09-16 | `perf-tfr-scifi-cancel-resume-ssd` | download-resume@cold | 90% (calibrated, network B1) | ssd cold | 41.76 s | none | 1 | 217 files, 4.33 GB, 4.22 GB hashed | completed | 6cbfc25-dirty | `20260916T163019Z-167859f4` |
| 2026-09-16 | `perf-tfr-scifi-cancel-resume-ssd` | force-redownload-cancelled | 67% (calibrated, network B1) | ssd warm | 8.87 s | none | 1 | 0.12 GB hashed | cancelled | 6cbfc25-dirty | `20260916T163019Z-167859f4` |
| 2026-09-16 | `perf-tfr-scifi-cancel-resume-ssd` | force-redownload-cancelled@cold | 70% (calibrated, network B1) | ssd cold | 8.99 s | none | 1 | 0.22 GB hashed | cancelled | 6cbfc25-dirty | `20260916T163019Z-167859f4` |
| 2026-09-16 | `perf-tfr-scifi-delta-patch-hdd` | download@evicted | 2% (calibrated, network B1) | hdd evicted | 7.22 s | none | 3 | 4 files, 0.01 GB, 0.21 GB hashed | completed,completed,completed | 6cbfc25-dirty | `20260916T133547Z-1a0a9b2c` |
| 2026-09-16 | `perf-tfr-scifi-delta-patch-ssd` | download | 2% (calibrated, network B1) | ssd warm | 2.27 s | none | 2 | 4 files, 0.01 GB, 0.21 GB hashed | completed,completed | 6cbfc25-dirty | `20260916T133519Z-0a251200` |
| 2026-09-16 | `perf-tfr-scifi-delta-patch-ssd` | download@cold | 3% (calibrated, network B1) | ssd cold | 2.17 s | none | 1 | 4 files, 0.01 GB, 0.21 GB hashed | completed | 6cbfc25-dirty | `20260916T133519Z-0a251200` |
| 2026-09-16 | `perf-tfr-scifi-first-check-ssd` | download | 57% (calibrated, network B1) | ssd warm | 6.87 s | none | 1 | 40 files, 0.42 GB | completed | 6cbfc25-dirty | `20260916T152630Z-3adc1740` |
| 2026-09-16 | `perf-tfr-scifi-first-check-ssd` | download@cold | 51% (calibrated, network B1) | ssd cold | 7.57 s | none | 1 | 40 files, 0.42 GB | completed | 6cbfc25-dirty | `20260916T152630Z-3adc1740` |
| 2026-09-16 | `perf-tfr-scifi-first-check-ssd` | remote-refresh | 80% (calibrated, hash B2+B6) | ssd warm | 1.19 s | none | 1 | 4.33 GB hashed | ok | 6cbfc25-dirty | `20260916T152630Z-3adc1740` |
| 2026-09-16 | `perf-tfr-scifi-first-check-ssd` | remote-refresh@cold | 198% (calibrated, hash B2+B6) | ssd cold | 1.16 s | none | 1 | 4.33 GB hashed | ok | 6cbfc25-dirty | `20260916T152630Z-3adc1740` |
| 2026-09-16 | `perf-tfr-scifi-first-check-ssd` | wipe-db | n/a | ssd warm | 30.58 s | none | 1 | 40 files, 0.42 GB | ok | 6cbfc25-dirty | `20260916T152630Z-3adc1740` |
| 2026-09-16 | `perf-tfr-scifi-first-check-ssd` | wipe-db@cold | n/a | ssd cold | 30.51 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T152630Z-3adc1740` |
| 2026-09-16 | `perf-tfr-scifi-patch-fallback-ssd` | download | 2% (calibrated, network B1) | ssd warm | 2.13 s | none | 1 | 5 files, 0.01 GB, 0.00 GB hashed | completed | 6cbfc25-dirty | `20260916T164148Z-07860bf8` |
| 2026-09-16 | `perf-tfr-scifi-patch-fallback-ssd` | download@cold | 2% (calibrated, network B1) | ssd cold | 2.24 s | none | 1 | 5 files, 0.01 GB, 0.00 GB hashed | completed | 6cbfc25-dirty | `20260916T164148Z-07860bf8` |
| 2026-09-16 | `perf-tfr-scifi-patch-fallback-ssd` | quick-check-plan | 30% (calibrated, hash B2+B6) | ssd warm | 0.48 s | none | 1 | 0.21 GB hashed | ok | 6cbfc25-dirty | `20260916T164148Z-07860bf8` |
| 2026-09-16 | `perf-tfr-scifi-patch-fallback-ssd` | quick-check-plan@cold | 74% (calibrated, hash B2+B6) | ssd cold | 0.47 s | none | 1 | 0.21 GB hashed | ok | 6cbfc25-dirty | `20260916T164148Z-07860bf8` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-hdd` | download | 12% (calibrated, network B1) | hdd warm | 31.97 s | none | 1 | 40 files, 0.42 GB | completed | 6cbfc25-dirty | `20260916T164216Z-381e63b4` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-hdd` | download@cold | 13% (calibrated, network B1) | hdd cold | 28.84 s | none | 1 | 40 files, 0.42 GB | completed | 6cbfc25-dirty | `20260916T164216Z-381e63b4` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-hdd` | quick-check-stale | 5% (calibrated, metadata B5) | hdd warm | 0.49 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T164216Z-381e63b4` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-hdd` | quick-check-stale@cold | 6% (calibrated, metadata B5) | hdd cold | 0.49 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T164216Z-381e63b4` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-hdd` | quick-check-verify@evicted | 67% (calibrated, hash B2+B6) | hdd evicted | 33.64 s | none | 2 | 2.44 GB hashed | ok | 6cbfc25-dirty | `20260916T164216Z-381e63b4` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-hdd` | remote-refresh | 6% (calibrated, metadata B5) | hdd warm | 0.64 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T164216Z-381e63b4` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-hdd` | remote-refresh@cold | 5% (calibrated, metadata B5) | hdd cold | 0.61 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T164216Z-381e63b4` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | download | 51% (calibrated, network B1) | ssd warm | 7.66 s | none | 1 | 40 files, 0.42 GB | completed | 6cbfc25-dirty | `20260916T163728Z-1d123960` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | download@cold | 52% (calibrated, network B1) | ssd cold | 7.46 s | none | 1 | 40 files, 0.42 GB | completed | 6cbfc25-dirty | `20260916T163728Z-1d123960` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | quick-check-stale | 3% (calibrated, metadata B5) | ssd warm | 0.46 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163728Z-1d123960` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | quick-check-stale@cold | 3% (calibrated, metadata B5) | ssd cold | 0.52 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163728Z-1d123960` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | quick-check-verify | 53% (calibrated, hash B2+B6) | ssd warm | 0.69 s | none | 1 | 2.44 GB hashed | ok | 6cbfc25-dirty | `20260916T163728Z-1d123960` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | quick-check-verify@cold | 140% (calibrated, hash B2+B6) | ssd cold | 0.65 s | none | 1 | 2.44 GB hashed | ok | 6cbfc25-dirty | `20260916T163728Z-1d123960` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | remote-refresh | 3% (calibrated, metadata B5) | ssd warm | 0.65 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163728Z-1d123960` |
| 2026-09-16 | `perf-tfr-scifi-stale-check-ssd` | remote-refresh@cold | 3% (calibrated, metadata B5) | ssd cold | 0.64 s | none | 1 | n/a | ok | 6cbfc25-dirty | `20260916T163728Z-1d123960` |

Flagship read of that table against the shipped build's rows of the same day
(`20260916T082712Z`), latest runs `20260916T152819Z` (O1, O6) and
`20260916T153043Z` (O8):

| Lane | Shipped build | This change | Verdict |
| --- | --- | --- | --- |
| O1 `force-redownload` warm download stage | 39.3-39.6 s, peak 118.1-118.4 MB/s, `sol` 0.92-0.93 | 39.6-40.1 s, peak 118.2-118.4 MB/s, `sol` 0.92-0.93, `sol_calibrated` 0.92-0.93 against B1, 217 files / 4.33 GB, oracle pass | unchanged (inside noise) |
| O1 memory guardrail (advisory) | peak 583-601 MB, retained 342-352 MB | peak 589-606 MB, retained 346-353 MB | unchanged |
| O6 `recheck` after the redownload, warm | 0.449-0.481 s runner elapsed | 0.479-0.508 s runner elapsed; app `sync_action` 0.124-0.139 s, `sol_calibrated` 0.93-0.94 against B4 | unchanged; the app makespan is now readable |
| O8 `startup`, app line | `20260910T102550Z`: 745-820 ms total, first frame 543-580 ms, verdict 130-159 ms | 566-605 ms total, first frame 434-472 ms, verdict 54-58 ms, 11/11 probes answered, probe `sol_calibrated` 0.78-0.86 | better than the last ledger run (different day, same case) |
| O8 memory guardrail (advisory) | `perf-memory-arma3-live` startup, Sept 10: peak 455-460 MB, retained 355-360 MB | peak 460-473 MB, retained 355-369 MB (`20260916T133825Z`) | +5-13 MB peak with a changed seeded configuration; advisory by the resource trade policy, not a verdict |

## 2. Baseline registry

Fill once per machine; re-measure after hardware, ISP, origin, driver or
engine changes. A ratio computed against another machine's baseline is not a
ratio. Definitions and measurement methods are in the convention (section
"Baselines"); this table holds the values.

| # | Baseline | Value (9950X3D desktop) | Date / provenance | Status |
| --- | --- | --- | --- | --- |
| B1 | Network body-byte throughput `R_net` | `network-20260916-3bc06636`: sustained 114,564,038 bytes/s (median 500 ms interval after a 2 s ramp, 96 connections, 2 MiB ranges, 20 s, 1,005 requests, 0 errors, 2.0 GB), peak window 117,293,029 bytes/s, average incl. ramp 99.8 MB/s; single connection 1,555,356 bytes/s sustained; curve 1: 1.6, 8: 13.0, 24: 38.6, 48: 76.2, 96: 114.6 MB/s (B7); ramp intervals 3, 8, 16, 26, 43, 65, 86, 106 MB/s per 500 ms, the same shape at 192 connections (114.8 MB/s sustained) | 2026-09-16 evening, `foxy-testkit calibrate --lanes network` against the sanitized reference origin | `accepted`; cited by `download.sol_calibrated`. The afternoon lane (`3a35e120`, 117.5 MB/s) and the 2026-09-10 `TcpStream` probe are `superseded`; the origin gives 2-3% less this evening, which is why the flagship reads 0.90-0.92 against it |
| B2 | Disk sequential read `R_disk_r` | NVMe test volume `disk-20260916-b8670f88`: unbuffered (page cache bypassed) 3,818,270,029 bytes/s one reader, 5,743,304,681 bytes/s one reader per core; warm re-read 9,111,784,727 / 13,357,091,086 bytes/s. HDD test volume `disk-20260916-d8f5366c`: unbuffered 139,264,702 bytes/s one reader, 85,940,625 bytes/s parallel (the seeks lose), warm re-read 9,223,357,921 / 13,080,375,012 bytes/s (page cache, not the platter) | 2026-09-16, `calibrate --lanes disk`, 1 GiB file, 8 MiB blocks, warm lane before the unbuffered one | `accepted`; cited by `hash.sol_calibrated` (the faster warm lane for warm rows, the faster unbuffered lane for evicted/cold). The earlier "about 110 MB/s" HDD inference from a hash pass and the `68410bf6` / `05726857` lanes are `superseded` |
| B3 | Disk sequential write `R_disk_w` | NVMe test volume 1,799,328,666 bytes/s; HDD test volume 150,396,363 bytes/s, both through `sync_all` (durable completion) | 2026-09-16, same lanes as B2 | `accepted` as a buffered-then-durable sequential figure; mixed and random lanes are still open |
| B4 | Latency to the reference origin | `latency-20260916-26ed4275`: TCP connect 38.9 ms, fresh `repo.json` request 80.9 ms (min 79.6 ms), kept-alive request 41.6 ms, medians of ten. Per host (`hosts` lane): the sanitized HTTP reference `hosts-20260916-8d57543a` measured 39.6 / 80.8 / 41.8 ms; the sanitized HTTPS reference `hosts-20260916-e893198e` measured 25.7 / 84.2 / 27.5 ms with the TLS handshake | 2026-09-16, `calibrate --lanes latency` and `--lanes hosts` | `accepted` for that origin over plain HTTP; cited by `startup_probe.sol_calibrated` and the no-change `sync_action.sol_calibrated`. Matches the 2026-09-10 figures (40 / 83 / 42 ms). The HTTPS host shows a fresh request costs the same 80-85 ms either way here: the handshake is hidden behind a shorter connect |
| B5 | Quick-scan metadata rate | NVMe test volume `metadata-20260916-ac7da5be`: 308,253 entries/s first pass, 289,427 warm; HDD test volume `metadata-20260916-7ac03780`: 180,362 / 171,368 entries/s (the tree was in the page cache) | 2026-09-16, `calibrate --lanes metadata` over the case repository tree | `accepted`; cited by `quick_scan.sol_calibrated` (`entries / rate` over `actual_s`). The 2026-06-13 "2,462 addons/s" figure is `superseded` (addons/s is not entries/s). A complete clean-scan latency reference is still open |
| B6 | Hash compute rate | `hash-20260916-0ba11d4e`: BLAKE3 5,725,572,610 bytes/s one thread, 40,492,888,433 bytes/s on 32 threads; MD5 117,437,603 bytes/s one thread, 2,309,155,970 bytes/s on 32 threads; 256 MiB in-memory buffer | 2026-09-16, `calibrate --lanes hash` | `accepted` as compute-only capacity; cited by `hash.sol_calibrated` together with B2. The 2026-06-13 `work_bytes / compute_s` figure and the `b9ea0eef` lane are `superseded` |
| B7 | Per-connection rate | `network-20260916-3bc06636` curve: 1.6 MB/s at C=1, 13.0 at 8, 38.6 at 24, 76.2 at 48, 114.6 at 96 and 114.8 at 192 against the reference origin (1.6, 1.6, 1.6, 1.6, 1.19, 0.60 MB/s per connection) | 2026-09-16 network lane | `accepted` as an aggregate curve for that origin; per-connection rate holds to 48 and the path saturates between 48 and 96; not one constant `R_conn`. The 2026-09-09 probe is `superseded` |
| B8 | Renderer memory floor | 221-239 MB private / 175-216 MB working set (wgpu, Vulkan, backend pinned, `MemoryHints::MemoryUsage`); 290 MB / 237 MB before pinning; 468 MB / 237 MB before both; 133 MB / 103 MB on glow; DX12 pinned 210 MB / 163 MB; GL pinned 215 MB / 172 MB | 2026-09-10, empty `--config-dir` | `accepted` as an empty-app baseline by backend; not a proven minimum for a loaded state |
| DB | Turso persistence workload reference | NVMe test volume `db-20260916-4948fffe`: 82,889 insert rows/s (200k `subfiles` rows, 256 per transaction), 39,161 keyed update rows/s (65,536 rows, 256 per transaction), 114,337 delete rows/s, checkpoint 7.8 ms; HDD test volume `db-20260916-cdb8dc4e`: 47,718 / 20,546 / 78,425 rows/s, checkpoint 51.9 ms; `synchronous=NORMAL`, one writer | 2026-09-16 evening, `calibrate --lanes db` | `accepted`; cited by `db_purge.sol_calibrated` (delete rate) and `db_persist.sol_calibrated` (update rate over the write windows). The afternoon lanes (`cc903364`, `df1a90cb`) are `superseded` |
| UI | Idle and loaded frame/input latency | not calibrated as a lane; measured beside work with `ui_probe_ms` (`perf-redownload-small-ssd-responsive`, `20260916T173824Z`): under the 4.33 GB download, worst frame 14-20 ms, p95 6.7-7.2 ms, smoothed fps never under 216; under the integrity recheck, worst frame 13-14 ms | 2026-09-16 | `best measured`; a frame stall under work would show up as `frame_ms_max` on the operation's own row |

Reference physics for sanity checks only: NVMe read 2-7 GB/s, SATA SSD about
550 MB/s, HDD 80-200 MB/s; NTFS warm-cache stat 10^4-10^5 entries/s, cold
about 10^3; 1 Gbps = 125 MB/s = 119.2 MiB/s.

## 3. Dated experiments

### O1 download tail and chunk ceiling (2026-09-09 to 2026-09-10, NVMe)

- `RANGE_CHUNK_TARGET` 8 MiB -> 2 MiB took the tail deficit from 1.1-2.1 s to
  0.25-0.54 s on `perf-redownload-small-ssd`; a wave-aligned chunk grid, a
  per-file ceiling equal to the global budget, largest-file-first ordering
  and fewer concurrent large files kept the range budget busy to the last
  byte. Accepted in `testkit/HYPOTHESES.md`.
- Path probe without Foxy code: 117.5-117.6 MB/s at 1, 2 and 8 MiB chunks with
  96 connections; 192 connections plateau identically. Chunk size is free at
  full concurrency against this origin. Build recipe:
  `testkit/ledger/perf-redownload-small-ssd.notes.md`.
- Ramp (the first samples climbing to the plateau) was identical with 96 or
  192 pre-established connections against this origin. That is a local
  finding about this path, not proof that ramp is never client-addressable.
- `download_stage_ms` used to include a fixed 5 s and 1 s quantum from
  background tasks sleeping on timers before reading their stop flag; removed
  2026-09-10. New download-path tasks wait on interruptible primitives.

### O1 rotational profile regression (2026-09-16, HDD)

A download profile with 3 concurrent large files, 16 small files and 32 MiB
chunks ran `perf-redownload-small-hdd` at 125.1-136.6 s (`sol` 0.28-0.30 by
the legacy model) against 45.2-58.2 s with the SSD network limits on the same
disk; a 13 MB mod queued 135 s behind the small-file lane. Rejected; details
in `testkit/HYPOTHESES.md`. The SSD limits plus `patch_applies=2` were kept.

### O2 delta insert requests on the full-download budget (2026-09-16, `6cbfc25` + uncommitted)

`perf-tfr-scifi-stale-check-ssd`, 40 patched files, 424,266,472 insert bytes
of 2,437,637,025 output bytes (82% saved), same origin as O1, `patched_files=40`,
`content_refresh_files_sampled=0`, oracle pass on every row.

| Build | Download stage | Stage average | Sampler peak window | Requests | Run id | Status |
| --- | --- | --- | --- | --- | --- | --- |
| 4 connections per file, 64 MiB runs (shipped) | 16.2-17.4 s | 25 MB/s | 71 MB/s | 236 | `20260916T082351Z` | `superseded` |
| global range budget, 2 MiB runs, fair-share cap | 11.6-12.7 s | 33-36 MB/s | 71-75 MB/s | 277 | `20260916T111954Z` | `superseded` |
| same, per-file ceiling instead of fair share | 12.0-12.3 s | 33 MB/s | 77 MB/s | 277 | `20260916T112533Z` | `superseded` (neutral) |
| same, ops larger than a chunk fetched as parallel chunks | 6.9-7.5 s | 54-59 MB/s | 113.6 MB/s | 231 runs + 129 chunks | `20260916T113531Z` | `accepted` |

The per-file `download` spans summed to 143 s inside the 16 s stage on the
shipped build (largest file 13.3 s alone); the accepted build's stage runs at
the path plateau.

The same change on the rotational lane (`perf-tfr-scifi-stale-check-hdd`,
warm sources, `20260916T113643Z`): 30.3-31.7 s against 28.7-30.0 s before,
inside HDD noise. Its per-file spans say why: `apply_wait` sums to 169-226 s
and `apply` to 55-57 s behind the `patch_applies=2` cap, so that lane is
bound by the disk applies (about 27 s serialized), not by the fetch. Status
`accepted` as neutral evidence for HDD; the fetch fix is an NVMe/network win. Peak consistency for the stage: `sol_raw` 0.47-0.49 on the
shipped build is not comparable with the accepted build's value, since the
peak window itself moved.

### O3 calibration accounting (2026-09-16)

- HDD (`perf-tfr-scifi-first-check-hdd`, `20260916T081256Z`): the large-part
  guard leaves one candidate profile; the 707 MB sample is hashed once in
  5.6-5.7 s and the 3.62 GB remainder in 35.2-35.5 s, 41 s for 4.33 GB at
  about 105 MB/s. The payload is hashed exactly once (`hash_work_bytes` equals
  the payload); there is no calibration waste on this lane and the 13.8 s
  benchmark figure the audit quoted does not appear in this run's log.
- NVMe (`perf-tfr-scifi-first-check-ssd`, `20260916T083021Z`): three
  candidates re-hashed the same sample from the page cache (8.5, 5.8 and
  9.7 GB/s; 1.33x payload hashed) and the selection measured cache warmth.
  With disjoint groups (`20260916T114141Z`, same case): three groups of
  three files (664, 446 and 604 MB) trialled once each, `hash_work_bytes`
  4,331,121,846 = the payload exactly (1.0x, was 5.75 GB), `hash_total_s`
  0.47 s against 0.55-0.60 s, selection still Balanced. The page cache is
  still warm on this lane (`evict-cache` does not evict the NVMe payload on
  this machine), so the profile choice remains a warm-cache choice; the
  redundant work is gone regardless. Status `accepted`.

### O4 stale-check overhead (2026-09-16)

The 0.445 s (HDD) / 0.447 s (NVMe) quoted for the outdated-but-untouched
check is the runner's `elapsed_s`. The app's `SOL op=quick_scan` says
`actual_s=0.021` with `tree_part_stats_load=15.89ms` the largest term; no
fixed overhead worth a change. Sub-second lanes are read through the app line.

### Section 8.2 lanes, cancellation and small-file fixes (2026-09-16 evening, `6cbfc25` + uncommitted)

The rest of the plan's case matrix, run on the evening origin (B1
`network-20260916-3bc06636`, 114.6 MB/s sustained, 2-3% under the
afternoon lane) with the app changes each lane forced. New kit surface:
`ui_probe_ms` on any GUI operation, `cancel_after_s` on every sync
operation, the mutator's `adjacent` profile and `preserve_mtime`, an
`origin.delay_ms` impairment with `unreachable` repositories, `synthetic
--mode swifty` and chunked large files, the `hosts` calibration lane
(HTTPS included), keyed-update rows in the `db` lane, `ramp_intervals_bps`
in the network lane, and the counters `patch_range_requests`,
`patch_gap_bytes`, `patch_copy_bytes`, `patch_cancelled`. New app records:
`ramp_s` / `plateau_s` / `tail_s` with their deficit bytes on the download
line, `rows_affected` on `db_persist`, `first_answer_s` / `last_answer_s`
on `startup_probe`, and `frame_ms` percentiles in the agent `fps` probe.

- Ramp and tail (`perf-redownload-small-ssd`, `20260916T173418Z` and
  `20260916T205020Z`): the 4.33 GB stage splits into a 3-4 s ramp
  (244-252 MiB short of the plateau), a 34-36 s plateau and a 0.7-2.8 s tail
  (59-70 MiB short), with all 96 ranges in flight from the first window. The
  calibration lane ramps identically at 96 and 192 connections (3, 8, 16-19,
  26-29, 43-48, 62-65, 84-86, 105-106 MB/s per 500 ms) while one connection
  is at full rate inside 0.5 s, so the ramp is the path, not the client;
  the "double the budget for the first seconds" idea is rejected without a
  build. After the fixes below the stage reads 39.5-39.8 s, 0.95 against
  the evening B1, peak 118.3 MB/s.
- Limiter above the path (`perf-redownload-small-ssd-limited-above`,
  `20260916T173645Z`): a 2000 Mbps cap reads 0.42 against the cap and
  0.90-0.91 against B1 at the same 40.6-40.8 s stage; the cap-relative
  number is not a regression when the cap sits above the path.
- Responsiveness under work (`perf-redownload-small-ssd-responsive`,
  `20260916T173824Z`): worst frame 14-20 ms and p95 6.7-7.2 ms under the
  download, 13-14 ms under the integrity recheck, smoothed fps never below
  216; the probe's own round trip is 155-215 ms (process spawn).
- Many tiny files (`perf-synthetic-tiny-files-ssd`, 32,016 x 8 KiB from the
  loopback origin): 822 s on the shipped rollback session
  (`20260916T175205Z`), 22.6-32.6 s after the append-only journal
  (`20260916T203538Z`), 25-36x; what remains is 293-357k persisted rows in
  10.9-14.1 s of write windows and 1,000 hash batches (6.0-10.6 s).
- One large file (`perf-synthetic-one-large-ssd`, `20260916T202401Z`): 2 GiB
  in 2.06-2.12 s (1.0 GB/s, peak window 1.36-1.41 GB/s), ramp 0 s, tail
  0.03-0.74 s; the range scheduler alone against the loopback path and the
  NVMe write.
- MD5 (`perf-synthetic-md5-check-ssd`, `20260916T181455Z`): 400 x 1 MiB
  hashed with `algorithm=md5` at 4.3-4.5 GB/s, 1.35-1.40 against the B6
  MD5 all-core lane (2.3 GB/s over one buffer); per-file parallel MD5 beats
  the single-buffer lane, which stays a floor.
- Delta locality (`perf-tfr-scifi-delta-locality-ssd`, `20260916T175048Z`):
  64 changed entries in 8 files as one run per file fetch 65 MB in 48 range
  requests with no over-fetch and copy 216 MB of source; scattered across
  the same files they fetch 90 MB in 74 requests (7 KB of gap) and copy
  335 MB, at the same 3.3-4.5 s stage; the resource cost, not the elapsed,
  is what locality changes at this size.
- Apply-time fallback (`perf-tfr-scifi-apply-fallback-ssd`,
  `20260916T203927Z`): with the planned files corrupted again under a
  preserved mtime, one of four plans failed its copy verification and fell
  back (`patch_fallbacks=1`, `patch_applies=3`), 29 MB moved, oracle pass.
- Cancel during patching (`perf-tfr-scifi-cancel-patch-ssd`): the first run
  (`20260916T175048Z`) found the detached hash flush and the fallback marking
  (section HYPOTHESES, "cancellation keeps its promises"); after the fixes
  (`20260916T204935Z`) cancel-to-quiescent is 728-730 ms and the resume
  applies the kept plans (33-34 patches, 438 MB, 6.7-7.3 s) instead of
  fetching 2.44 GB.
- Cancel during hashing (`perf-tfr-scifi-cancel-hash-hdd`, cold HDD): 26.2
  s to quiescence on the shipped build because the integrity pass took no
  cancel receiver (`20260916T204121Z` batch); 297-317 ms after
  (`20260916T204815Z`), with 712-758 MB hashed before the stop and the
  complete pass at 25.1-25.8 s.
- Cancel and resume of a force redownload (`perf-tfr-scifi-cancel-resume-ssd`,
  `20260916T174716Z`): cancel-to-quiescent 552-561 ms; the resume moves the
  whole 4.33 GB again because the rollback reverts every promoted file,
  finished mods included (161-165 files). The reused queue now prunes files
  that are verified and present, so this is the rollback policy, recorded as
  an open product row.
- Adverse origins (`perf-startup-adverse-origins`, `20260916T205421Z`): a
  fast live host, a loopback mirror holding every response 3 s and a closed
  port. First frame 378-423 ms; the fast branch answers at 86-108 ms, the
  slow one at 3.01-3.02 s, verdict settles at 2.97-2.98 s, the offline
  repository stays `unknown`, `outcome=settled` every time.
- Game-space switch after work (`perf-space-switch-live`,
  `20260916T204121Z`): a quick check in arma3 before the switch changes
  nothing: 16-20 ms request-to-visible (drain 5-7, reset 0-1, reload 9-14),
  11 repositories reloaded on the way back.
- Hosts lane (`hosts-20260916-8d57543a`, `hosts-20260916-e893198e`): the
  sanitized HTTP reference measured 39.6 / 80.8 / 41.8 ms (connect, fresh,
  kept-alive), and the sanitized HTTPS reference measured 25.7 / 84.2 /
  27.5 ms, with the TLS handshake hidden
  behind a shorter connect.
- Flagship after everything (`20260916T205243Z`): startup 529-581 ms, first
  frame 406-459 ms, verdict 61-80 ms, 11 probes answered between 88 and
  103 ms; stale-check NVMe delta 6.7-7.0 s (`20260916T205303Z`).

### Remaining plan items: lanes, cases and records (2026-09-16, `6cbfc25` + uncommitted)

The last pass over the audit's leftovers. New references: B5 metadata
(`metadata` lane, 308k first-pass / 289k warm entries per second on the NVMe
test volume, 180k / 171k on the HDD test volume) and a Turso workload lane
(`db`: 70k inserts and 126k deletes per second on the NVMe test volume, 42k /
84k on the HDD test volume, one writer, gate 1); B1 now
records the concurrency curve. New records: `SOL op=db_purge`, `SOL
op=space_switch`, `algorithm=` on hash lines, `entries=` on quick-scan lines.
New cases and what they measured:

- Limiter (`perf-redownload-small-ssd-limited`, `20260916T163230Z`): under a
  400 Mbps cap the download reads 95-97% against the cap (`limiter_cap`,
  modeled bound, peak window 50.7 MB/s) and 40-41% against B1. The two
  numbers answer different questions and the kit shows both; a cap-bound run
  is not a regression.
- Instrumentation A/B (`perf-redownload-small-ssd-profiled`,
  `20260916T162528Z`): download stage 40.1-40.7 s profiled against
  40.4-41.0 s unprofiled the same hour; inside noise on a network-bound lane.
- Cancel and resume (`perf-tfr-scifi-cancel-resume-ssd`, `20260916T163019Z`):
  cancel-to-quiescent 563-661 ms with 4-5 mods complete and 592-667 MB
  credited; the resume then moved the whole 4.33 GB again (40.4-40.7 s).
  Cancellation is clean; the resume does not reuse completed files (open in
  `testkit/HYPOTHESES.md`).
- Changed source after a plan (`perf-tfr-scifi-patch-fallback-ssd`,
  `20260916T164148Z`): the download's quick verify saw the second mutation
  and re-planned (5 patched files, 0 apply-time fallbacks, oracle pass), so
  the apply-time fallback path stays covered by the unit tests (wrong chunk
  checksum) rather than by a kit lane.
- Game-space switch (`perf-space-switch-live`, `20260916T162951Z`): request
  to the new space visible 17-22 ms (drain 6-7 ms, reset 0-1 ms, reload
  10-16 ms for 11 repositories); the round trip leaves arma3 fully reloaded.
  Runner elapsed 0.45-0.52 s is driver round trips plus the target space's
  startup work.
- HDD delta on a fresh payload (`perf-tfr-scifi-stale-check-hdd`,
  `20260916T164216Z`, the isolated HDD payload deleted and re-downloaded
  first): 28.4 s on the first mutate-and-patch cycle, 31.6 s on the second,
  against 32.3-32.6 s on the worn payload and 28.7-30.0 s shipped. The
  drift was payload wear; each cycle rewrites the patched outputs on
  rotational media and the next cycle pays for it.
- Quick scan against B5: 3-6% on the untouched stale check (255 entries in
  a 21 ms scan), 21-28% on a clean recheck, 38-54% on the startup sweep;
  enumeration is a minority of the scan, the rest is cache validation and
  DB reads.
- Flagship recheck after these changes (`20260916T163824Z`, O1) and O8: O1
  40.4-41.0 s stage, peak 115.7-116.3 MB/s, 0.90-0.91 against B1 (the origin
  read 1-2 MB/s lower this hour than at calibration); O6 0.88-0.94 against
  B4; O8 startup 622-686 ms (`20260916T164052Z`, first frame 454-523 ms,
  verdict 44-86 ms) with the probe at 0.72-0.76.

### Action records and calibrated references (2026-09-16, `6cbfc25` + uncommitted)

Phase 2 gave every operation an owner and the pipeline a terminal record;
Phase 3 gave the flagship lanes independent references. Nothing in the app's
hot paths changed in this step, so the numbers are the same builds read
better, plus the four-file delta lanes and the memory lane rerun after the
request-budget change.

- Ownership: every `SOL` line emitted inside a sync action now carries its
  `op_id` (hash batches, quick scans, the remote refresh); the startup line
  and its probe share one id. `SOL op=sync_action` (one per pipeline exit,
  with `stage_<name>_s` per stage), `SOL op=remote_refresh` (branch and
  manifest work) and `SOL op=db_persist` (gated write windows per action)
  are the new records. On the delta lane the download action's 604 gated
  writes in 16 categories sum to 125-126 ms inside a 7.5 s action; the
  clean recheck is `remote_repository` 0.124-0.139 s and nothing else.
- References (`foxy-testkit calibrate`, section 2): B1 117.5 MB/s
  sustained against the origin, B4 81 ms fresh / 41 ms reused, B2/B3 per
  volume, B6 41 GB/s BLAKE3 on 32 threads. Rows cite the lane ids and carry
  `sol_calibrated`; a recalibration retires the baselines that cited the
  old ids.
- What the calibrated ratios say: O1 0.92-0.93 (the same figure as the
  same-run peak consistency, so the peak was the path plateau all along);
  O2 40-file NVMe 0.52-0.53 of B1 for its 424 MB (6.9-7.2 s, the rest is
  apply and promotion); O6 0.93-0.94 (two index requests); the startup probe
  0.78-0.86; O3 warm NVMe 0.80 against the slower of B2 warm parallel read
  and B6, `hash_total_s` 0.39 s. Iteration-zero NVMe hash rows read 1.36-1.97
  against the device lane: the "cold" lane is page-cache warm on this
  machine and the ratio says so instead of a plausible percentage.
- Four-file delta lanes after the request-budget change: NVMe 1.66-1.87 s
  (1.71-1.91 s shipped), HDD evicted 2.75-2.85 s (2.82-3.02 s shipped);
  neutral, as expected for 5 MB.
- HDD 40-file delta: 32.3-32.6 s this build against 28.7-30.0 s shipped and
  30.3-31.7 s with the budget alone. Blob fetch service is under 2 s in all
  three; the stage is the `patch_applies=2` apply path. The payload on the HDD
  test volume has been mutated and patched many times today, so the drift is not
  attributed until a fresh-payload rerun; recorded as `open` in
  `testkit/HYPOTHESES.md`.
- Memory lane (`20260916T133825Z`): startup peak 460-473 MB, retained
  355-369 MB, UI-walk retention +37-40 MB (saturating, as on Sept 10).
  Advisory by the resource trade policy; no time metric moved.

### O1 preparation and finalization (2026-09-16, evidence only)

On the delta lane the click-to-first-request is 219-224 ms (95 ms from
`Reusing confirmation-prepared download queue` to the first `Starting download
for mod`); the transfer stage ends within 0.3 s of the last file's promotion.
Neither is a candidate while the transfer itself is on the path plateau.

### O7 WAL versus MVCC (2026-09-09, Turso 0.7.2, Foxy 1.2.0, gate 4, NVMe)

| Case | WAL | MVCC |
| --- | --- | --- |
| Small redownload (217 files) write time | 125 ms | 210 ms (+68%) |
| Small redownload elapsed | about 51 s | about 51 s |
| Big redownload (3,738 files, about 92 GB) write time | 11.6 s | 37.4 s (about 3x) |
| Big redownload elapsed | 801 s | 805 s |
| Recheck after the big download | 0.44 s | 0.45 s |

MVCC is worse on persistence and not faster on wall clock; the sync ratios do
not show the engine benches where MVCC looks better. Shipping journal is WAL.
Full matrix and re-run steps: `conventions/CORE_CONVENTIONS.md` (WAL vs MVCC).
Do not reopen without a new reason such as an engine change.

`db_write_time_ms` and the per-category `txn_ms` are gated transaction windows:
one frozen refresh reports about 300 ms at gate 1 and about 2,180 ms at gate 8
for identical work. Never compare either number across gate sizes. Before
2026-09-09 the per-category field was named `total_ms`; only the name changed.

### O8 startup levers (2026-09-10, NVMe, 11 repositories, 10 probed)

1. Moving the startup system summary (466 ms) and the editor mission scan
   (267 ms, 3.7 s cold) off the paint path took first frame from
   1,259-1,342 ms to 530-561 ms.
2. Starting the network probe in `Foxy::new` instead of after first paint took
   `verdict_s` from 128-142 ms to 49-51 ms.
3. Replacing a `COUNT`/`SUM` over every part row (1.03 s on a 141k-part
   repository, gating all eleven verdicts) with `LIMIT 1` probes.
4. Bounding the probe stage above the per-request timeout.
5. Pinning the graphics backend that last reached a window: first frame
   561-575 ms enumerating three backends, 393-397 ms pinned
   (`ui/launcher.rs`; a narrowed launch that fails retries with the full list).

The published 74% for the 0.523-0.533 s settled verdict is `disputed`: the
line printed a 0.34 s light (64-65%), while the 0.39 s measured paint reference
gives 73-75%, and the probe overlaps paint rather than adding to it. Recompute
under the O8 card's dependency model before quoting a ratio.

### M1 memory levers (2026-09-10, empty configuration unless stated)

1. `MemoryHints::MemoryUsage` instead of the wgpu default: 468 MB -> 290 MB
   private commit, working set unchanged.
2. Remembering the graphics backend that worked: 290 MB -> 221-239 MB commit,
   working set 237 MB -> 175-216 MB.
3. Streaming the model tree's part rows (`DbTxn::query_each`) instead of
   collecting them: a structural saving; on the metadata-refresh lane the
   run-to-run spread is wider than the difference, elapsed moved (-15%) where
   footprint did not.
4. Three `HashMap::with_capacity(pairs.len())` calls sized by every part to
   hold one entry per file.

Open, not attributed: the first walk through every view costs about +44 MB of
commit that is never given back, identical after three or eight passes, so a
one-time fill rather than a leak. Foxy's own buckets (2.5 MB) and the glyph
atlas (8192x128, one percent full) do not explain it. Measure with the series
in `memory-<iteration>-ui-walk.json` before claiming a cause.

## 4. Tracking table (archive)

Rows are verbatim from the convention as it stood on 2026-09-16, with a
status column added. Ratios in `legacy` rows were computed by the pre-2026-09-16
model: `sol` clamped at 1, the download peak measured as bytes per nominal
sampler tick, hash `compute_s` meaning blocking-task elapsed, and hash rows
showing the last batch only. They remain readable and replayable; they are
not references for rows recorded under the corrected model.

| Status | Date | Version | Machine | Op | Work | T_actual | R_actual | Light (src) | sol | Bottleneck | Action |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| legacy | _2026-06-13_ | _1.0.0_ | _example_ | download | 8.2 GiB | 612 s | 14.3 MiB/s | 15.0 MiB/s (limiter) | 0.95 | limiter | none - at light |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.210 s | 456.1 addons/s | 2,462 addons/s (B5 self) | 0.185 | no cache hits, DB load | persistent cache empty; track cache-key fix |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.071 s | 1,356.0 addons/s | 2,462 addons/s (B5 self) | 0.551 | addon hash + DB load | cache_hits_persistent=0 |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.074 s | 1,305.9 addons/s | 2,462 addons/s (B5 self) | 0.530 | addon hash + DB load | cache_hits_persistent=0 |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.069 s | 1,399.2 addons/s | 2,462 addons/s (B5 self) | 0.568 | addon hash + DB load | best in this log, still under B5 |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.072 s | 1,324.2 addons/s | 2,462 addons/s (B5 self) | 0.538 | addon hash + DB load | cache_hits_persistent=0 |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.071 s | 1,353.6 addons/s | 2,462 addons/s (B5 self) | 0.550 | addon hash + DB load | cache_hits_persistent=0 |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | hash benchmark Conservative | 737.6 MiB | 0.362 s | 2,037.9 MiB/s | 6,003.6 MiB/s (same-run best) | 0.339 | profile limits | Balanced wins on this run |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | hash benchmark Balanced | 737.6 MiB | 0.123 s | 6,003.6 MiB/s | 6,003.6 MiB/s (same-run best) | 1.000 | at same-run light | selected profile |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | hash benchmark Aggressive | 737.6 MiB | 0.126 s | 5,859.4 MiB/s | 6,003.6 MiB/s (same-run best) | 0.976 | near same-run light | no action |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | hash selected remaining | 20.60 GiB | 6.571 s | 3,209.5 MiB/s | 6,003.6 MiB/s (benchmark best) | 0.535 | file mix, stragglers | 1086 files, max file 2.410 s |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | repository DB purge | 440,492 rows | 144.54 s | 3,047 rows/s | self_baseline | na | SQLite delete | zero-row part delete took 78.59 s; subfile delete took 59.59 s |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | deferred part insert | 433,016 rows | 107.30 s | 4,035 rows/s | self_baseline | na | SQLite insert with live indexes | 1,692 batches of 256; biggest sync cost |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | remote refresh rebuild | 3,738 files | 125.73 s | 29.7 files/s | self_baseline | na | DB persistence | 110.84 s DB write time; tree_hash_bootstrap 118.29 s |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | no-change remote skip | repo.json + foxy_addons | 0.27 s | 1 clean verdict | self_baseline | na | RTT + quick verify | repeat clean skip after rebuild |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 96 addons | 0.072 s | 1,324.4 addons/s | 2,462 addons/s (B5 self) | 0.538 | addon hash + DB load | large-repo startup quick scan |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.167 s | 245.6 addons/s | self_baseline | na | missing-file diff | 41-addon repo before download: 41 addons updated, 1,515 files missing |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.158 s | 260.2 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.154 s | 265.5 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan updates | 41 addons | 0.153 s | 268.8 addons/s | self_baseline | na | missing-file diff | repeated update check before download |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | download | 22.56 GiB | 222.671 s | 103.73 MiB/s | 112.92 MiB/s (peak_1s) | 0.919 | network path | 1,515 full downloads, 1,515 files, no retries |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | download pipeline | 22.56 GiB | 247.54 s | 93.31 MiB/s | self_baseline | na | post-download tail | download 240.68 s, hash_finalize 17.75 s, DB writes 14.84 s |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | hash finalize tail | 22.56 GiB | 15.15 s | 1,524.6 MiB/s | self_baseline | na | rollup or persistence tail | all 1,515 files incrementally hashed during download |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | download DB checkpoint | 3,583 rows | 1.79 s | 2,002 rows/s | self_baseline | na | SQLite progress persistence | 36 batches, avg_batch=49.6ms |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 41 addons | 0.040 s | 1,019.4 addons/s | 2,462 addons/s (B5 self) | 0.414 | addon hash + DB load | 41-addon repo clean after download |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | quick_scan clean | 41 addons | 0.042 s | 972.4 addons/s | 2,462 addons/s (B5 self) | 0.395 | addon hash + DB load | 41-addon repo remote skip verification |
| legacy | 2026-07-03 | 1.0.0 | 9950X3D desktop | no-change remote skip | repo.json + foxy_addons | 0.21 s | 1 clean verdict | self_baseline | na | RTT + quick verify | 41-addon repo clean skip after download |
| legacy | 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 WAL | 9950X3D NVMe | force-redownload | 217 files | 51.17 s | 86.4 MB/s | peak_1s | 0.79 | network | perf-redownload-small-ssd warm; db_write 125 ms |
| legacy | 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 MVCC | 9950X3D NVMe | force-redownload | 217 files | 51.35 s | 86.3 MB/s | peak_1s | 0.78 | network | same case; db_write 210 ms (+68%); elapsed no-difference |
| legacy | 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 WAL | 9950X3D NVMe | force-redownload | 92.2 GB / 3738 files | 801 s | 116.1 MB/s | peak_1s | 0.97 | network | perf-redownload-big-ssd cold; db_write 11.6 s |
| legacy | 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 MVCC | 9950X3D NVMe | force-redownload | 92.2 GB / 3738 files | 805 s | 115.4 MB/s | peak_1s | 0.97 | network | same case; db_write 37.4 s (~3x); elapsed no-difference |
| legacy | 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 WAL | 9950X3D NVMe | recheck | large repo after redownload | 0.44 s | 1 clean | self_baseline | na | RTT + quick verify | big-ssd cold recheck |
| legacy | 2026-09-09 | 1.2.0 Turso 0.7.2 gate 4 MVCC | 9950X3D NVMe | recheck | large repo after redownload | 0.45 s | 1 clean | self_baseline | na | RTT + quick verify | same; not faster than WAL |
| superseded | 2026-09-09 | 1.2.0 pre-tail-work | 9950X3D NVMe | download | 4.33 GB / 217 files | 45.11 s | 96.0 MB/s | 117.8 MB/s (peak_1s) | 0.815 | tail | perf-redownload-small-ssd warm. Plateau already at the ceiling; 2.1 s ramp + 10-12 s tail |
| legacy | 2026-09-10 | 1.2.0 post-tail-work | 9950X3D NVMe | download | 4.33 GB / 217 files | 39.79 s | 108.8 MB/s | 118.4 MB/s (peak_1s) | 0.922 | ramp | same case, same bytes/files. Tail deficit 0.25-0.54 s; run is now ramp-bound |
| superseded | 2026-09-09 | 1.2.0 pre-tail-work | 9950X3D HDD | download | 4.33 GB / 217 files | 65.12 s | 66.5 MB/s | peak_1s | 0.563 | tail | perf-redownload-small-hdd warm median 67.6 s |
| legacy | 2026-09-10 | 1.2.0 post-tail-work | 9950X3D HDD | download | 4.33 GB / 217 files | 44.45 s | 97.4 MB/s | peak_1s | 0.825 | tail + disk | same case; warm median 48.4 s. Spinning media is noisy, read medians |
| accepted | 2026-09-10 | probe (no Foxy code) | 9950X3D NVMe | path ceiling | 96 conns, 1/2/8 MiB chunks | 20 s each | 117.5-117.6 MB/s | 118.4 MB/s | ~0.99 | path | chunk size is free at full concurrency; 192 conns plateau identically |
| superseded | 2026-09-10 | 1.2.0 pre-startup-work | 9950X3D NVMe | startup | 11 repos, 10 probed | 1.500 s | 1 verdict | 0.62 s (paint 0.54 + 2xRTT 0.08) | 0.41 | pre-paint blocking work | perf-startup-arma3-live warm median. first_frame 1.30 s, verdict 0.135 s |
| superseded | 2026-09-10 | 1.2.0 post-startup-work | 9950X3D NVMe | startup | 11 repos, 10 probed | 0.660 s | 1 verdict | 0.59 s (paint 0.54 + 2xRTT 0.08 overlapped) | 0.89 | renderer init | same case. first_frame 0.54 s, verdict 0.050 s; probe now overlaps paint |
| legacy | 2026-09-10 | 1.2.0 post-startup-work | 9950X3D NVMe | startup_probe | 10 repo.json | 0.092 s | 10 probes | 0.080 s (2 x B4) | 0.87 | RTT | all probes at one depth; 12 ms client overhead |
| accepted | 2026-09-10 | probe (no Foxy code) | 9950X3D NVMe | repo.json GET | 1 062 B | 0.083 s | fresh conn | 0.080 s (2 x B4) | 0.96 | RTT | keep-alive repeat 0.042 s = 1 x RTT; payload is not a term |
| legacy | 2026-09-10 | 1.2.0 | 9950X3D NVMe | app_update_check | 1 manifest | 0.085 s | 1 check | 0.080 s (2 x B4) | 0.94 | RTT | `SOL op=app_update_check`, outcome=up_to_date |
| superseded | 2026-09-10 | 1.2.0 pre-memory-work | 9950X3D NVMe | footprint empty config | launch, no repositories | n/a | 468 MB commit / 237 MB WS | 133 MB (glow floor) | na | wgpu reserve | three backends enumerated, `MemoryHints::Performance` |
| accepted | 2026-09-10 | 1.2.0 post-memory-work | 9950X3D NVMe | footprint empty config | launch, no repositories | n/a | 221 MB commit / 175 MB WS | 133 MB (glow floor) | na | wgpu device | `MemoryHints::MemoryUsage` + pinned backend; -53% commit, -26% WS |
| superseded | 2026-09-10 | 1.2.0 pre-memory-work | 9950X3D NVMe | footprint startup | 11 repos, live arma3 space | n/a | 680 MB peak / 581 MB retained | 468 MB (B8 then) | na | renderer floor | perf-memory-arma3-live |
| accepted | 2026-09-10 | 1.2.0 post-memory-work | 9950X3D NVMe | footprint startup | 11 repos, live arma3 space | n/a | 438 MB peak / 343 MB retained | 221-239 MB (B8) | na | renderer floor | same case; -36% peak, -41% retained |
| superseded | 2026-09-10 | 1.2.0 pre-memory-work | 9950X3D NVMe | footprint recheck | largest configured repository, clean | n/a | 595 MB peak / 593 MB retained | 468 MB (B8 then) | na | renderer floor | warm median of three |
| accepted | 2026-09-10 | 1.2.0 post-memory-work | 9950X3D NVMe | footprint recheck | largest configured repository, clean | n/a | 346 MB peak / 344 MB retained | 221-239 MB (B8) | na | renderer floor | same case; -42% retained |
| superseded | 2026-09-10 | 1.2.0 pre-memory-work | 9950X3D NVMe | footprint ui-walk | 133 view steps, 3 passes | n/a | 620 MB peak / 614 MB retained | 468 MB (B8 then) | na | unattributed | growth +21 MB |
| accepted | 2026-09-10 | 1.2.0 post-memory-work | 9950X3D NVMe | footprint ui-walk | 134 view steps, 3 passes | n/a | 393 MB peak / 390 MB retained | 221-239 MB (B8) | na | unattributed | same case; growth +45 MB, identical at 8 passes; buckets 2.5 MB, atlas 1% full |
| superseded | 2026-09-10 | 1.2.0 pre-memory-work | 9950X3D NVMe | footprint remote-refresh (CLI) | 96 mods, 433k parts | 1.78-1.93 s | 373 MB peak (median of 3) | no renderer in a CLI run | na | manifest parse + part upsert | perf-db-refresh-main, same binary minus the app changes |
| accepted | 2026-09-10 | 1.2.0 post-memory-work | 9950X3D NVMe | footprint remote-refresh (CLI) | 96 mods, 433k parts | 1.50-1.53 s | 345-370 MB peak (two runs, medians of 3) | no renderer in a CLI run | na | manifest parse + part upsert | same case. Elapsed is a real -15% and reproduced twice; **memory is not** - the within-run spread is 330-425 MB either side, so the streamed read shows no measurable footprint change on this lane |
| superseded | 2026-09-10 | 1.2.0 pre-memory-work | 9950X3D NVMe | footprint force-redownload | 4.33 GB / 217 files | 40.8-41.0 s | 727 MB peak / 521 MB retained (medians) | 468 MB (B8 then) | na | renderer floor | perf-redownload-small-ssd |
| accepted | 2026-09-10 | 1.2.0 post-memory-work | 9950X3D NVMe | footprint force-redownload | 4.33 GB / 217 files | 40.4-40.9 s | 563 MB peak / 339 MB retained (medians) | 221-239 MB (B8) | na | download buffers | same case, same 217 files and 4 331 121 846 bytes; -23% peak, -35% retained, elapsed and working-set peak unchanged; payload content-verified by `foxy-testkit-oracle` (3 744 parts, 0 problems) |
| disputed | 2026-09-10 | 1.2.0 post-memory-work | 9950X3D NVMe | startup | 11 repos, 10 probed | 0.523-0.533 s | 1 verdict | 0.34 s (paint 0.39 measured, 2 x RTT overlapped) | 0.74 | renderer init | same case as the O8 rows above; pinned backend cut first frame 0.56 s -> 0.39 s. The printed 0.34 s light gives 0.64-0.65; the 0.39 s paint reference gives 0.73-0.75 |
| legacy (rejected profile) | 2026-09-16 | 1.2.0 rotational download profile (3 large, 16 small, 32 MiB) | 9950X3D HDD | download | 4.33 GB / 217 files | 125.1-136.6 s | 32 MB/s | 108 MB/s (peak_1s) | 0.28-0.30 | connections | perf-redownload-small-hdd `20260916T051653Z`, three reps. **Regression** of the HDD profile shipped with the Sept-14 build; a 13 MB mod queued 135 s behind the small-file lane. Rejected in `testkit/HYPOTHESES.md` |
| legacy | 2026-09-16 | 1.2.0 SSD network limits on HDD, patch_applies=2 | 9950X3D HDD | download | 4.33 GB / 217 files | 45.2-58.2 s | 74-96 MB/s | 118 MB/s (peak_1s) | 0.63-0.81 | tail + disk | same case `20260916T053420Z`; back inside the 44-58 s spread of the 2026-09-10 rows. Hashing fully overlapped: 60 incremental batches inside the stage, final flush 13 MB, `files_reused_from_hash_pass=217/217` |
| legacy | 2026-09-16 | 1.2.0 | 9950X3D NVMe | download | 4.33 GB / 217 files | 39.4-39.7 s | 109 MB/s | 118 MB/s (peak_1s) | 0.92-0.93 | ramp | perf-redownload-small-ssd `20260916T082712Z`; unchanged against the 39.5 s baseline. The `hash.actual_s` verdict (63 -> 90 ms on the last 424 MB page-cache batch) is scheduling noise, candidate since 2026-09-10 |
| disputed | 2026-09-16 | 1.2.0 | 9950X3D HDD | hash baseline (first check, cold) | 4.33 GB / 217 files | 35.2 s remaining after 13.8 s benchmark | 103 MB/s | ~110 MB/s (B2 HDD) | ~0.94 | disk | perf-tfr-scifi-first-check-hdd `20260916T081256Z`: `hash_work_bytes` equals the payload once, `tree_verify_runs=0`, `content_refresh_files_sampled=0`. The ratio covers the remaining pass only, not the 49.0 s first check |
| legacy | 2026-09-16 | 1.2.0 | 9950X3D NVMe | hash baseline (first check) | 4.33 GB / 217 files | 0.24 s remaining | page cache | n/a | na | benchmark | perf-tfr-scifi-first-check-ssd `20260916T083021Z`: 1.33 x payload hashed because the auto benchmark tries three profiles on its 707 MB sample; expected, case budget 5.85 GB |
| accepted | 2026-09-16 | 1.2.0 | 9950X3D HDD | quick_scan outdated, untouched | 217 files, 40 outdated | 0.445 s | 0 bytes hashed | self_baseline | na | RTT + DB | perf-tfr-scifi-stale-check-hdd `20260916T080847Z` `quick-check-stale`; the following recheck 0.547 s, also 0 bytes. The plan's "every later check while outdated" row, 260 s -> under 1 s |
| accepted | 2026-09-16 | 1.2.0 | 9950X3D NVMe | quick_scan outdated, untouched | 217 files, 40 outdated | 0.447 s | 0 bytes hashed | self_baseline | na | RTT + DB | perf-tfr-scifi-stale-check-ssd `20260916T082351Z`; same shape as the HDD row, the path is not disk-bound |
| legacy | 2026-09-16 | 1.2.0 | 9950X3D HDD | delta update after a check | 40 patched / 424 MB wire | 28.7-30.0 s | 14.6 MB/s wire | peak_1s | 0.36 | patch apply cap 2 | perf-tfr-scifi-stale-check-hdd download: `prepared_queue_reuses=1`, first `Starting download for mod` 219-224 ms after the click, `hash_source=segments` 40/40, `content_refresh_files_sampled=0`, `hash_batches_after_download=0` |
| legacy | 2026-09-16 | 1.2.0 | 9950X3D HDD | delta update, evicted sources | 40 patched / 424 MB wire | 57.6-58.8 s | 7.3 MB/s wire | peak_1s | 0.31-0.35 | patch copy reads from cold disk | perf-tfr-scifi-first-check-hdd download after `evict-cache`; same counters as the row above. Twice the warm row: the copy sources are read from the platter |
| legacy | 2026-09-16 | 1.2.0 | 9950X3D NVMe | delta update after a check | 40 patched / 424 MB wire | 16.2-17.4 s | 25 MB/s wire | peak_1s | 0.43-0.53 | insert-range requests | perf-tfr-scifi-stale-check-ssd and first-check-ssd downloads, `download_patch_applies_limit=60`, same correctness counters as HDD |
| legacy | 2026-09-16 | 1.2.0 | 9950X3D HDD | delta patch, 4 files | 4 patched / 5.2 MB wire, 97% saved | 2.82-3.02 s | n/a | peak_1s | 0.40 | blob RTT | perf-tfr-scifi-delta-patch-hdd `20260916T082008Z`: `hash_source_reread_files=0`, promotion fingerprints reused, `content_refresh_files_sampled=0` |
| legacy | 2026-09-16 | 1.2.0 | 9950X3D NVMe | delta patch, 4 files | 4 patched / 5.2 MB wire, 97% saved | 1.71-1.91 s | n/a | peak_1s | 0.65-0.70 | blob RTT | perf-tfr-scifi-delta-patch-ssd `20260916T082243Z`; same counters |

New rows go above the archive as a short curated block, one per accepted
measurement that changes a decision: date, checkout, case or run id, operation
label, lane, metric kind, actual, reference, `sol_raw`, outcome, and where the
raw artifact lives (a saved benchmark id or a `testkit/runs/` id; never a
committed log, database, cache or machine path).
