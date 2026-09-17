---
name: foxy-testkit
description: "Run Foxy's developer-machine test kit: UX click-through cases through the agent GUI driver, and downloader/checker/database performance cases with an append-only ledger, baselines, regression verdicts, and WAL versus MVCC comparison."
argument-hint: "[case name, suite filter, or 'perf'/'ux']"
---

# Foxy test kit

The test kit runs explicit, developer-machine cases against a real `Foxy.exe`.
Nothing here runs from `cargo test`, CI, a hook, or a build. Every entry point is
a subcommand of `foxy-testkit` that the user or an agent invokes on purpose,
because these cases need real disks, a real network or origin, and (for the UX
lane) a real window.

The kit is Rust: `testkit/runner` (`foxy-testkit`), `testkit/oracle`
(`foxy-testkit-oracle`), and `testkit/mutator` (`foxy-testkit-mutate`). There is
no PowerShell and no Python in it.

Read `testkit/README.md`, `testkit/CASE_FORMAT.md`, and `testkit/LEDGER_FORMAT.md`
before changing a case or the runner. For an optimization loop, follow
`testkit/OPTIMIZATION_LOOP.md` and `testkit/HYPOTHESES.md` exactly.

## Before running anything

- Close any other running `Foxy`. One process owns a game space database; the
  `require_no_other_foxy` guard fails the run rather than fighting over it.
- Build once, then pass `--no-build` so iterations do not pay for a `cargo build`
  no-op check: `cargo build -p Foxy --bin Foxy --release`,
  `cargo build -p foxy-testkit`, and `cargo build -p foxy-testkit-oracle --release`.
  Mutation operations still perform the mutator's Cargo freshness check.
- Perf cases are `release` by default. Never compare a debug row to a release
  row, a GUI row to a CLI row, or a WAL row to an MVCC row except through
  `foxy-testkit compare`.

## Running

```powershell
# one case
.\target\debug\foxy-testkit.exe run --case .\testkit\cases\ux-boot-smoke.json --no-build

# validate a case without launching anything
foxy-testkit run --case .\testkit\cases\perf-db-refresh-main.json --validate-only

# a suite (a failing case does not stop the rest)
foxy-testkit suite --filter "ux-*" --no-build
foxy-testkit suite --tag database --no-build

# database variants of the same frozen case
foxy-testkit run --case .\testkit\cases\perf-db-refresh-main.json --database-mode wal  --no-build
foxy-testkit run --case .\testkit\cases\perf-db-refresh-main.json --database-mode mvcc --no-build
foxy-testkit compare --case-id perf-db-refresh-main

# the whole mode/gate matrix, then read it as a curve
foxy-testkit sweep --case .\testkit\cases\perf-db-refresh-main.json --modes wal,mvcc --gates 1,2,4,8
foxy-testkit report --case-id perf-db-refresh-main --op remote-refresh
```

Artifacts land in `testkit/runs/<case-id>/<run-id>/`: `app.out`, `app.log`,
`summary.json`, `sol.jsonl`, `collected-*.json`, `operation-*.log`,
`breakdown-*.json`, the resolved case, and the fixture actually written. Ledger
rows append to `testkit/ledger/<case-id>.jsonl`.

## Flagship lanes and the measurement table

After any change that can touch download, sync or startup paths, rerun the
flagship lanes and read the correctness counters and memory guardrails on the
same rows (`conventions/SPEED_OF_LIGHT.md`, Maintenance):

```powershell
foxy-testkit suite --filter "perf-redownload-small-ssd,perf-startup-arma3-live" --db-write-gate 4 --no-build
```

`compare` retires a baseline whose run profile (including `diagnostics`),
environment (CPU, OS, memory bucket, origin), origin checksum or cited
calibration lanes differ from the current row (`rebaseline-required`,
`profile-mismatch`, `origin-changed`, `reference-changed`); re-accept on a
clean revision rather than reading across. Memory metrics are advisory: a
`memory-advisory` flag reports a footprint move and never sets the verdict
(Foxy trades footprint for speed on purpose). The curated table in
`conventions/SPEED_OF_LIGHT_MEASUREMENTS.md` is rendered from the latest
valid run per case with `foxy-testkit measurements`; paste rows, do not type
them.

