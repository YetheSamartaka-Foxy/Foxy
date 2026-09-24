# Performance hypotheses

Entries are append-only during an optimization loop. Close one with its ledger
row; do not delete rejected ideas.

Direction is stated for the metric named: a SoL ratio improves when it goes
*higher*, a duration when it goes *lower*. Since 2026-09-16 the hash rows
gate on the summed hash wall time rather than `hash.sol`, which for a
self-baseline operation is never a ratio; four open rows were corrected from
"lower" ratio to the direction they actually mean, without changing their
claims.

| Status | Claim | Primary metric | Expected direction | Cheapest case |
| --- | --- | --- | --- | --- |
| accepted | A wave-aligned range grid plus a per-file ceiling equal to the global budget removes the straggler wave that is the download tail | `download.sol` | higher | `perf-redownload-small-ssd` |
| accepted | The download stage waits out the checkpoint and sampler sleeps before reading their stop flags | `summary.download_stage_ms` | lower | `perf-redownload-small-ssd` |
| accepted | Largest-file-first within a mod, and half as many concurrent large files, shorten the makespan tail | `download.sol` | higher | `perf-redownload-small-ssd` |
| accepted | Reducing the range chunk from 8 MiB to 2 MiB reduced the measured tail deficit from 1.1-2.1 s to 0.25-0.54 s on the reference lane | `download.tail_deficit_bytes`, `download.tail_s` | lower | `perf-redownload-small-ssd` |
| rejected | More than 96 concurrent range requests buys aggregate throughput | `download.sol` | higher | probe against the reference origin |
| open | Tune per-file range count and global range budget against RTT | `download.sol` | higher | delta scattered |
| open | Coalesce small writes without increasing retries | `download.sol` (higher), `breakdown.disk` write time | higher ratio, lower time | delta scattered |
| open | Reuse TLS connections more effectively | `breakdown.network.permit_wait_s` | lower | force redownload |
| open | Tune patch copy buffer for spinning disks | `summary.total_ms` | lower | delta single-entry HDD |
| closed | Repeated layout discovery or mapping contributes materially to the full evicted HDD recheck; reducing it lowers action time without changing part work | `layout_sum`, full recheck elapsed | n/a, not isolated: parsing the layout through the hash reader shipped in the round 1 bundle (-1.8% HDD with three other changes); per-file seeks, not layout CPU, were the cost | `perf-hdd-plan-main-recheck-{hdd,ssd}` |
| closed | Let the game module name its container format (Arma 3 PBO, Reforger PAC1) so hashing never opens an archive head to tell PBO from gapless PAC1 | `layout_sum`, full recheck elapsed | n/a, kept as a correctness-neutral cleanup: -0.31% HDD and -1.12% SSD, both within tolerance against accepted clean baselines | `perf-hdd-plan-main-recheck-{hdd,ssd}` |
| accepted | The two worker HDD hash schedule causes enough extra seeking that a more sequential read order reduces the full evicted recheck median | full recheck elapsed | lower: on-disk (first cluster) job order -1.8%, and 16, 32 and 64 MiB rotational reads -2.9%, -5.0% and -3.0% in three-run screens; the final 7/7 gate measured 547.26 s against 650.985 s (-15.9%) | `perf-hdd-plan-main-recheck-hdd-screen`, `perf-hdd-plan-main-recheck-hdd` |
| accepted | The post-hash fingerprint's eight 16 KiB samples, read through the 4 MiB hash reader, refill it each time and re-read 21.8 GB (23.6%) of a full TFR Main recheck | process read bytes / hashed bytes | lower: 1.200 on the notebook's old build, 1.003 once the samples come from the streamed bytes (`FingerprintTap`) | `perf-hdd-plan-main-recheck-{hdd,ssd}` with `memory.read_transfer_bytes` |
| accepted | A parts-weighted progress bar runs far ahead of a heavy-first recheck; bytes track the work | `summary.progress_probe.max_gap_points` | lower: 1.5 points from linear on the 547 s HDD recheck (the parts bar showed 76% at 5 of 14 minutes on the notebook) | `perf-hdd-plan-main-recheck-hdd` with `--progress-probe-ms` |
| rejected | One HDD hash worker with 64 MiB reads beats two, because the head no longer alternates between files | full recheck elapsed | n/a, 586.14 s against 555.78 s with two workers (+5.5%): the second stream keeps the disk busy while the first hashes and opens its next file | `perf-hdd-plan-main-recheck-hdd-screen` |
| rejected | The sequential-scan hint helps hash reads on every storage class | full recheck elapsed | n/a on SSD: 32.77 s with the hint against 29.74 s without (the baseline is 31.735 s); kept on rotational storage, where it was part of a -1.0% screen step | `perf-hdd-plan-main-recheck-ssd` |
| accepted | 128 MiB rotational reads gain again (the gain per doubling fell from 5% to 3%) | full recheck elapsed | lower: 539.95 s against 555.78 s at 64 MiB (-2.8%) in a three-run screen | `perf-hdd-plan-main-recheck-hdd-screen` |
| accepted | An unbuffered overlapped reader (`FILE_FLAG_NO_BUFFERING`, aligned buffers, reads in flight on one handle) closes the SSD gap to the 6.1 GB/s 32-reader reference | full recheck elapsed on SSD | lower: 19.52 s against 29.74 s (-34%); hashing reads 87.4 GB in 12.3 s, 7.1 GB/s, above the reference | `perf-hdd-plan-main-recheck-ssd` |
| accepted | On HDD, the same reader with one file streaming at a time (`DiskTurn`) beats two cached streams | full recheck elapsed | lower: 525.71 s against 539.95 s (-2.6%), spread 1.2 s | `perf-hdd-plan-main-recheck-hdd-screen` |
| rejected | Adjusting the SSD worker count by throughput during the run beats the Auto choice | full recheck elapsed on SSD | n/a: 20.69 s against 19.52 s; throughput stayed at 7.1 GB/s from 4 to 16 workers, so the disk, not the count, is the limit | `perf-hdd-plan-main-recheck-ssd` |
| accepted | A verified-hash record keyed by NTFS file id, size, write and change time and the USN (where a journal exists) lets a bootstrap after a database wipe skip files proven untouched | bootstrap elapsed | lower: 12.08 s against about 526 s; all 3,738 files restored in 0.47 s and the run ended clean. Accepted baseline `dc09a72`: 11.80 s median of 7 [11.54-12.44], every run restoring all 3,738 files in 0.48-0.53 s. Never used by an integrity recheck or a force redownload, or for files the database knows | `perf-hdd-plan-main-bootstrap-record-hdd` |
| rejected | 32 MiB non-cached blocks beat 16 MiB on HDD | full recheck elapsed | n/a: 525.98 s against 525.71 s; one stream already saturates the disk | `perf-hdd-plan-main-recheck-hdd-screen` |
| rejected | One non-cached read per SSD worker instead of two keeps the time and lowers peak memory | full recheck elapsed on SSD | n/a: 20.48 s against 19.72 s; peak private memory 1.60 against 2.00 GB, but the resource trade policy never takes footprint over time | `perf-hdd-plan-main-recheck-ssd` |
| accepted | The desktop HDD read-path changes carry over to a laptop HDD, where each switch between files costs about four times as much | notebook full recheck elapsed | lower: 613.75 s against 829.04 s (-26.0%) on `b1c4684`, one export with extended diagnostics like the one it is compared with; hashing 151.9 MB/s against 112.3, per-file overhead 12.3 ms against 18.8, process reads / hashed 1.01, every file non-cached with no fallback; reads then follow the platter zone curve, so the run is disk-bound | notebook benchmark export (`bm-20260923-184708-recheck.zip`) |
| accepted | mimalloc 3 as the global allocator costs less CPU and time than the Windows process heap on the checks Foxy runs | `memory.cpu_s`, elapsed | lower: SSD recheck about 36.5 CPU s and 19.2 s against 38.5 s and 19.99 s; record restore 14.7 CPU s and 11.56 s against 16.7 s and 12.24 s; HDD screen 525.28 s (unchanged, disk-bound); the start footprint no longer grows from check to check | `perf-hdd-plan-main-recheck-ssd`, `perf-hdd-plan-main-bootstrap-record-hdd`, `perf-hdd-plan-main-recheck-hdd-screen` |
| accepted | Committing mimalloc pages on demand removes committed memory the hash threads never touch, at no time cost | `memory.peak_private_bytes`, elapsed | lower: SSD peak private 2.18 GB against 2.85 GB with whole-page commit; interleaved A/B 19.22 s against 19.29 s | `perf-hdd-plan-main-recheck-ssd` |
| rejected | Pooling the small-file read buffer per hash thread cuts SSD page faults and peak | page faults, `memory.peak_private_bytes` | n/a: 602k faults and 2.83 GB against 622k and 2.87 GB, within spread | `perf-hdd-plan-main-recheck-ssd` |
| rejected | A mimalloc purge delay of 0 or the 2.x line lowers the peak | `memory.peak_private_bytes` | n/a: purge 0 kept 2.85 GB with 60% more faults; 2.3.2 peaked at 2.97 GB | `perf-hdd-plan-main-recheck-ssd` |
| accepted | Writing each group of fetched manifests' part rows with their addon links while later manifests download takes the 433k-row insert off the end of a restore from the verified-hash record | elapsed | lower: record case 9.61 s median of 7 against 11.36 s (-15%), 12.1 CPU s against 13.7; the first manifest lands about 2 s into the fetch and the writer then runs back to back, so the insert stays the tail. Only on a fresh load the record holds entries for, with no links yet; the SSD case (record forgotten by its wipe) keeps the single flush, 19.29 s | `perf-hdd-plan-main-bootstrap-record-hdd`, `perf-hdd-plan-main-recheck-ssd` |
| accepted | Foreign key checks cost about a tenth of the part insert; a parent check inside the transaction covers what they guard | elapsed | lower: `bench_subfiles_checksum_encoding` 3.57-3.75 s against 3.91-4.28 s; record case 9.43-9.61 s against 9.96 s with the checks on. Applied to the streamed groups only | `bench_subfiles_checksum_encoding`, `perf-hdd-plan-main-bootstrap-record-hdd` |
| rejected | Part checksums stored as 32-byte blobs make the insert enough cheaper to pay for a schema bump | deferred insert elapsed | n/a: 3.88-4.05 s against 3.91-4.28 s as 64-char text (about 3%) | `bench_subfiles_checksum_encoding` |
| accepted | Hash jobs that share the tree's part list instead of each copying its file's parts are time-neutral and allocate less | elapsed, `memory.peak_private_bytes` | neutral: SSD 19.29 s and 34.8 CPU s, hash phase 13.36 s against 13.41 s; the peak did not move measurably (2.12 GB SSD, 1.11 GB record), so it is kept for the allocations, not the footprint | `perf-hdd-plan-main-recheck-ssd`, `perf-hdd-plan-main-bootstrap-record-hdd` |
| accepted | Keeping each mod manifest with the server's ETag / Last-Modified and revalidating it makes the metadata rebuild after a database wipe a round of 304s | elapsed | lower: SSD recheck 15.50-15.57 s against 19.28-19.39 s (-20%), "Remote data recheck" 0.85-0.89 s against 4.2-5.5 s with all 96 manifests confirmed; record case 7.13 s against 7.87 s | `perf-hdd-plan-main-recheck-ssd`, `perf-hdd-plan-main-bootstrap-record-hdd` |
| accepted | Manifest downloads need their own concurrency; the SQLite-sized mod task limit (16) makes small manifests queue behind large ones and starts the streamed part insert late | elapsed | lower on the record case: 7.87 s with 64 slots against 9.26 s with 16 (both with the folder check last); neutral on the SSD check, where the insert runs under hashing | `perf-hdd-plan-main-bootstrap-record-hdd`, `perf-hdd-plan-main-recheck-ssd` |
| accepted | The per-mod folder `exists()` inside the fetch slot costs a cold HDD seek each and decides nothing on a fresh rebuild | elapsed | lower: record case 9.26 s with the check last against 9.61 s | `perf-hdd-plan-main-bootstrap-record-hdd` |
| accepted | The UI draws at the display rate through a whole check because egui's spinner, `needs_repaint` and the progress repaint thread each ask for the next frame at once (egui starts a delayed frame one predicted frame early) | UI frames, `memory.cpu_s` | lower: 223 fps before, 50 fps under the testkit (its agent adds 20) and 31.6 fps on the record case after pacing everything through `request_frame_after`; UI thread 1.8 CPU s per SSD check against 4.8, process 31.3 against 35.7 CPU s; elapsed unchanged (15.57 s) | `perf-hdd-plan-main-recheck-ssd` with the `UI frame cost during sync` line |
| accepted | A 256 MiB page cache on the bulk part insert's own connection keeps the 433k-row transaction from spilling dirty pages | deferred insert elapsed | lower: flush 4.35-4.53 s to 3.66-3.72 s, `perf-db-parts-bulk` 9.73 to 8.96 s median of 3, 19.2 to 16.7 CPU s; `bench_subfiles_bulk_knobs` 3.43 to 3.03 s; rows per statement (64-1024) flat | `perf-db-parts-bulk`, `bench_subfiles_bulk_knobs` |
| rejected | Foreign keys off speed the download-overlapped flush in the app as they do in the bench | deferred insert elapsed | lower: 4.34-4.64 s against 4.35-4.53 s; kept only because the bulk connection is retired either way and the parent check replaces them | `perf-db-parts-bulk` |
| rejected | The bulk page cache speeds the streamed part groups too | `remote_refresh.actual_s` on the record case | lower: 7.31 s with it, 7.09 s without, 7.03 s before, all inside the run spread; the groups run at about 11 us a row beside the restore and tree work either way | `perf-hdd-plan-main-bootstrap-record-hdd` |
| accepted | Parsing manifest bytes straight into the typed manifest (no UTF-8 string copy, no newline-stripping copy, no JSON value tree) cuts CPU | `memory.cpu_s` | lower: SSD check 31.3 to 28.6 CPU s, record case 9.4 to 8.5-8.8 CPU s; elapsed unchanged | `perf-hdd-plan-main-recheck-ssd`, `perf-hdd-plan-main-bootstrap-record-hdd` |
| open | Improve persistent quick-scan cache key hit rate | `quick_scan.actual_s` | lower | touch-only quick check |
| open | Use BLAKE3 mmap rayon for large SSD files | `breakdown.run_metrics.hash_total_s` | lower | SSD recheck |
| open | Replace manual compaction with `VACUUM INTO` after the large probe | DB compaction elapsed | lower | large bloated DB probe |
| rejected | Retune Turso 0.7 bulk-write chunks | DB write elapsed | lower | remote refresh |
| accepted | Pooling tuned connections and caching their compiled statements cuts DB time | DB write elapsed | lower | `perf-db-refresh-main` |
| rejected | Turso 0.7 MVCC can beat WAL for Foxy's metadata rebuild | DB write elapsed and correctness | lower, no failures | `perf-db-refresh-main` |
| rejected | MVCC wins once the per-call `journal_mode` pragma is no longer paid (retest on the pooled build) | `summary.total_ms` | lower | `perf-db-refresh-main` |
| accepted | `DB_WRITE_GATE` above 1 is worth ~18% on the metadata refresh | `summary.total_ms` | lower | `perf-db-refresh-main` |
| closed | Improve hash profile auto-selection by storage class | `breakdown.run_metrics.hash_total_s` | n/a, SSD selection is stable and the HDD Conservative/Balanced difference is below the 10% switch guard; the accepted seven-sample cold HDD Auto baseline selected Conservative throughout | `perf-tfr-scifi-first-check-ssd`, `perf-tfr-scifi-hash-profile-{conservative,balanced}-hdd`, `perf-tfr-scifi-cold-auto-hash-hdd` |
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
| rejected | A rotational download profile with 3 concurrent large files, 16 small files and 32 MiB range chunks keeps range writes sequential and shortens the HDD download | `download.sol` | higher | `perf-redownload-small-hdd` |
| accepted | Delta-patched files hand their promotion-time fingerprint to the content-hash refresh, so the post-download pass samples nothing from disk | `run_metrics.content_refresh_files_sampled` | zero | `perf-tfr-scifi-delta-patch-hdd` |
| accepted | Delta insert requests share the full-download range budget and chunk size, and ops larger than a chunk travel as parallel chunks verified from the blob, so a patched file fills the link like a full download does | `summary.download_stage_ms`, `download.sol` | lower, higher | `perf-tfr-scifi-stale-check-ssd` |
| accepted | Each hash calibration profile trials its own disjoint sample group, so a first check hashes every byte once and no trial reads what an earlier trial warmed | `breakdown.run_metrics.hash_work_bytes` | equal to the payload | `perf-tfr-scifi-first-check-ssd` |
| closed | The stale-check lane has a fixed sub-second overhead worth removing | `quick_scan.actual_s` | n/a, measurement artifact | `perf-tfr-scifi-stale-check-ssd` |
| closed | The `perf-startup-arma3-live` memory peak (+13-19 MB over Sept 10) is a code regression worth chasing | `memory.peak_private_bytes` | n/a, advisory by policy | `perf-memory-arma3-live` |
| rejected | The HDD 40-file delta stage drifted from 28.7-30.0 s to 32.3-32.6 s across today's builds because chunked blob writes fragment the staging file on rotational media, not because of payload wear | `summary.download_stage_ms` | n/a, a fresh payload measured 28.4 s (`20260916T164216Z`), so the drift was payload wear; each mutate-and-patch cycle rewrites outputs on rotational media | `perf-tfr-scifi-stale-check-hdd` |
| closed | Persisting the auto hash profile across restarts removes a calibration cost on the first check | `breakdown.run_metrics.hash_total_s` | n/a, evidence says the cost is 0.2-0.3 s on NVMe and nothing on HDD | `perf-tfr-scifi-first-check-ssd` |
| closed | A download after a cancelled force redownload re-fetches every file instead of resuming the 4-5 mods the cancelled run completed and hashed | `summary.downloaded_bytes` of the resume | n/a, the cancel reverts every promoted file, finished mods included, so there is nothing on disk to resume; the reused queue now prunes verified files, but only a policy change can leave them | `perf-tfr-scifi-cancel-resume-ssd` |
| closed | Committing the rollback entries of mods that finished before a cancel (instead of reverting them with the half-done ones) lets the next download resume, at the cost of a repository that is partly new after a cancel | `summary.downloaded_bytes` of the resume | n/a, rollback remains the policy; revisit only if partly updated repositories become an accepted product behavior | `perf-tfr-scifi-cancel-resume-ssd`, `perf-tfr-scifi-cancel-patch-ssd` |
| rejected | Doubling the range budget for the first seconds of a transfer shortens the 4 s ramp (250 MB of deficit against the plateau) when the ramp is per-connection growth at the origin rather than a path-level effect | `download.ramp_s`, `download.ramp_deficit_bytes` | n/a, the calibration lane ramps identically at 96 and 192 connections (3, 8, 16-19, 26-29, 43-48, 62-65, 84-86, 105-106 MB/s per 500 ms) while one connection is at full rate inside 0.5 s: the ramp is the path, not the client | `foxy-testkit calibrate --lanes network --connections 192` against 96, 2026-09-16 |
| accepted | The rollback session rewrites its whole manifest on every promoted file, so an update of many small files pays a cost that grows with the file count | `summary.download_stage_ms` | lower, linear in files | `perf-synthetic-tiny-files-ssd` |
| accepted | A cancelled sync leaves the incremental hash flush running detached, so reverted files keep the checksums of bytes the disk no longer has | oracle after a resume | pass | `perf-tfr-scifi-cancel-patch-ssd` |
| accepted | A cancelled patch attempt is recorded as a fallback, so the next download fetches the whole file instead of applying the plan it still has | `summary.downloaded_bytes` of the resume | lower (the plan bytes) | `perf-tfr-scifi-cancel-patch-ssd` |
| accepted | The integrity recheck runs its hash pass without a cancel receiver, so a cancel lands only after the last byte is hashed | `summary.cancel_quiescent_ms` | lower (seconds, not the whole pass) | `perf-tfr-scifi-cancel-hash-hdd` |
| closed | A cap above the path makes the cap-relative download ratio read as a regression | `download.sol` (cap) beside `download.sol_calibrated` (B1) | n/a, the kit shows both: 0.42 against 2000 Mbps, 0.90 against B1, same 40.6-40.8 s stage | `perf-redownload-small-ssd-limited-above` |
| closed | The MD5 all-core calibration lane (one 256 MiB buffer) bounds an MD5 integrity recheck | `hash.sol_calibrated` | n/a, 400 x 1 MiB files hash at 4.3-4.5 GB/s against the lane's 2.3 GB/s (ratio 1.35-1.40); the single-buffer lane is a floor for per-file parallel MD5, kept as such | `perf-synthetic-md5-check-ssd` |
| closed | Deep profiling (`FOXY_PROFILE`) costs enough on a network-bound download to need its own comparison lane | `summary.download_stage_ms` | n/a, inside noise on this lane | `perf-redownload-small-ssd-profiled` |

