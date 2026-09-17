# Test kit case format

Cases are UTF-8 JSON objects. Real cases belong in the ignored `cases/`
directory. `${REPO_ROOT}`, `${RUN_DIR}`, and `${ENV:NAME}` placeholders are
expanded in string values before execution. The resolved case is stored only in
the ignored run directory.

## Common fields

| Field | Required | Default | Values |
| --- | --- | --- | --- |
| `id` | yes | none | kebab-case string |
| `kind` | yes | none | `ux` or `perf` |
| `description` | no | empty | string |
| `enabled` | no | `true` | boolean |
| `tags` | no | `[]` | string array |
| `build` | no | perf: `release`, UX: `debug` | `debug` or `release` |
| `harness` | no | `gui` | `gui` or `cli` |
| `timeout_s` | no | `3600` | positive integer |
| `config_dir` | no | run-local `config` | isolated path, never the live Foxy root |
| `config_seed` | no | none | directory copied into `config_dir` before the fixture |
| `fixture` | no | generated from `repository` | agent-gui fixture object |
| `origin` | no | none | `{root, port}` local origin served for the run |
| `profile` | no | `false` | boolean; sets `FOXY_PROFILE=1` for the run |
| `notes` | no | empty | Free text kept with the case; ignored by the runner |

The generated fixture fetches `foxy_addons.json` (falling back to `repo.json`)
to enable the repository's published required and optional addons, and is
written straight into the isolated config directory in the legacy flat layout
so Foxy's one-shot game-space migration converts it on first start. Supply
`fixture.files` to use an exact fixture instead; only `settings.json`,
`repositories.json`, and `repository_spaces.json` are accepted.

`config_seed` copies a real Foxy configuration directory into the isolated
config directory, so a case can measure an already-populated profile - real
repositories, real spaces, a warm database - instead of a bootstrap. The source
is only read; `database.lock`, `database.owner`, `logs/` and `backups/` are
skipped because they belong to whichever app instance is running, and
`guards.require_no_other_foxy` is what keeps the copy from being torn by a live
app writing into it. A seeded case needs no fixture: supplying one anyway would
write the flat legacy layout over the seed and trigger the game-space migration
on the very state the case is measuring, so fixture generation is skipped unless
the case sets `fixture.files` explicitly. The copied byte count is recorded in
`config-seed.json` in the run directory.

When `origin` is present the runner serves `origin.root` on `origin.port`
(loopback, byte-range capable) for the duration of the case, and a perf case
additionally refuses to start when the repository's manifest publishes no
mods.

`profile` turns on the app's deep profiler. The run then emits `PROFILE ...`
lines that the kit parses into `profile-<iteration>-<operation>.json`:

- `totals`: wall clock, phase-covered time, the time no phase claims, and
  call/row/byte counts for database reads, database writes and filesystem calls
- `phases`: `purge`, `pre-download`, `download`, `hash`, `finalize`
- `db`: one row per (phase, read/write, `verb table`) with calls, rows returned
  or affected, total time and the slowest single call
- `fs`: one row per (phase, operation) with calls, bytes or entries, total time
  and the slowest single call
- `stages`: the app's own `PIPELINE SUMMARY` table, parsed; present whether or
  not profiling was on

Per-call timing can cost roughly 5-10% on database- and filesystem-bound
lanes, while a network-bound lane may show no separable cost. Measure it per
lane, and never compare a profiled row with an unprofiled one. Keep a profiled case under its own id, as
`perf-db-parts-bulk-profiled` does, rather than turning the flag on and off
inside one case history. Profiling locates cost; the unprofiled twin tracks it.

Filesystem coverage includes download transfer and range paths, hashing reads,
addon folder scans, positional read/write helpers, and file removal. Metadata
refresh and local-path preflight also report `exists`, `metadata`, `read_dir`,
and `read_dir_next`. Simple-download buffered flushes report `flush` separately
from writes into the buffer. This is targeted coverage, not a process-wide
filesystem trace; uninstrumented calls remain outside the filesystem totals.

## UX fields

| Field | Default | Meaning |
| --- | --- | --- |
| `steps` | required | Driver command objects accepted by `agent-gui scenario` |
| `screenshots` | `on-failure` | `none`, `on-failure`, or `each-step` |
| `stable_render` | `true` | Enables deterministic rendering before the scenario |
| `allow_warnings` | `[]` | WARN/ERROR message substrings allowed for this case |

A UX case passes only when the scenario passes and its bracketed log slice has
no unallowlisted WARN or ERROR entries.

## Performance fields