Calibrated ratios need the reference lanes once per machine, origin and
volume:

```powershell
foxy-testkit calibrate --case .\testkit\cases\perf-redownload-small-ssd.json
foxy-testkit calibrate --case .\testkit\cases\perf-tfr-scifi-stale-check-hdd.json --lanes disk
```

Rows then carry `references` and `download.sol_calibrated`,
`hash.sol_calibrated`, `startup_probe.sol_calibrated`, `quick_scan.sol_calibrated`
and the no-change `sync_action.sol_calibrated`; DB references remain evidence
only until action counters separate statement kinds. The app's own `sol` stays the peak-consistency
figure. `--lanes hosts --hosts <url,...>` records B4 per host (HTTPS included) for a multi-host startup graph. Run the network lane with the path otherwise idle; rerun the lanes
after a hardware, OS, network or origin change.

## Changing the kit itself

Do not run a case to verify a change to a parser, a metric, a threshold, or a
ledger field. `foxy-testkit replay --all` re-derives rows from every recorded run
in milliseconds and exits non-zero on any difference from the recorded row:

```powershell
foxy-testkit replay --all
foxy-testkit replay .\testkit\runs\<case-id>\<run-id>
```

Run a fresh case only once the derivation is right. The kit's own correctness
gates are `cargo test -p foxy-testkit -p foxy-testkit-oracle`: oracle
independence, `insta` parser snapshots over `tests/log-corpus/`, and the driver
response corpus in `tests/driver-corpus/`.

## Reading a failure

- UX case: open `summary.json`. Each step records `ok`; a failing step carries
  the driver payload, and `unexpected_logs` holds any WARN/ERROR in the case's
  own log slice that `allow_warnings` did not cover.
- Perf case: the row's `flags` say why a run was invalidated
  (`db-wipe-detected`, `incomplete-payload`, `oracle-failed`, `runaway-bytes`,
  `threshold-failed`, `assertion-failed`, `outcome-mismatch`). `breakdown-*.json` localizes a
  regression to network, disk, database, hash, scan, or patch.

## Measuring startup

`"op": "startup"` restarts the app inside the run and waits for the
`startup-sync` busy reason to clear, then reads the app's `SOL op=startup` line
for the timeline (first frame, dispatch, eligibility, verdict). Give the case a
`config_seed` pointing at a real Foxy configuration directory so it measures a
populated profile rather than a first-run bootstrap; the source is read-only and
`require_no_other_foxy` is what keeps the copy consistent, so close Foxy first.

Read `startup` rows through the app's line, not the runner's `elapsed_s`: the
latter includes the driver's readiness polling and is quantized to it.

## Measuring memory

Every operation is bracketed by a process memory sampler, so a footprint
regression lands on a download row as readily as on a memory case's. Read
`memory.peak_private_bytes` and `memory.retained_private_bytes` from the row;
`memory-<iteration>-<operation>.json` in the run directory has the raw series,
which is what distinguishes a one-time cache fill from a leak.

`perf-memory-arma3-live` is the dedicated case: startup, a recheck of the
largest repository, a `ui-walk` through the views a user actually opens, and a
no-change sync, all against a seeded live configuration.

Private commit is the number, not working set: the OS trims resident pages under
pressure. To separate Foxy's own state from the renderer floor, launch once
against an empty `--config-dir` and subtract; that floor is eframe, wgpu and the
graphics driver, and it moves when they do.
`conventions/SPEED_OF_LIGHT.md` M1 carries the equation; the levers and the recorded floor are in `conventions/SPEED_OF_LIGHT_MEASUREMENTS.md`.

## The local origin

`testkit/origin/data/` holds repository mirrors served on loopback so a perf case
is not measuring someone else's server on a bad day.