The 2026-09-20 full profiled F: run `20260920T170149Z-078ec658` spent
646.323 of 650.928 action seconds in tree hashing; the paired H: run
`20260920T172355Z-0af979c8` passed the same work. F: layout totaled 79.871
worker seconds and 1203.555 read and hash worker seconds, both overlapping
across two workers. Database writes overlapped hashing. Fresh F: unbuffered
sequential calibration was 129.4 MB/s, while live workload disk samples were
about 138-148 MB/s. The PAC1 header-probe candidate still lacks a controlled
wall-time comparison, and the repeated-layout and schedule claims remain
unproven. Do not change parser ownership or worker limits based on the
overlapping worker sums alone.

## Closed entries

### accepted: an append-only rollback journal

`perf-synthetic-tiny-files-ssd` (32,016 files of 8 KiB from the loopback
origin, `20260916T175205Z`) took 822 s for 256 MB: 0.3 MB/s on a path that
serves a 2 GiB file at 1.0 GB/s. The log cadence gave it away, 32 files
every 0.6 s and slower as the run went on: `UpdateRollbackSession` rewrote
its whole manifest (pretty JSON of every entry) on every `prepare_replace`
and `promote_file`, and looked entries up with a linear scan, so the cost per
file grew with the files already promoted. The session now writes the
manifest once as a header and appends one line per change to
`journal.jsonl` (`Register`, `Promoted`, `Restored`, `Committed`), keeps a
path index, and `cleanup_stale_sessions` replays the journal (a torn last
line from a crash is ignored). Same case afterwards (`20260916T203538Z`):
22.6-32.6 s for the same 32,016 files, 25-36x, with the remaining time in
the 293-357k rows of download-progress and hash persistence (10.9-14.1 s of
write windows) and 1,000 incremental hash batches (6.0-10.6 s).