| Field | Required | Default | Meaning |
| --- | --- | --- | --- |
| `repository` | yes | none | `{name,address,path,space_id}` |
| `extra_repositories` | no | `[]` | Further `{name,address,path,space_id}` entries written after `repository`; an entry with `"unreachable": true` skips the manifest probe and gets no addons (an offline host in a startup graph) |
| `space` | no | `null` | Repository-space fixture object |
| `operations` | yes | none | Ordered operation objects |
| `repetitions` | no | `3` | Recorded passes |
| `warmup` | no | `false` | One unrecorded pass before recorded passes |
| `metrics` | no | all | Dotted metrics included in reports |
| `thresholds` | no | `{}` | Per-metric `{min,max}` hard gates |
| `guards` | no | defaults below | Precondition object |
| `settings` | no | none | object merged into the generated `settings.json` (a bandwidth cap, a hash profile); ignored with `fixture.files` or a `config_seed` |
| `origin` | no | none | `{root, port}` served in-process for the run; `delay_ms` holds every response back that long (an adverse-origin lane) |

Supported operations are `startup`, `ui-walk`, `switch-game-space`,
`remote-refresh`, `quick-check`, `recheck`, `recheck-integrity`,
`force-redownload`, `download`, `wipe-db`, `mutate`, `restore`, and
`evict-cache`. Each operation may contain `wait_timeout_s`,
`expect`, `label`, and `repository`. `repository` names the fixture repository
the operation acts on (the CLI `--repo-name`, or the GUI row with that name);
it defaults to the case `repository`. `extra_repositories` is what puts a
second repository into the fixture, so a case can download one repository and
then check its sibling in the same folder. Mutation, the oracle, and the
manifest probe always address the case `repository`. Setup operations are not ledgered. `label` names the
operation's artifacts and its ledger row (`label` field) when a case repeats
one kind of operation inside an iteration, such as a quick check run twice to
prove the second one reads nothing; without it the second operation's
artifacts overwrite the first.

The two harnesses run the same mode through different entry points, and the
difference matters for the sync-path cases: the GUI `remote-refresh` is the
toolbar recheck, which also prepares the download queue so the following
`download` reuses it (`run_metrics.prepared_queue_reuses`), while the CLI
`remote-refresh` is `repo sync --mode remote-refresh` and prepares nothing. The
GUI `quick-check` is the toolbar quick check and `wipe-db` is the repository's
"wipe database entries" action (busy reason `repository-db-wipe`).

GUI sync operations handled by the common download/check/refresh/wipe path may
carry `ui_probe_ms`: a frame probe then polls the
app's `fps` intent at that cadence for the whole operation (the probe keeps
the app repainting, so frame intervals are real) and the row records
`summary.ui_probe`: `frame_ms_max` and `frame_ms_p95_worst` (the worst
figures the app reported from its last 240 frames across the reads),
`fps_min`, and the probe's own `rtt_ms_p50` / `rtt_ms_p95` (process spawn
included, so an upper bound on input latency, not a frame time). Expect on
`summary.ui_probe.frame_ms_max` to fail an operation that bought its
throughput by blocking the UI thread.

`run_metrics` also carries the redundant-work counters the checker cases
assert on: `hash_work_bytes` (bytes read by every `SOL op=hash` run in the
operation), `tree_verify_runs` (targeted tree-hash verifies the quick scan
triggered), `fs_watcher_starts`, and `prepared_queue_reuses`. Expectations
reach them as `breakdown.run_metrics.<name>`.

