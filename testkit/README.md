# Foxy test kit

The test kit runs explicit developer-machine UX and performance cases against a
real Foxy executable. It never runs from `cargo test`, CI, hooks, or builds.

The kit is three Rust workspace members:

- `testkit/runner` (`foxy-testkit`), the binary that runs everything below
- `testkit/oracle` (`foxy-testkit-oracle`), the independent payload verifier
- `testkit/mutator` (`foxy-testkit-mutate`), the payload mutator

Requirements:

- Rust 1.96 and the MSVC target used by Foxy
- A case JSON file under the ignored `testkit/cases/` directory
- For GUI cases, an interactive desktop session

Before a `--no-build` performance run, build the release app, debug runner,
and release oracle explicitly: `cargo build -p Foxy --bin Foxy --release`,
`cargo build -p foxy-testkit`, and
`cargo build -p foxy-testkit-oracle --release`. Mutation operations still run
the mutator's Cargo freshness check.

Build it once, then invoke it directly:

```powershell
cargo build -p foxy-testkit
.\target\debug\foxy-testkit.exe --help
```

Run a case:

```powershell
foxy-testkit run --case .\testkit\cases\ux-boot-smoke.json
foxy-testkit run --case .\testkit\cases\perf-repo.json
```

Run a whole suite (a failing case does not stop the rest):

```powershell
foxy-testkit suite --filter "ux-*" --no-build
foxy-testkit suite --tag database --no-build
# the flagship lanes: O1 download and O6 clean recheck (small-ssd), O8 startup; rerun
# them after any change that can touch download, sync or startup paths
foxy-testkit suite --filter "perf-redownload-small-ssd,perf-startup-arma3-live" --no-build
```

A filter is a glob, or several globs separated by commas (any may match), so
a fixed lane list runs as one suite without tagging the case files; tags are
part of the case hash and would start a new ledger history.

Validate a case without building or touching its repository path:

```powershell
foxy-testkit run --case .\testkit\examples\perf-repo.example.json --validate-only
```

Accept a performance baseline only from a clean worktree:

```powershell
foxy-testkit run --case .\testkit\cases\perf-repo.json --accept
```

Compare WAL and MVCC without changing the case workload:

```powershell
foxy-testkit run --case .\testkit\cases\perf-db-refresh-main.json --database-mode wal
foxy-testkit run --case .\testkit\cases\perf-db-refresh-main.json --database-mode mvcc
```

Put the two variants side by side once both have rows:

```powershell
foxy-testkit compare --case-id perf-db-refresh-main
```

A variant is a mode *and* a gate size, so both sides are named explicitly:
`--baseline-gate` / `--candidate-gate` (default 1 each). Pass the same mode on
both sides to compare two gate sizes instead of two engines. `compare` refuses
when either side has no rows.

Sweep the whole matrix over one frozen case, then read it as a curve:

```powershell
foxy-testkit sweep --case .\testkit\cases\perf-db-refresh-main.json --modes wal,mvcc --gates 1,2,4,8
foxy-testkit report --case-id perf-db-refresh-main --op remote-refresh
```

`--db-write-gate 0` leaves `FOXY_DB_WRITE_GATE` unset and records whatever size
the app chose, so the kit measures the shipping configuration; any value from 1
to 8 forces that size instead.

`--database-mode` is recorded in every row and uses a separate baseline. `mvcc`
sets `FOXY_DB_MVCC=1`; `wal` clears it. The runner confirms the effective mode
from Foxy's startup log before trusting a measurement. The shipping write gate
default is `min(4, cpus)`.

`FOXY_DB_POOL_IDLE` is inherited by the app, so exporting `FOXY_DB_POOL_IDLE=0`
before a run turns the connection pool off and gives a same-binary A/B against
the pooled configuration. The shipping idle limit is 6. Every row records
`db_pool_idle` when the variable was set, and comparisons never cross it.

Every run gets its own config directory below
`testkit/runs/<case-id>/<run-id>/config`. The runner refuses `%APPDATA%\Foxy`,
does not copy a database, and never deletes a repository target during cleanup.
Performance cases may intentionally mutate or redownload their configured
target, so review the case before running it. A case that should measure a
rotational disk needs an `evict-cache` operation ahead of the measured one:
the ledger's `cache_state` only records the iteration, and the payload a
`setup_once` download just wrote is otherwise served from the page cache.

Foxy is spawned inside a Windows job object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Graceful shutdown is still attempted
first, so a normal run exercises Foxy's real shutdown path, but no orphan can
outlive the runner even if the runner itself is killed.

## Measuring startup

A case whose operation is `startup` restarts the app and reads the app's own
`SOL op=startup` timeline (`conventions/SPEED_OF_LIGHT.md` O8). Pair it with
`config_seed` so the launch it measures is one against a populated profile:

```json
{
  "config_seed": "C:/Users/<you>/AppData/Roaming/Foxy",
  "operations": [{ "op": "startup", "wait_timeout_s": 600 }]
}
```

The seed is copied into the run's isolated config directory once, before the app
starts, and the live directory is only ever read. Close Foxy first: the copy is
protected by `require_no_other_foxy`, not by a lock. See
[`examples/perf-startup.example.json`](examples/perf-startup.example.json).