### accepted: cancellation keeps its promises

Three cancel lanes found three ways a cancel left work behind. A cancelled
download rolled its promoted files back while the incremental hash worker's
final flush was still running detached, so reverted files kept the checksums
of bytes the disk no longer had and the next download skipped them
(`perf-tfr-scifi-cancel-patch-ssd` `20260916T175048Z`, oracle: six patched
files wrong). The pipeline now joins the worker instead of aborting it, the
worker skips its final flush once a cancel is pending, and every rollback
clears the local hash baseline of the files it reverted. A cancelled patch
attempt was recorded as `fallback_full`, so the resume fetched the whole
file (2.44 GB for 40 files whose plan needed 424 MB); a cancelled attempt now
puts the plan back to `planned` (`Delta patch cancelled for file_id=`,
counted as `patch_cancelled`). The integrity recheck hashed the whole payload
before noticing the cancel (`perf-tfr-scifi-cancel-hash-hdd`, 26 s to
quiescence on a 31 s pass); it now runs the cancellable hash entry and exits
with `outcome=cancelled`.

### accepted: prune verified files from a reused download queue

`perf-tfr-scifi-cancel-resume-ssd` (`20260916T163019Z`) cancelled a force
redownload 8 s in with 4-5 mods complete and hashed, and the plain download
that followed reused the confirmation-prepared queue as it stood: all 217
files, 4.33 GB moved again. The reuse path now drops every queued file
whose `local_checksum` equals its `remote_checksum` and whose bytes are on
disk at full length (`prepared_queue_prune`, `prune_verified_download_targets`
in `tasks/truncate_download_targets.rs`), and a queue that empties out is
handed to the quick verify instead of failing on "no queue". The first run
of the change exposed the reason the stat is part of the rule: a cancelled
run rolls its promoted files back to their pre-download state, finished mods
included, but the incremental hash had already persisted their checksums, so
162-164 "verified" files were missing on disk and the oracle failed
(`20260916T173221Z`). A rollback now clears the local hash baseline of every
reverted file (`forget_reverted_hashes`, every `restore_all` site), which is
a correctness fix on its own: nothing may trust a checksum the disk no longer
carries. With that in place the resume moved the whole 4.33 GB again
(`20260916T174716Z`, `Reverted 161-165 files after cancel`), which is the
rollback policy at work, not a queue defect; the prune is the half of a
resume the queue can do, the other half is the open policy row above.

