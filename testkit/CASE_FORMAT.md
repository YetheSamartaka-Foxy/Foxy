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

Per-call timing costs roughly 5-10% of wall clock, so a profiled row is **not**
comparable with an unprofiled one. Keep a profiled case under its own id, as
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
| `space` | no | `null` | Repository-space fixture object |
| `operations` | yes | none | Ordered operation objects |
| `repetitions` | no | `3` | Recorded passes |
| `warmup` | no | `false` | One unrecorded pass before recorded passes |
| `metrics` | no | all | Dotted metrics included in reports |
| `thresholds` | no | `{}` | Per-metric `{min,max}` hard gates |
| `guards` | no | defaults below | Precondition object |

Supported operations are `remote-refresh`, `quick-check`, `recheck`,
`recheck-integrity`, `force-redownload`, `download`, `wipe-db`, `mutate`, and
`restore`. Each operation may contain `wait_timeout_s` and `expect`. GUI cannot
express remote-refresh-only or quick-check, so those operations require the CLI
harness. Setup operations are not ledgered.

An expectation is `{path, equals}`, `{path, min}`, `{path, max}`, or
`{path, between:[low,high]}`. Dotted paths resolve against the collected result.

The `mutate` operation adds `profile`, `seed`, `files`, `entries`, `bytes`, and
optional `path`. `path` defaults to the repository target. `restore` replays the
run journal. Supported profiles are documented by `foxy-testkit-mutate --help`.

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