The hash-source counters say where an update's hash bytes came from and when
they were paid: `content_refresh_runs` and `content_refresh_files_sampled`
(`Content-hash baseline refreshed` passes, and the files they had to sample
from disk because no hash pass in the operation fingerprinted them),
`hash_source_segments_files` and `hash_source_reread_files` (delta-patched
files recorded from their apply segments versus re-read),
`hash_batches_after_download` and `final_hash_flush_files` (incremental hash
work that ran after `Download stage completed`, so an update whose hashing
stopped overlapping the transfer is visible), `download_large_files_limit`,
`download_small_files_limit` and `download_patch_applies_limit` (the profile
the destination's storage class selected), and `first_download_start_ms`
(first timestamped event of the slice to the first `Starting download for
mod`, the time an update spent preparing before the first byte).

The pipeline's own verdict is there too: `pipeline_outcome` is the outcome of
the last `Pipeline summary` line in the operation (`early-exit-clean`,
`failed-empty-queue`, ...) and `failed_pipelines` counts the `failed-*` and
`cancelled` ones. A sync that fails still clears its busy reason and returns a
summary, so a sync-path case must assert `failed_pipelines` equals 0 or it
passes on a failure the user would have seen as an error dialog.

`ui-walk` runs an array of driver `steps` - the same command objects a UX case
uses - as one measured operation, and requires the GUI harness. A UX case
answers "did the scenario pass"; a `ui-walk` answers "what did walking the app
cost", which is a perf question and belongs on a perf row. The row records
`summary.total_ms` and `summary.steps`, and the operation fails if any step
fails or if fewer steps ran than the case listed. The scenario transcript is
written to `ui-walk-transcript.json` in the run directory.

`startup` measures O8, launch to sync verdict. It stops the running app, starts
a fresh one, and waits for the `startup-sync` busy reason to clear, which the app
holds from the moment startup work is dispatched until the last repository has a
verdict. It requires the GUI harness, and it fails when the run produced no
`SOL op=startup` line, because that means the app never reached a verdict. The
app's own line is the authoritative timeline; the runner's `elapsed_s` is only
the outer bracket and includes the driver's readiness polling. The row carries:

| field | meaning |
| --- | --- |
| `startup_total_ms` | process start to the last repository's verdict |
| `first_frame_ms` | process start to the first painted frame |
| `dispatch_ms` | process start to startup work being dispatched |
| `eligibility_ms` | dispatch to the eligibility plan landing |
| `verdict_ms` | dispatch to the last repository's verdict |

Because the operation restarts the app, the config directory persists across
iterations: iteration 0 is the unprepared first pass and later iterations are
warm. The ledger retains the historical `cold` label for that first pass, but
only an explicit `evict-cache` operation establishes a device-cold read lane.

## Memory

Every operation is sampled from outside the process, so a footprint regression
shows up on a download row as readily as on a memory case's. The sampler reads
`PrivateUsage` and `WorkingSetSize` every 100 ms for the pid the run is driving
(the GUI child, or the `Foxy.exe` invocation for a CLI operation), and reduces
the series into `summary.memory`, mirrored into the ledger row as `memory`:

| field | meaning |
| --- | --- |
| `peak_private_bytes` | highest private commit seen during the operation |
| `retained_private_bytes` | lowest commit in the quiet window after it |
| `growth_private_bytes` | retained minus the operation's first sample, floored at zero |
| `transient_private_bytes` | peak minus retained: what the operation borrowed |
| `median_private_bytes`, `peak_working_set_bytes`, `retained_working_set_bytes`, `process_peak_working_set_bytes`, `page_faults`, `samples`, `span_ms` | supporting detail |

Private commit is the gated metric because the OS trims working set under
pressure: commit moves when an allocation regression lands, resident pages move
when another process wants memory. `memory.peak_private_bytes` and
`memory.retained_private_bytes` carry a 15% tolerance and
`memory.growth_private_bytes` 50%, because footprint moves in allocator-sized
steps rather than in percent.

A settle window runs after the operation's own work and before the watch stops,
which is what separates "peaked here" from "still holding it"; it defaults to
1500 ms and an operation may set `memory_settle_ms`. It is charged after
`elapsed_s` is taken, so it does not enter any timing metric. The raw series is
written to `memory-<iteration>-<operation>.json` beside the other artifacts and
stripped from the ledger row.

A `startup` operation replaces the app mid-watch; the sampler notices the pid
change and discards the outgoing process's samples, so `start_private_bytes` for
a startup row is a fresh process rather than the one being replaced. For a CLI
operation the process exits at the end, so `retained_private_bytes` is the last
live sample and only `peak_private_bytes` is meaningful.

An expectation is `{path, equals}`, `{path, min}`, `{path, max}`, or
`{path, between:[low,high]}`. Dotted paths resolve against the collected result.
A numeric gate (`min`, `max`, `between`) fails when the value is missing or not
a finite number; add `optional: true` when the metric may legitimately be
absent for that operation and the gate should then be skipped.

The expectation view exposes the last aggregate patch action as `delta_patch`
and its typed per-file stage records as `delta_patch_stages`, in addition to
`summary`, `sol`, `breakdown`, and `elapsed_s`. Patch cases should assert the
aggregate outcome, conservation state, and fallback or cancellation counters.

The `mutate` operation adds `profile`, `seed`, `files`, `entries`, `bytes`,
optional `path` and optional `preserve_mtime`. `path` defaults to the
repository target. `restore` replays the run journals in reverse; several
`mutate` operations in one iteration each get their own journal. Supported
profiles are documented by `foxy-testkit-mutate --help`; `scattered` spreads
`entries` changes across `files` files, `adjacent` puts one contiguous run of
`entries / files` changed entries in each file (the locality counterpart).
`preserve_mtime` puts every mutated file's modification time back afterwards,
so a size-and-mtime fingerprint cannot see the change: that is how a prepared
patch plan is made to reach the apply stage against a source that no longer
matches, the apply-time fallback lane.

`evict-cache` drops the OS page cache for every file under `path` (default: the
repository target) by opening each one non-cached, and is not ledgered. Without
it an HDD case whose payload was just downloaded or mutated measures memory,
not the disk: the ledger's `cache_state` only records the iteration number.
Place it after `mutate` and before the first measured operation, and once more
before a `download` that should read its patch sources cold. Windows only; on
other platforms it walks the tree and reports `supported: false`.

## Guards

| Field | Default | Meaning |
| --- | --- | --- |
| `require_free_gb` | payload estimate times 1.5 when known | Minimum target-drive free space |
| `require_storage_class` | unset | `ssd`, `hdd`, or `removable` |
| `require_no_other_foxy` | `true` | Reject an already running Foxy process |
| `require_clean_worktree` | `false` | Reject a dirty Git worktree |
| `fail_on_db_wipe` | `true` | Invalidate a run whose logs show a DB wipe |
| `max_run_gb` | unset | Invalidate a run that downloads too much |
| `expected_files` | unset | Required `files_updated` for full-download operations |
| `oracle_command` | unset | Independent post-run argv array; nonzero exit invalidates the row |

`oracle_command` runs after every measured `download` and `force-redownload`
operation, not after checks: a check that sits between a mutation and its
repair sees the mutated payload on purpose.

`oracle_command` is an **argv array**, never a command string. A bare string is
rejected with an error naming the fix rather than being split on whitespace,
because whitespace splitting on Windows paths is how that change produces a
mystery failure later. `${REPOSITORY_PATH}` and `${REPOSITORY_URL}` are expanded
in its arguments:

```json
"oracle_command": [
  "${REPO_ROOT}\\target\\release\\foxy-testkit-oracle.exe",
  "--repository-path", "${REPOSITORY_PATH}",
  "--repository-url", "${REPOSITORY_URL}"
]
```

It runs only after a measured operation and receives
`FOXY_TESTKIT_REPOSITORY_PATH`, `FOXY_TESTKIT_REPOSITORY_URL`, and
`FOXY_TESTKIT_RUN_DIR`. Do not use Foxy's own hash implementation as the oracle
when the hash path is the subject under test.

`foxy-testkit-oracle` is the shipped oracle. It is a separate workspace member
with **zero in-repo dependencies**, enforced by
`testkit/runner/tests/oracle_independence.rs`: it fetches the repository's own
published manifests over HTTP, does its own byte-range extraction, and shares no
code with the sync pipeline, part mapping, or storage layer. `--structure-only`
downgrades it to a presence-and-length check; a structural pass is weaker
evidence than a content pass and must not be read as one.

It checks **part** checksums, not file checksums. A manifest's per-file
`checksum` is a rollup over that file's parts and is not comparable to a
whole-file digest; the per-part `checksum` values are plain BLAKE3 over the
part's byte range. Part granularity is also the right one for the patch lane,
where a repaired file can have the correct length and wrong bytes in one part.

## Database variants

The case stays frozen while `--database-mode wal|mvcc` selects a runtime variant.
Rows and baselines are partitioned by `database_mode` and `db_write_gate` so
unlike modes are never treated as regressions of each other. The runner records
and verifies Foxy's effective startup `journal_mode` and `mvcc_enabled` values.

## Cancellation and game-space switch operations

A `download` or `force-redownload` operation with `cancel_after_s` lets the
transfer run that long, invokes `cancel-download`, and waits for the `core-sync`
busy reason to clear; the row's `summary.cancel_quiescent_ms` is the
cancel-to-quiescent latency and the app's `sync_action` record carries
`outcome=cancelled`. Give such an operation `"expect_outcome": "cancelled"` so
the runner requires that exact owned pipeline outcome and skips the
`expected_files` gate and oracle for it; a mismatch adds `outcome-mismatch`.
The download that follows is the recovery gate and the oracle verifies the
payload; rollback policy may leave no files reusable. `patch_fallbacks` and `patch_applies` in `breakdown.run_metrics`
count apply-time delta fallbacks and successful patch applies on any download
row, `patch_cancelled` the attempts a cancel interrupted (their plans stay
`planned` for the resume); `patch_range_requests`, `patch_gap_bytes` and `patch_copy_bytes` sum the
blob range requests, the bytes over-fetched to coalesce them and the bytes
copied from the local source, so an adjacent mutation and a scattered one are
told apart by resource cost, not only elapsed. `cancel_after_s` works on every
GUI sync operation (a `recheck-integrity` cancelled in its hash stage, a
patching `download` cancelled while applying), not only on full downloads.

The ledger row also retains `delta_patch_stages`, the typed per-file planning,
fetch, apply, promote, verify and finalize spans emitted during the operation.
Each span carries monotonic offsets, ownership ids and its own outcome, so a
case can distinguish stage overlap from summed service time and locate a
fallback or cancellation without parsing prose logs.

`switch-game-space` (GUI harness, `config_seed` with several game spaces) takes
`game_space` (the target space id from `games.json`), invokes the app's
`switch-game-space` intent, waits for the `SOL op=space_switch` record and then
for the target space's startup work to settle (`settle_timeout_s`, default
120). The row's `space_switch` record splits the request-to-visible time into
`drain_s`, `reset_s` and `reload_s`.