### accepted: delta insert requests on the full-download budget

The 40-file delta lane (`perf-tfr-scifi-stale-check-ssd`, 424 MB of insert
bytes) took 16.2-17.4 s on 2026-09-16 while the same origin serves full
downloads at 118 MB/s. The per-file spans in `Delta patch applied
successfully:` summed to 143 s of `download` inside a 16 s stage and the
largest file alone took 13.3 s: `PATCH_PARALLEL_CONCURRENCY = 4` gave every
file four connections at the origin's ~1.2-1.6 MB/s per-connection cap, one
request per 64 MiB run. Sharing the global 96-permit range budget with the
2 MiB run cap took the stage to 11.6-12.7 s (`20260916T111954Z`); files whose
plan is a few multi-megabyte inserts still ran at 4-7 MB/s because a run
cannot split an op whose MD5 is verified from the stream. Chunking those ops
into parallel requests written straight into the blob, with the op verified
from the blob afterwards, took the stage to 6.9-7.5 s with the sampler peak
at 113.6 MB/s (`20260916T113531Z`): 231 run requests plus 129 chunk requests,
`patched_files=40`, `content_refresh_files_sampled=0`, oracle pass. A chunked
op whose blob bytes miss the plan checksum fails the patch and falls back to a
full download exactly as a stream mismatch does.