## Measuring memory

Every operation is bracketed by a process memory sampler, so memory is a
dimension of every perf row rather than something only a memory case records.
The reduced block lands in `summary.memory` and in the ledger row's `memory`;
the raw series is written to `memory-<iteration>-<operation>.json`.

`perf-memory-arma3-live` is the dedicated case: it seeds the live configuration,
then measures launch to sync verdict, a recheck of the largest repository, a
`ui-walk` through the views a user actually opens, and a no-change sync.

```powershell
foxy-testkit run --case .\testkit\cases\perf-memory-arma3-live.json --no-build
```

Read private commit, not working set: the OS trims resident pages under
pressure, so only commit moves when an allocation regression lands. To separate
Foxy's own state from the renderer floor, launch once against an empty
`--config-dir` and subtract - that floor is not Foxy code and moves with eframe,
wgpu and the graphics driver. `conventions/SPEED_OF_LIGHT.md` M1 has the
equation; `conventions/SPEED_OF_LIGHT_MEASUREMENTS.md` has the levers and the
recorded floor for this machine.

## Deep profiling

A case with `"profile": true` runs Foxy with `FOXY_PROFILE=1`, and the run gains
`profile-<iteration>-<operation>.json`: a phase timeline, every statement through
the database seam split read/write and attributed to its phase, the instrumented
filesystem calls, and the app's own `PIPELINE SUMMARY` table parsed into rows.

The same profiler is what the "Extended diagnostics logging" application setting
turns on for users, together with debug-level records and periodic `RESOURCES`
samples; a user log with that setting on carries the `PROFILE` lines a
profiled case does. Per-call timing can cost roughly 5-10% on database- and
filesystem-bound lanes and may be inside noise on a network-bound lane. Measure
per lane; profiled and unprofiled rows are never comparable. Keep the profiled variant under its own case id
(`perf-db-parts-bulk-profiled` beside `perf-db-parts-bulk`) rather than toggling
the flag inside one history. See `CASE_FORMAT.md` for the field and for what the
filesystem instrumentation does and does not cover.

## Measurement table

`measurements` renders the curated table for
`conventions/SPEED_OF_LIGHT_MEASUREMENTS.md` from structured records rather
than hand-typed numbers: for every case ledger, the latest valid run reduced to
one line per operation lane (label, cache lane), with the SoL and its kind,
median elapsed, the accepted baseline it compares with (or `legacy`, `expired`,
`no lane`), the work counters, outcome, build and run id:

```powershell
foxy-testkit measurements                       # every case, Markdown
foxy-testkit measurements --filter tfr-scifi    # cases whose id contains the text
foxy-testkit measurements --json
```

Paste the rows that change a decision into the measurements file; keep raw
artifacts in `testkit/runs/`.

## Calibration

`calibrate` measures the independent references a row's calibrated ratios
cite (`conventions/SPEED_OF_LIGHT.md`, Baselines B1, B2/B3, B4, B6) and keeps
them in `testkit/ledger/calibration.json`, one entry per lane with a dated id
and the environment fingerprint:

```powershell
foxy-testkit calibrate --case .\testkit\cases\perf-redownload-small-ssd.json
foxy-testkit calibrate --case .\testkit\cases\perf-tfr-scifi-stale-check-hdd.json --lanes disk
foxy-testkit calibrate --case ... --lanes network --seconds 30 --connections 96
```

`network` loads the origin with 2 MiB range requests over its largest files
(one connection, the 8/24/48 curve, then the request budget) and records the
sustained plateau rate; `latency` records connect, fresh and reused
`repo.json` request times; `disk` writes, unbuffered-reads and warm-reads a
1 GiB file beside the case's repository path, single-reader and one reader
per core (so run it once per volume); `hash` measures compute-only BLAKE3 and
MD5; `metadata` enumerates the repository tree twice (first pass and warm
entries per second, per volume); `db` inserts and deletes 200k part rows in a
throwaway Turso database on the case volume, with a keyed-update pass in
between (insert, update and delete rows per second at gate 1, per volume);
`hosts` records connect, fresh and reused request times for any list of URLs
(`--hosts https://a/repo.json,http://b/repo.json`, the case address when
omitted), HTTPS included, one entry per host under `lanes.hosts`. Run the
network lane with nothing else on the path. Every
later run selects the lanes that match it and derives `sol_calibrated` on
the row (see `LEDGER_FORMAT.md`); recalibrating retires the baselines that
cited the old lanes.

## Replay

`replay` rebuilds ledger rows from artifacts an earlier run already wrote, in
milliseconds and without running Foxy at all. It is the fastest way to verify a
change to a parser, a metric, a threshold, or a ledger field, and it checks that
change against every recorded run rather than one fresh one:

```powershell
foxy-testkit replay .\testkit\runs\perf-db-refresh-main\20260909T045957Z-07cbda32
foxy-testkit replay --all
```

`--all` exits non-zero if any run's rebuilt row differs from what was recorded,
excluding `case_hash` and timestamps. UX runs have no rows and are reported as
skipped.