```powershell
foxy-testkit mirror --upstream http://host:8080/mody/Repo/ --output .\testkit\origin\data\my-mirror --repo-name "My Mirror" --force
foxy-testkit synthetic --output .\testkit\origin\data\synthetic --mods 32 --files-per-mod 500
```

`synthetic` writes flat `.bin` files, one manifest part each. `--pbo-entries N`
writes PBOs of `N` entries instead, and the generator splits each PBO into one
part per entry, so part rows scale with entries rather than payload bytes. That
is how `perf-db-parts-bulk` gets 433 248 part rows out of 42 MB.

By default `mirror` mirrors manifests only (`repo.json`, `foxy_addons.json`, and
each mod's `foxy_addon.json`), which is what the metadata-rebuild and database
lanes exercise: tens of megabytes instead of tens of gigabytes, and every part
row the real repository would produce. Add `--payload` when a case needs real
bytes for a download or delta-patch run.

A case's `origin` block is served **in-process** for the life of the run by
`tower-http`'s `ServeDir` (byte ranges, `HEAD`, `416`, traversal refusal), so
there is no origin process to leak. Serve one by hand with
`foxy-testkit origin --root <dir>`, and measure the server itself with
`foxy-testkit origin bench`.

Note for any manifest work: a FoxyModeV1 repository publishes its mod list in
`foxy_addons.json`. `repo.json`'s `requiredMods`/`optionalMods` are the legacy
Swifty path and are normally empty, so a mod list read from `repo.json` alone
will look like an empty repository.

## Deep profiling

A case with `"profile": true` sets `FOXY_PROFILE=1` and the run gains
`profile-<iteration>-<operation>.json`: phases, every seam statement split
read/write and attributed to a phase, instrumented filesystem calls, and the
parsed `PIPELINE SUMMARY`. It can cost 5-10% on DB/filesystem-bound lanes, so a profiled case
carries its own id and its own history; use it to find where time goes, and the
unprofiled twin to prove a change moved it.

## The independent oracle

`foxy-testkit-oracle` verifies a synced payload against the repository's
published manifests with an implementation that is not the one under
optimization: a separate process, its own manifest fetch over HTTP, its own
byte-range extraction, and **zero in-repo dependencies**, enforced by
`testkit/runner/tests/oracle_independence.rs`. Wire it into a case as
`guards.oracle_command`, which is an argv array; a non-zero exit invalidates the
row.

It compares **part** checksums. A manifest's per-file `checksum` is a rollup
over that file's parts, so a whole-file digest will mismatch every file and mean
nothing; the per-part `checksum` values are plain BLAKE3 over the byte range.
`--structure-only` degrades it to a presence-and-length check - do not read that
as a content pass.

## Rules that keep the numbers worth having

- One process, one case, sequentially. Two perf cases at once measure each other.
- Never edit a case during an optimization loop. `case_hash` is recorded per row
  and a changed hash starts a new history instead of continuing an old one.
- Iteration 0 is the unprepared first pass and is ledgered separately from the warm median.
  Its historical `cold` label is only the iteration number: a payload the run just downloaded or
  mutated is still in the OS page cache, so an HDD case measures memory unless
  it carries an `evict-cache` operation (a non-cached open of every payload
  file, Windows only) before the operation that should read the disk.
- A run that moved fewer bytes, files, or parts is not faster, it is wrong.
- `database.write_time_ms` and the per-category `txn_ms` behind it are gated
  transaction windows, not row cost, and are meaningless across write-gate
  sizes: Turso has one internal writer, so above gate 1 the waiters block
  inside `conn.execute` and that queue time is charged to the window rather
  than to `permit_wait_ms`. The same rows measure ~72 ms at gate 1 and
  570-1 150 ms at gate 8. Gate 1 is the uncontended reference; the logs
  carry `write_gate=`. `summary.total_ms` is operation elapsed and unrelated.
- `--accept` writes a baseline and refuses a dirty worktree.
- Never point a case at `%APPDATA%\Foxy`; the runner refuses it, and cases use a
  per-run isolated config directory.