### accepted: disjoint calibration groups

On `perf-tfr-scifi-first-check-ssd` (`20260916T083021Z`) the auto benchmark
hashed its 707 MB sample three times, once per candidate profile, and picked
Balanced at 9.7 GB/s over Aggressive at 8.5 GB/s: all three trials read the
page cache, so the choice measured warmth and noise. On HDD
(`20260916T081256Z`) the large-part guard left one candidate, so the sample
was hashed once and the 5.6 s trial plus 35.2 s remainder is the disk at
~105 MB/s; the "13.8 s of benchmark" the audit quoted was not in that run's
log. Each profile now trials its own group of the sample (round-robin from
the part-heaviest files), the groups are hashed exactly once and all count
as results, and the selection compares cold data with cold data. Measured
after the change on the same case (`20260916T114141Z`): `hash_work_bytes`
equal to the payload (1.0x, was 1.33x), `hash_total_s` 0.47 s against
0.55-0.60 s; the HDD lane was already single-trial and is unchanged.

### closed: the container format comes from the game, not a file probe (2026-09-21)

Clean seven-sample HDD and five-sample SSD baselines for the full evicted
TFR Main recheck were accepted from `38e52f6` (`20260921T164741Z-0221e59c`,
median 650.985 s [650.381-652.607]; `20260921T181601Z-08b1ec54`, median
31.735 s [29.951-32.280]). `6ee594a` adds `GameModule::content_formats`:
Arma 3 declares PBO, Arma Reforger declares PAC1, and `remote_parts_format_id`
trusts a single declared format instead of opening every archive head to
tell PBO from a gapless PAC1; only an undeclared game still probes. Measured
on clean `b8acc65` against those baselines: HDD `20260921T182557Z-0cd5b248`
median 648.970 s [648.235-650.366] (-0.31%), SSD `20260921T195356Z-1d8e32e0`
median 31.379 s [25.598-32.293] (-1.12%), both `ok` within tolerance with
the same 92,193,872,029 hashed bytes, 3,738 files and 433,063 parts. All
seven HDD candidate rows sit at or below the baseline minimum, which is the
direction one fewer open per archive predicts, but the effect is inside the
12% duration tolerance and the 0.6% run-to-run drift, so it is not a
confirmed improvement and does not explain the notebook's 0.76%. Kept
because the game already answers the question the probe asked. The two
remaining rows above (repeated layout work, two-worker seeking) stay open
for Phase 2 of the local HDD plan; the 92 GB hash already runs at 111% of
the calibrated B2+B6 reference, so either needs evidence of avoidable reads
before a trial.