## Local origin

A case may serve its own repository from `testkit/origin/data/<name>` instead
of depending on a remote server whose published payload can change between
runs. Build a mirror with:

```powershell
foxy-testkit mirror --upstream http://host:8080/mody/Repo/ --output .\testkit\origin\data\my-mirror --repo-name "My Mirror" --force
```

The default mirror is manifests only (`repo.json`, `foxy_addons.json`, and each
mod's `foxy_addon.json`). That is the whole input to a metadata rebuild, so the
database lane gets every part row the real repository would produce for a few
MB on disk. Pass `--payload` when a case needs real bytes. Generate a row-heavy
origin instead with `foxy-testkit synthetic`.

`synthetic` writes flat `.bin` files by default, which produce one manifest part
each. Pass `--pbo-entries N` to write PBOs of `N` entries instead: the repository
generator splits a PBO into one part per entry, so part rows scale with entries
rather than with payload size. `--entry-bytes` sets the payload behind each
entry.

```powershell
foxy-testkit synthetic --output .\testkit\origin\data\synthetic-parts --mods 96 --files-per-mod 8 --pbo-entries 562 --entry-bytes 64 --force
```

That is 433 248 part rows in 42 MB, matching a real 96-mod repository row for row
while moving three orders of magnitude fewer bytes. It is what
`perf-db-parts-bulk` uses to exercise the deferred bulk part insert, which is the
largest single write Foxy performs.

Large files are written in 8 MiB pieces, so `--file-bytes 2147483648` is fine,
and `--mode swifty` makes the generator publish the legacy MD5 layout
(`mod.srf` per mod, no `foxy_addon.json`) for a hash-algorithm lane. The
distribution and algorithm cases document their generator lines in `notes`:
`synthetic-tiny` (16 x 2000 x 8 KiB), `synthetic-large` (one 2 GiB file) and
`synthetic-md5` (8 x 50 x 1 MiB, swifty mode). The oracle reads FoxyMode
manifests only, so an MD5 case keeps the `expected_files` guard instead.

An `origin` block can also carry `delay_ms`, which holds every response back
that long: with an `extra_repositories` entry marked `"unreachable": true`
(a closed port, no manifest probe) that is the adverse-origin startup graph
(`perf-startup-adverse-origins`).

Two repositories that publish the same addons (a repository space, or two
standalone repositories installed into one folder) are two copies of one
origin under one root. `perf-sibling-shared-folder-check` serves
`testkit/origin/data/sibling-synthetic/` with `repo-a/` and `repo-b/`, both a
plain copy of `synthetic-writes`:

```powershell
Copy-Item -Recurse .\testkit\origin\data\synthetic-writes .\testkit\origin\data\sibling-synthetic\repo-a
Copy-Item -Recurse .\testkit\origin\data\synthetic-writes .\testkit\origin\data\sibling-synthetic\repo-b
```

The case downloads `repo-a`, then checks and updates `repo-b` through the
operation-level `repository` field; the second repository must come out clean
with no hashing and no failed pipeline.

A case declares `"origin": {"root": ..., "port": ...}`; the runner serves it
in-process for the life of the run, so there is no origin process to leak. Serve
one by hand with `foxy-testkit origin --root <dir>`, and measure the server
itself with `foxy-testkit origin bench`.

A FoxyModeV1 repository publishes its mod list in `foxy_addons.json`.
`repo.json`'s `requiredMods` / `optionalMods` are the legacy Swifty path and
are normally empty, so anything that reads a mod list from `repo.json` alone
will see an empty repository.

Real cases, ledgers, baselines, and run artifacts are ignored because they can
contain machine paths, server credentials, and large logs. Copy an example into
`testkit/cases/`, replace placeholders locally, then run it.

The GUI driver injects discrete input events. A clean scroll case is useful
evidence, but it cannot prove that interactive hover, momentum, and DPI paths are
free of every egui multi-pass issue.

## Kit correctness

The kit's own gates run from `cargo test -p foxy-testkit -p foxy-testkit-oracle`:

- `tests/oracle_independence.rs` walks `cargo metadata` and asserts every crate
  the oracle resolves to comes from a registry, so the oracle can never share an
  implementation with the code it verifies. `cargo-deny` cannot express this
  because its bans check skips path dependencies.
- `src/collect/snapshots` pins every log parser with `insta` against real
  recorded slices in `tests/log-corpus/`. Refresh with `cargo insta review` only
  when the log format changed on purpose.
- `tests/driver-corpus/` holds real `agent-gui` responses. The kit mirrors the
  driver types rather than importing them, so a Foxy change that breaks the wire
  contract fails here instead of compiling clean. Refresh the corpus in the same
  commit as a deliberate driver change by setting
  `FOXY_TESTKIT_DRIVER_CAPTURE=<dir>` for a run and copying the envelopes in.

See [CASE_FORMAT.md](CASE_FORMAT.md), [LEDGER_FORMAT.md](LEDGER_FORMAT.md), and
[OPTIMIZATION_LOOP.md](OPTIMIZATION_LOOP.md).