### closed: stale-check fixed overhead is a measurement artifact

The 0.445 s quoted for the outdated-but-untouched quick check is the runner's
`elapsed_s`, which includes the driver round trips; the app's own
`SOL op=quick_scan actual_s=0.021` and `Quick scan timings: total=20.74ms`
(16 ms of it `tree_part_stats_load`) leave nothing worth a change. Read
sub-second lanes through the app line, as the startup lane already does.

### rejected: a few-files, big-chunks download profile for rotational destinations

The profile came from the 2026-09-13 user bundle, where a 12-file, 96-range
download onto a 7200 rpm disk ran at `sol=0.205`. It reasoned that the disk
was the light and that fewer files in flight with 32 MiB chunks would keep the
range writes sequential. Measured on `perf-redownload-small-hdd` (4.33 GB, 217
files, same origin) it ran the download stage at 125-137 s, `sol` 0.28-0.30,
against 45-58 s and `sol` 0.63-0.81 with the SSD limits on the same disk
(`20260916T051653Z` versus `20260916T053420Z`). Aggregate throughput on this
path is bought with connections (~1.5 MB/s each, see the rejected 96-connection
entry), and the 2026-09-09 HDD rows had already shown the fine 2 MiB grid 20%
faster on spinning media than a coarser one. The bundle's disk cost was the
44 concurrent seek-bound patch applies and the 23 GB hash re-read, both fixed
separately; the network limits were never the problem. `rotational()` now
keeps the SSD network limits and caps only `patch_applies` at 2.

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

### closed: stabilize auto hash selection across disjoint groups (2026-09-19)

The explicit cold NVMe lane `20260918T153343Z-15292830` selected Aggressive and
then Balanced when profile order rotated. The groups differed materially in
bytes and the apparent second-run win was only about 10%, so no profile-choice
win was attributed. `21d9955` keeps the storage heuristic unless another valid
profile is at least 10% faster, and `d561cf9` deals disjoint jobs using a combined
bytes, parts and file-count load score.

The first post-dealing physical attempt, `20260918T201420Z-1d105dfc`, found
severe resource pressure because swap use exceeded 35 GiB. Foxy correctly
reduced the candidates to Conservative and patch concurrency to 5, so that run
cannot validate rotation stability. Repeat the two-sample evicted lane only
under normal pressure and require the same selected profile, sufficient held-out
work, one-pass byte accounting, no flags and an oracle pass. Do not add a
pressure override or increase the switch threshold to manufacture agreement.

The normal-pressure rerun `20260919T075256Z-3af24498` completed on clean
`a0b7a2a`: 64.44 GiB available, zero pagefile use, two distinct rotations,
Aggressive selected both times. Each evicted pass covered 217 files / 4.33 GB
once with zero eviction failures. Hash service was 0.784 and 0.777 s; the
208-file / 2.62 GB held-out work was sufficient and ran at 96.75% and 96.96%
of selected-sample throughput. Both repairs passed the independent oracle and
all rows had zero flags. This closes the NVMe stability check; it does not
establish an HDD profile choice or a five-sample performance baseline.

The HDD check on clean `811447d` used `F:` with normal memory pressure,
explicitly evicted 217 files / 4,331,121,846 bytes before each full integrity
recheck, and compared fixed profiles on the same payload. Conservative
(`20260919T164418Z-01e904ac`) used 31.858 and 31.502 s of hash service;
Balanced (`20260919T164540Z-312983cc`) used 31.158 and 31.174 s. All four
rows hashed the payload once, completed with no flags, and an independent
oracle found zero problems across 3,744 parts. Balanced's roughly 1-2% lead
in this two-run-per-profile probe is below the existing 10% profile switch
guard and is too small to justify changing the HDD heuristic. Keep
Conservative for this large-part workload; revisit only with repeated,
same-state evidence of a larger benefit.

### closed: memory footprint is advisory, not a gate (2026-09-16)

The +13-19 MB startup peak read on 2026-09-16 against the Sept 10 memory lane
came with a changed seeded configuration, and by the resource trade policy in
`conventions/SPEED_OF_LIGHT.md` a footprint move beside unchanged or faster
operations is the accepted price, not a defect. The kit now reports memory
deltas as `advisory-regression` / `advisory-improvement` with a
`memory-advisory` flag and never lets them decide a verdict; the only memory
finding that reopens an entry is unbounded growth (`memory.growth_private_bytes`
continuing across longer UI walks) or a cost paid with no speed in return.

### closed: persist the auto hash profile across restarts (2026-09-16)

With disjoint calibration groups every trial byte is useful work, so what a
persisted profile could still save is the trial's scheduling overhead: 0.22-0.27
s on the NVMe first check (`20260916T083021Z` shipped, 0.39 s total hash time
after the change), and nothing on the HDD lane, where the large-part guard
leaves a single candidate and the sample is hashed once anyway. A persisted
profile would have to be keyed by volume, storage class and CPU, invalidated on
any of them changing and re-benchmarked on the download milestones regardless;
a stale choice on a moved payload costs more than the trial it skips. Not
pursued; reopen only with a workload whose calibration costs seconds.

### closed: default rollback leaves nothing resumable (2026-09-16)

`perf-tfr-scifi-cancel-resume-ssd` (`20260916T162820Z`): cancelling the force
redownload 8 s in reaches quiescence in 563-661 ms (`cancel_quiescent_ms`),
with 4-5 mods complete and 592-667 MB credited. The download that follows
moves the full 4,331,121,846 bytes again (217 files, 40.4-40.7 s): none of
the completed, incrementally hashed files were reused. The cancellation itself
is clean. The reused queue now prunes only files that still match their
persisted fingerprint, but the default rollback restores all promoted files,
including completed mods, so this run has nothing to prune. Keeping completed
mods is the separate open product-policy decision in the table.

### closed: deep profiling overhead on the download lane (2026-09-16)

`perf-redownload-small-ssd-profiled` against `perf-redownload-small-ssd` on
the same build and day: download stage 40.1-40.7 s profiled versus
40.4-41.0 s unprofiled, peak commit 581-583 MB versus 556-608 MB. On a
network-bound lane the per-call timing is inside the noise; the 5-10%
figure in `CASE_FORMAT.md` applies to database- and filesystem-bound lanes
(`perf-db-parts-bulk-profiled`), and profiled rows keep their own case id.
