# Foxy Sync Algorithm Convention

This document is the canonical convention for Foxy's repository update system.
Load it before changing quick scan, remote refresh, tree hashing, download
queue construction, delta patching, full downloads, pending updates, or
post-download finalization.

The goal is simple:

1. Be fast when nothing changed.
2. Be precise when something changed.
3. Avoid part-level work until a file is already proven suspicious.
4. Avoid remote metadata work unless the repository checksum says it is needed.
5. Never let stale download targets, stale patch plans, or stale hashes decide a
   user's update state.

## Source of Truth

This convention owns the intended behavior for these current code areas:

- `src/core/api/quick_scan/`
- `src/core/api/sync_pipeline/pipeline.rs`
- `src/core/tasks/remote_repository.rs`
- `src/core/tasks/remote_mods/`
- `src/core/tasks/remote_files.rs`
- `src/core/tasks/remote_file_parts/`
- `src/core/tasks/calculate_hashes/`
- `src/core/tasks/delta_patch/`
- `src/core/tasks/download_files/`
- `src/core/models/download_target_file*.rs`
- `src/core/models/download_patch_*.rs`
- UI and CLI entry points that start `SyncMode` runs

When code and this document disagree, the code should be changed to match this
document unless the document is intentionally revised first.

Current schema note: Rust code exposes file parts through `FilePartEntity` and
`FoxyModFilePart`; the backing SQLite table is still named `subfiles`.
Migration 20 drops the former `file_subfiles` junction table because each part
row now stores its owning `file_id` directly.

## Terms

### Remote Tree Checksum

The checksum published by the server in `repo.json` and mod/file/part metadata.
This is the server's authoritative state.

For legacy Swifty-compatible tree checksums, parent checksums are MD5 rollups:

1. Part checksums feed file checksums.
2. File checksums feed addon checksums.
3. Addon checksums feed repository checksums.
4. Children are sorted by `data_order`.
5. Parent checksum input is each child `local_checksum`/remote checksum in
   order.
6. Stored tree checksums are uppercase.

FoxyMode may use BLAKE3 for part content checksums, but the tree relationship
is still ordered and deterministic. The expected algorithm is chosen from the
remote checksum length where needed.

### Local Tree Checksum

The verified local equivalent of the remote tree checksum. It lives in
`local_checksum` on repositories, addons, files, and file parts (`subfiles` in
SQLite, `FilePartEntity`/`FoxyModFilePart` in Rust).

Local tree checksums are expensive because file verification can require
reading part byte ranges from disk. They are the only local values that may be
compared to remote tree checksums to decide exact download requirements.

### Local Content Hash

The fast local-only drift detector. It lives in `local_content_hash` on
repositories, addons, and files.

It is never compared to a server value. It only answers: "Does the local disk
state still look like the last verified local disk state?"

Local content hashes must be file and folder based, not part based:

- Addon content hash: recursive folder fingerprint of expected and unexpected
  local files under the addon directory, ignoring Foxy temp artifacts.
- File content hash: fast file fingerprint using file metadata plus sampled or
  full file bytes.
- Repository content hash: ordered rollup of addon content hashes.

Part checksums are not part of the quick content hash layer.

### Download Target

A transient row in `download_target_file` or `download_target_file_part`.
Download targets describe work for the current run only. They are not a durable
truth source and must not survive into later decisions as stale evidence.

### Delta Patch Plan

A transient per-file plan in `download_patch_file` and `download_patch_op`, with
patch artifacts under the Foxy temp patches directory. A plan can only be used
for the exact file version it was built for. Any validation failure falls back
to a full-file download.

## Core Invariants

1. `repo.json.checksum == repositories.local_checksum` means the local
   repository tree is fully verified against the current remote repository.
   Remote metadata fetches beyond `repo.json` must be skipped unless forced.
2. `repo.json.checksum != repositories.local_checksum` means more work is
   needed, but it does not automatically mean full remote metadata is needed.
3. If `repo.json.checksum == repositories.remote_checksum` and local tree differs,
   the remote metadata graph is unchanged. Use existing DB remote metadata and
   verify local state.
4. If `repo.json.checksum != repositories.remote_checksum`, the remote metadata
   graph may have changed. Fetch remote addon/file/part metadata only for the
   changed or forced scope.
5. `local_content_hash` is valid only for entities whose local tree checksum is
   clean or intentionally baseline-initialized from a clean tree.
6. If a file or addon has a tree mismatch, its content hash must not be refreshed
   to the mismatched disk state. Clear it or leave it stale so quick scan keeps
   routing that item to tree verification.
7. Quick scan must never read part ranges. It can read addon folders and file
   metadata/content samples only.
8. Tree hash verification must be targeted to suspicious files whenever
   possible.
9. Targeted tree operations must not load unrelated file/part rows. They may
   load all repository/addon rows needed for correct rollups, but file and part
   rows must be scoped to affected addons or affected files.
10. Repository-level rollups must never be computed from a partial addon set.
    If the tree is scoped and not all addons are present, update file/addon
    state only, or load enough addon state to roll up safely.
11. Part-level mismatches decide patch/download granularity only after targeted
   tree verification.
12. Download targets and patch plans must be deleted before a new scoped run and
    after a successful run.
13. Patch-first execution is an optimization. Full-file download is always the
    correctness fallback.
14. Pending update cache is derived state. It may speed UI startup, but it must
    be invalidated by clean quick scan, clean tree verification, successful
    download, repository removal, or local path changes.

## Layered Decision Model

The system has four verification layers. Each layer may early-exit or escalate
to the next layer.

### Scoped Tree Loading Contract

Tree loading is one of the most expensive operations in large repositories.
Targeted update paths must distinguish between three scopes:

| Scope | Required rows | Allowed use |
| --- | --- | --- |
| Full repository tree | repository, all addons, all files, all parts | integrity recheck, first full bootstrap, full content-hash repository rollup, fallback repair |
| Addon-scoped tree | repository plus only affected addons/files/parts | pending-addon quick verify, unexpected-file scan, targeted bootstrap, queue rebuild |
| File-scoped tree with full addon headers | repository, all addon rows, affected file/part rows | partial file hashing where repository/addon rollup correctness must be preserved |

Rules:

1. A pending update cache may scope work to affected addon names, but only as a
   hint.
2. A clean scoped result may not clear the update state when cached targets
   still exist; run a full quick verification first.
3. Partial hash repair should load file/part rows only for requested files, but
   keep enough addon/repository rows to recompute ordered parent checksums.
4. Scoped content-hash refresh may persist file/addon content hashes for the
   scoped entities, but it must not rewrite repository `local_content_hash` from
   a partial tree.
5. Full tree loading is acceptable only for full integrity modes, first-time
   full bootstrap, or explicit fallback when scoped mapping is impossible.

### Minimum Work Planner

Every sync mode should build an explicit work scope before doing expensive work.
The scope is a monotonic plan: it may widen when evidence proves the original
scope was too narrow, but it must not silently widen to the full repository for
convenience.

The planner tracks:

- addon names from pending update cache, remote addon-list changes, or quick
  scan mismatches
- addon IDs resolved from the current DB graph
- file IDs proven suspicious by quick scan, manifest diff, or download results
- whether the remote graph is unchanged, partially changed, incomplete, or
  unknown
- whether repository-level rollups require complete addon state

Scope widening rules:

1. Pending cache gives the first addon-name scope for download mode.
2. A clean scoped quick scan with no cached targets can stop.
3. A clean scoped quick scan with cached targets widens to full quick scan
   before clearing update state.
4. A remote addon-list diff widens the scope only to addons whose remote
   checksum, enabled state, path identity, or DB link state changed.
5. Missing DB links widen to the affected addon or file set when they can be
   resolved from metadata.
6. Full repository scope is allowed only for first sync, explicit integrity
   mode, complete baseline bootstrap, or corruption repair where scoped mapping
   cannot be trusted.

The work scope should be logged once per major stage with counts for addons,
files, parts, and the reason the scope widened.

### Remote Addon-List Diff Contract

Remote metadata refresh must avoid fetching per-addon file manifests until the
repository-level addon list proves an addon needs deeper work.

After fetching `repo.json` or `foxy_addons.json`, compare the remote addon list
to the stored addon rows and repository links:

1. If an addon is new, removed, newly linked, unlinked, renamed, moved, enabled
   state changed, or its remote checksum changed, mark that addon changed.
2. If an addon remote checksum equals stored remote checksum and local path
   identity is unchanged, keep its existing file/part graph.
3. If an addon is changed, explicitly forced by recheck level, or DB graph is
   incomplete for that addon, fetch only that addon's file manifest. Pending
   local mismatch alone should reuse the existing remote graph when the addon
   remote checksum and graph completeness prove it is still valid.
4. If only repository-level metadata changed but addon checksums and links are
   unchanged, update repository metadata and skip all mod/file/part fetches.

This keeps ordinary remote updates proportional to changed addons instead of
repository size.

### Pending Scope Payload

The pending update cache should carry enough scope to avoid rediscovering the
same work on the next click.

A pending payload may include:

- addon name
- addon ID when known
- suspicious file IDs
- expected transfer bytes
- remote repository checksum seen when the payload was created
- remote addon checksum seen when the payload was created
- whether unexpected local files were detected
- whether tree verification already ran for the file set

Validity rules:

1. If the current `repo.json.checksum` differs from the payload checksum, keep
   the addon names as a hint but re-run the remote addon-list diff.
2. If addon path identity changed, discard file IDs for that addon.
3. If quick scan proves the scoped payload clean, clear it only after stale
   targets are also cleared or a full quick scan confirms clean state.
4. If download fails or is cancelled, preserve the payload with the remaining
   addon/file scope.

### Layer 0: Metadata Readiness

Purpose: decide whether quick local checks are even meaningful.

Required DB state:

- repository row exists
- repository has linked addons
- addon rows have non-empty `remote_checksum`
- file rows have non-empty `remote_checksum`
- part rows exist and have non-empty `remote_checksum` when the manifest uses
  parts
- local tree baseline exists, unless this run is explicitly allowed to bootstrap
- local content baseline exists for quick scan

Early exits:

- If remote metadata is missing, do not quick scan. Run remote metadata refresh
  or initial bootstrap.
- If local tree baseline is missing, initialize tree checksums once.
- If local content baseline is missing but tree is clean, refresh content hashes
  from the current clean tree.

### Layer 1: Remote Repository Gate

Purpose: decide whether remote metadata needs to be fetched.

This layer fetches only:

```text
<repository_url>/repo.json
```

Then it compares:

```text
repo_json.checksum vs repositories.local_checksum
repo_json.checksum vs repositories.remote_checksum
```

Decision table:

| State | Meaning | Required action |
| --- | --- | --- |
| no DB repository row | first sync | fetch full metadata |
| DB graph incomplete | bootstrap or repair | fetch missing metadata |
| `repo_json.checksum == local_checksum` and baselines complete | local repo is verified current | early exit, no mod/file/part fetch |
| `repo_json.checksum == remote_checksum` but `local_checksum` differs | remote unchanged, local drift or incomplete local hash | skip remote metadata; run local quick/tree path using existing DB remote graph |
| `repo_json.checksum != remote_checksum` | remote changed | fetch remote metadata for changed or forced scope |
| forced integrity recheck | user requested full verification | fetch remote metadata and recalculate tree according to mode |

Important rule: remote recheck must not fetch every `mod.srf`,
`foxy_addon.json`, or part list merely because the user clicked recheck. The
repository-level checksum decides whether deeper remote fetches are needed.

### Layer 2: Quick Local Content Check

Purpose: detect local disk drift cheaply and decide which addons/files deserve
tree verification.

This layer is local-only. It must not fetch remote files or part manifests. It
must not read part ranges.

Algorithm:

1. Load repository, linked enabled addons, and stored `local_content_hash`.
2. If the caller supplied a work scope, load and hash only those addons first.
3. Compute current addon folder fingerprints for enabled addon directories in
   scope.
4. If all enabled addon folder fingerprints equal stored addon
   `local_content_hash` and no addon has a tree mismatch, early exit clean.
5. For each addon with folder content mismatch, missing folder, or existing tree
   mismatch:
   - load expected file rows for only that addon
   - detect missing expected files
   - detect size mismatches
   - compute fast file content hashes only for expected files in that addon
   - detect unexpected local files under the addon folder
6. For each suspicious file, add the file ID to `files_needing_tree_verify`.
7. If `auto_tree_verify_on_mismatch` is enabled, run targeted tree hash
   verification for `files_needing_tree_verify`, then rerun quick scan once.
8. If quick scan proves the addon folder changed but all expected files are
   still valid and only harmless content baseline drift occurred, update the
   addon content hash baseline.
9. Persist pending update summaries only from the final quick scan result.

Minimum work rules:

- Do not enumerate file rows for addons whose folder fingerprint matches and
  whose tree checksum is clean.
- Do not scan unexpected files for addons that are clean.
- Do not compute file content hashes for every file in an addon when missing
  file or size checks already prove the addon needs update, unless file-level
  narrowing is required for patch planning.
- If a persistent addon hash cache miss occurs because the root fingerprint is
  volatile, compute only that addon's fresh folder hash and update the cache
  after the final clean result.

Quick scan outputs:

- clean result: no pending updates, clear pending cache
- suspicious addons/files: pending update summary, optional targeted tree verify
- missing addon folder: pending full addon update
- unexpected files: pending cleanup before download

Quick scan must use the cheapest available cache:

1. Per-worker shared memory addon hash cache.
2. Persistent addon hash cache, keyed by stable addon root fingerprint.
3. Fresh addon folder fingerprint.

Cache correctness rule: a cache hit is valid only when the root fingerprint
matches the current folder state. A volatile or ambiguous fingerprint must miss,
not produce a false clean result.

### Layer 3: Targeted Tree Hash Verification

Purpose: identify the exact files and parts that do not match the remote tree.

This layer may read local file byte ranges and part ranges. It must be scoped to
known suspicious files whenever possible.

Algorithm for a targeted file set:

1. Load a scoped tree for the target files. Keep all repository/addon rows needed
   for parent rollups, but load file/part rows only for affected files/addons.
2. Resolve target file indices from file IDs.
3. Hash parts for only those files.
4. Persist changed part `local_checksum`, `local_start`, and `local_length`.
5. Recompute file checksums from ordered parts.
6. Recompute affected addon checksums from ordered files.
7. Recompute affected repository checksums from ordered addons only when all
   addon rows required for the ordered rollup are available.
8. Refresh content hashes only for entities whose tree checksum now matches
   remote.
9. For a scoped content-hash refresh, persist scoped file/addon content hashes
   and leave repository `local_content_hash` untouched unless the tree is full.
10. Build or update download targets for files whose file or part tree checksums
   still differ.

Escalation to full tree hash is allowed only when:

- the local tree baseline is empty on first initialization
- the user requested integrity recheck
- DB corruption or missing links prevent safe targeted hashing
- a targeted path cannot map file IDs to tree nodes and correctness requires a
  full repair

## Startup Algorithm

Startup runs after the first rendered frame. It must not block first paint.

For each configured repository:

1. Normalize repository URL with trailing slash.
2. Restore cached pending update payload if present, but treat it as provisional.
3. If auto remote recheck is enabled:
   - run the remote repository gate
   - if `repo_json.checksum == local_checksum`, finish clean for remote state
   - if remote changed or DB incomplete, enqueue remote refresh
4. If auto quick scan is enabled:
   - first run a cheap remote checksum probe against `repo.json`
   - if remote checksum differs from local checksum, do not quick scan as the
     final answer; enqueue remote refresh or mark remote update pending
   - if remote checksum matches local checksum and baselines are ready, run quick
     local content check
   - if baselines are not ready, skip quick scan and leave a clear reason in logs
5. Persist only final results:
   - clean quick scan clears pending cache
   - dirty quick scan saves pending cache
   - remote changed state should not be overwritten by a clean local quick scan

Early exits:

- No repositories configured: no workers.
- Repository not in DB: skip quick scan, remote refresh only if requested.
- `repo_json` probe timeout: do not mark clean or dirty from remote; fall back to
  local quick scan only if baselines are ready, and log that remote freshness is
  unknown.
- `repo_json.checksum == local_checksum` and content baseline ready: quick scan
  can finish in the addon-folder layer without tree work.

## Manual Recheck Algorithm

Manual remote recheck means: "Tell me whether the remote repository differs from
my verified local repository."

Algorithm:

1. Fetch `repo.json`.
2. If checksum equals `repositories.local_checksum` and DB state is complete:
   - emit clean state
   - update cheap metadata fields if needed
   - stop
3. If checksum equals `repositories.remote_checksum` but differs from
   `local_checksum`:
   - remote did not change
   - run quick local content check
   - run targeted tree hash if quick scan finds drift
   - build pending update state from local tree mismatch
4. If checksum differs from `repositories.remote_checksum`:
   - fetch changed remote metadata
   - upsert remote addons/files/parts preserving valid local checksums
   - prune stale addon/file links and stale file parts from old manifests
   - run targeted tree verification for affected files/addons
   - build pending update state

Manual recheck must not perform a full hash of every file unless the local tree
baseline is missing or integrity mode was explicitly requested.

## Quick Check Algorithm

Manual quick check means: "Check local disk drift against my last verified
baseline."

Algorithm:

1. Do not fetch remote metadata.
2. Verify metadata and baseline readiness.
3. Run quick local content check.
4. If quick content mismatches exist, run targeted tree verification only for
   suspicious files when the mode requests automatic verification.
5. Persist pending update state.

If remote metadata is not ready, quick check exits with no authoritative result
and logs why.

## Download Algorithm

Download means: "Make the local repository match the current remote repository."

Required inputs:

- normalized repository URL
- local path
- selected addon enable states
- current remote graph in DB or ability to fetch it
- pending update cache is optional and only scopes work optimistically

Algorithm:

1. Clear all download target and patch plan tables before building a new queue.
2. Run a scoped quick local verify:
   - if pending update cache exists, check those addons first
   - scoped quick verify must load only the pending addons' file/part metadata
     when it needs tree state
   - if scoped check is clean but cached targets exist, fall back to full quick
     verify before trusting the clean result
   - if final quick verify is clean, clear stale targets and early exit
3. Run remote repository gate:
   - if remote checksum equals local checksum, early exit clean
   - if remote checksum equals stored remote checksum but local differs, skip
     remote metadata fetch and use existing remote graph
   - if remote checksum differs from stored remote checksum, fetch remote
     metadata for changed or forced addons
4. Run post-remote quick verify:
   - use content hashes to scope suspicious addons/files
   - tree-verify suspicious files when required
   - reuse scoped tree state from bootstrap/hash repair where possible
   - persist pending updates
5. Clean unexpected local files only for addons that are already pending update.
   Then rerun quick verify for those addons.
6. Build download queue from tree mismatches:
   - file target for each mismatched file
   - part target rows for diagnostics and future partial support
   - patch plan rows for files where delta planning is valid and beneficial
7. If no pending updates remain, clear queue and early exit.
8. If pending updates exist but no download targets were built, fail loudly. Do
   not silently mark clean.
9. Optionally back up affected addons before modifying files.
10. Execute patch-first/full-download worker.
11. Hash final downloaded files incrementally using a tree loaded only when the
    first hash batch needs it.
12. Roll up affected addon and repository checksums, reusing the loaded tree
    instead of loading the full repository again.
13. Refresh content hashes for now-clean entities, reusing the same tree when
    it is complete enough for repository rollup.
14. Propagate checksums to sibling repositories sharing the same local paths.
15. Emit final diff and clear pending cache if clean.
16. Delete download targets and successful patch artifacts.
17. Commit rollback session.

Early exits:

- Clean scoped and full quick verify before remote work: stop.
- Remote gate clean before metadata fetch: stop.
- Post-remote quick verify clean: stop.
- Empty download queue with no pending updates: stop.

Failure exits:

- Pending updates but empty download queue: fail.
- Backup failure: fail before modifying files.
- Download failure: rollback touched files.
- Cancellation: rollback touched files and preserve enough state for retry.

## Remote Metadata Refresh Algorithm

Remote metadata refresh has two jobs:

1. Update the DB remote graph to match the server.
2. Preserve reusable local verification state when paths still refer to the
   same local files.

Repository stage:

1. Fetch `repo.json`.
2. Normalize URL and local path.
3. Upsert repository remote checksum and metadata.
4. Check graph completeness without loading the entire tree.
5. If remote checksum equals local checksum and graph is complete, early exit.
6. If graph is incomplete or remote checksum changed, continue.

Addon stage:

1. Fetch addon list from `repo.json` or `foxy_addons.json` depending on FoxyMode.
2. Diff the remote addon list against stored DB addon/link state before
   fetching any per-addon file manifests.
3. Upsert addons by stable remote/local identity.
4. Preserve addon `local_checksum` and `local_content_hash` only when local path
   identity is unchanged.
5. Link addons to repository.
6. Skip disabled addons.
7. For enabled addons, skip file metadata fetch when:
   - addon remote checksum equals addon local checksum
   - addon remote checksum equals stored addon remote checksum
   - addon local path exists
   - addon content baseline exists
   - addon is not explicitly forced by recheck level
   - recheck level does not require addon refresh
8. In download mode, pending local mismatches may scope local verification and
   queue rebuilds to pending addon names, but they must not force a network
   manifest fetch when that addon's remote checksum and stored graph are
   unchanged.

File stage:

1. Fetch `mod.srf` or `foxy_addon.json` only for addons that passed the addon
   stage as needing refresh.
2. Upsert file rows.
3. Preserve file local tree and content hashes only when local path identity is
   unchanged.
4. Reconcile `addon_files` links and prune stale links when safe.
5. Diff the fetched file list before part processing. Mark changed only when
   file checksum, length, path identity, link state, or DB completeness changed.
6. Skip part metadata processing for a file when:
   - file remote checksum equals file local checksum
   - file remote checksum equals stored file remote checksum
   - local file exists with expected length
   - file part graph is complete
   - recheck level does not require file refresh

Part stage:

1. Load and upsert current manifest parts only for files that reached the part
   stage.
2. Preserve existing local part checksums unless remote part metadata changed.
3. Upsert file parts through `FilePartEntity`/`FoxyModFilePart`; do not
   recreate the removed `file_subfiles` junction table.
4. Delete stale `subfiles` rows no longer present in the manifest for those
   files.
5. Compare current part local checksums to remote part checksums.
6. Queue files with layout mismatch, file checksum mismatch, or part checksum
   mismatch.
7. Build delta patch plans for queued files when possible.

Scope rule:

- A remote repository checksum change does not grant permission to recalculate
  every local addon. Fetch enough remote metadata to update the DB graph, but
  local hash repair and queue construction must stay limited to changed or
  forced addons/files.
- If the remote graph is unchanged (`repo.json.checksum ==
  repositories.remote_checksum`) and only the local tree differs, skip remote
  addon/file/part metadata fetches and operate from the existing DB graph.

## Delta Patch Planning

Delta planning happens after remote part metadata is current and before download
execution.

Inputs:

- target file row
- new remote parts
- old local part metadata snapshot
- current local file on disk

A plan is valid only if:

1. The target local file exists.
2. Old local part metadata contains non-empty local checksums and lengths.
3. New remote parts are sorted by `data_order`.
4. Operations cover byte range `0..file.length` exactly with no gaps.
5. Each op has non-zero length.
6. Copy ops point inside the current local file.
7. Insert ops define non-overlapping patch blob offsets.
8. Planned download bytes are less than full file bytes.
9. Savings meet the configured threshold.
10. Ordered target checksums roll up to the new file remote checksum.

Copy matching priority:

1. Same part display path, same length, same checksum.
2. Any old local part with same checksum and length.

Plan persistence:

1. Write patch artifact JSON atomically.
2. Create/truncate patch blob to planned insert byte length.
3. Save one `download_patch_file` row.
4. Replace all `download_patch_op` rows for the file.

If any step fails, do not block the update. Skip the patch plan and allow
full-file download.

## Patch-First Download Algorithm

For each download target:

1. If no patch plan exists, perform full download.
2. Load patch file row and artifact.
3. Validate DB/artifact file ID match.
4. Validate expected output size.
5. Load patch ops.
6. Validate operation coverage.
7. Validate planned target checksum rollup.
8. Preflight copy sources by sampling or checking copy op ranges.
9. If preflight fails, mark fallback and perform full download.
10. Download all insert-remote ranges into the patch blob.
11. Apply copy and insert ops into a temp output file.
12. Promote temp file atomically with rollback protection.
13. Verify final tree checksum from applied segment checksums.
14. On success:
    - mark done
    - clean patch artifacts
    - delete patch ops and patch file row
15. On any failure:
    - restore backup if promotion happened
    - mark fallback
    - clean patch artifacts unless diagnostics are enabled
    - perform full download

Patch fallback must never leave the target file in a partially patched state.

## Full Download Algorithm

Full download is the correctness fallback for every file.

1. Create or resume `*.foxy.part`.
2. Use simple sequential GET for small files (and any file when the server's
   session-level range check failed). Sequential parts resume from the part
   file length.
3. Use the ranged work queue for large files when the range check passed:
   the file is split into a fixed chunk grid, downloaded by parallel range
   workers, and completed chunks are recorded in a `*.foxy.part.meta` sidecar
   after their bytes are on disk. Ranged parts are pre-allocated to full
   length, so resume state comes only from the sidecar, never the part length.
4. Per-file range concurrency is a fair share of the global range budget:
   it grows as the queue drains so tail files and single-file downloads can
   use the full budget, and every range worker goes through the bandwidth
   limiter.
5. Validate `Content-Range` for ranged responses.
6. Validate final byte count (resumed chunks count toward it).
7. Remove the sidecar, then promote `*.foxy.part` atomically to the final path
   with rollback protection.
8. Update in-memory and persisted progress at coarse intervals.

Never write directly to the final file path during transfer. Never trust a
full-length part file without either a valid sidecar or complete persisted
progress.

## Post-Download Hash Finalization

After patch or full download succeeds:

1. Hash only downloaded/touched files.
2. Persist updated part checksums.
3. Roll up file checksums from ordered parts.
4. Roll up addon checksums from ordered files.
5. Roll up repository checksum from ordered addons.
6. Reuse the incremental hash tree for repository rollup, final content refresh,
   and final diff when it is already loaded.
7. If all affected tree checksums match remote, refresh content hashes for those
   files/addons/repository. Repository content hash may be refreshed only from a
   full tree or equivalent complete addon content state.
8. Propagate matching local tree/content checksums to sibling repositories that
   share the same local paths.
9. Emit a final diff.
10. If final diff is clean, clear pending update payload.

The repository is considered successfully updated only when
`repositories.local_checksum == repositories.remote_checksum` and no enabled
addon has pending mismatches.

## Preflight and Cleanup Rules

Before download:

- remove stale `.foxy.tmp`, `.foxy.part`, `.foxy.part.meta`, and `.foxy.bak`
  files only in target directories for the current work scope; sidecars and
  part files of queued targets are kept for resume
- remove stale patch artifacts not referenced by current scoped download targets
- validate persisted download progress against actual temp file sizes (and
  resume sidecars for ranged parts) only for files in the current queue
- perform a session-level connectivity check
- perform a session-level range support check
- perform disk space check using expected transfer bytes, not full bytes when
  patch plans exist

Before queue rebuild:

- truncate download target tables
- remove stale patch rows and artifacts for files not in scope
- rebuild target rows from the final scoped mismatch set, not from all stale
  mismatches in the DB

After successful run:

- truncate download target tables
- remove successful patch rows/artifacts
- commit rollback session
- clear pending updates if clean

After failed run:

- rollback touched files
- preserve enough pending state for retry
- do not mark repository clean

## Pending Update Cache Rules

Pending update cache exists for UI responsiveness and CLI `--update-all` style
operations. It is not authoritative.

It may be used to scope quick verification and queue rebuilds, but only as a
hint. A clean scoped check must fall back to full repository quick verification
before clearing updates if cached download targets exist.

Clear pending cache when:

- quick scan final result is clean
- post-download final diff is clean
- repository is removed
- local path changes
- remote graph is rebuilt and no mismatches remain

Keep or refresh pending cache when:

- quick scan finds local drift
- remote recheck finds remote change
- download fails or is cancelled after rollback

## UI and CLI Mode Contracts

### `QuickCheckOnly`

- local-only
- no remote metadata fetch
- uses quick content layer first
- targeted tree verification only when requested by mode
- updates pending cache from final result

### `RemoteRefreshOnly`

- fetches `repo.json`
- early exits when remote repo checksum equals local repo checksum
- fetches deeper remote metadata only when repo checksum or DB completeness
  requires it
- does not download
- may build pending update cache
- the manual UI "remote recheck" runs this mode with the `prepare_download_plan`
  option, so when updates are found it also builds the final scoped download
  queue, applies patch-plan transfer bytes, and emits that exact queue as the
  pending update payload - without downloading

### `RecheckOnly`

- same remote gate as `RemoteRefreshOnly`
- verifies local tree state when local checksum differs
- does not download
- may build pending update cache
- the addon force-redownload preflight runs this mode with the
  `prepare_download_plan` option to rebuild the final scoped queue and patch-plan
  transfer bytes the same way

### Preparing the download plan (`prepare_download_plan`)

- either recheck mode can set this option to build and persist the final
  download queue (with patch-plan transfer bytes) but stop before backup or
  transfer
- the plan is prepared up front during the recheck the user already triggered,
  so opening the confirmation modal is instant and a following `Download` reuses
  the prepared queue instead of running a second redundant recheck
- backup and network transfer still begin only after the user confirms; the
  reuse fast path re-probes `repo.json` and re-validates every file, so a stale
  queue degrades to a rebuild, never silent corruption

### `RecheckIntegrity`

- explicit expensive mode
- fetches full remote metadata
- recalculates local tree hashes
- refreshes content hashes only for clean tree state
- persists final pending cache

### `Download`

- quick verify first
- remote gate second
- targeted tree verification before queue build
- patch-first download
- full download fallback
- incremental final hash
- clear queue and pending cache only after clean final state

## Performance Requirements

Expected no-change path:

1. Fetch `repo.json` only when remote freshness is requested.
2. Compare `repo_json.checksum` to `repositories.local_checksum`.
3. If equal and baselines complete, exit.
4. If quick local scan is requested, use addon folder fingerprints and cache.
5. Do not load the full tree.
6. Do not read part ranges.
7. Do not fetch mod manifests.
8. Do not build download targets.

Expected local-only drift path:

1. Addon folder hash detects changed addon.
2. File content hash narrows changed files.
3. Targeted tree hash reads only those files' parts.
4. Queue only files whose tree checksums still mismatch.

Expected remote-change path:

1. `repo.json` checksum detects remote change.
2. Fetch only needed remote metadata.
3. Preserve local checksums where remote/local path identity is unchanged.
4. Target tree hashing to changed files/addons.
5. Queue only mismatched files.

Expected pending-addon download path:

1. Use the pending update cache to scope quick verification to affected addons.
2. If tree bootstrap or repair is needed, load only those addons' file/part
   rows.
3. If partial file hashing is needed, load all addon headers but only affected
   file/part rows.
4. Scan unexpected files only under pending addon roots.
5. Rebuild download targets only for final pending addons.
6. Do not perform 100k+ or 1M+ row part-table reads for unrelated addons.

Expected remote addon-list-only path:

1. `repo.json` or `foxy_addons.json` changed.
2. Addon-list diff shows all addon checksums and links are unchanged.
3. Upsert repository-level metadata.
4. Skip all per-addon file manifests.
5. Run local quick/tree verification only if the local tree or content baseline
   is not already clean.

Expected remote single-addon change path:

1. Addon-list diff identifies one changed addon.
2. Fetch only that addon's file manifest.
3. Diff file rows and fetch/process parts only for changed or incomplete files.
4. Build patch plans and download targets only for that addon's mismatched
   files.
5. Reuse existing local checksums and content hashes for all untouched addons.

Expected retry-after-failure path:

1. Read pending scope from the previous failed or cancelled run.
2. Validate only files/addons in that remaining scope.
3. Reuse valid patch plans whose source checksum and target checksum still
   match.
4. Discard only invalid patch plans, not the whole queue.
5. Continue from the remaining files after rollback has restored touched files.

## Logging Requirements

Every major early exit must log:

- repository URL
- mode
- reason
- elapsed time
- whether remote metadata was fetched
- whether tree hashing ran
- whether download targets were built

Every escalation must log:

- source layer
- target layer
- scope count, such as addons/files/parts
- reason for escalation
- whether the scope widened from pending, addon-list diff, quick scan, tree
  verify, or fallback repair

Patch fallback must log:

- file ID
- reason
- whether artifacts were preserved
- whether full download was attempted

The logs should make it possible to answer:

1. Did we fetch only `repo.json` or all manifests?
2. Did quick scan read only folders/files or did tree hashing run?
3. Which files caused part-level verification?
4. Why was a patch plan accepted or rejected?
5. Why did a patch fall back to full download?
6. Why was a repository marked clean or dirty?
7. Was tree loading full-repository, addon-scoped, or file-scoped?
8. How many addons/files/parts were included in the scope?
9. Why did the planner widen or keep the current scope?
10. Did remote refresh fetch addon lists only, addon manifests, or part
    manifests?

## Forbidden Behaviors

- Do not compare `local_content_hash` to remote checksums.
- Do not refresh content hashes for known tree-mismatched entities.
- Do not use stale download target rows as proof that a file still needs update.
- Do not fetch all mod manifests when `repo.json.checksum == local_checksum`.
- Do not run full tree hashing for ordinary no-change startup.
- Do not load the full repository file/part tree for targeted pending-addon
  update work.
- Do not fetch per-addon file manifests before proving the addon changed,
  was forced, or has incomplete DB graph state.
- Do not clean temp files, patch artifacts, or unexpected files outside the
  current work scope unless running explicit maintenance.
- Do not use part hashing as the first local drift detector.
- Do not mark clean after a failed tree hash, failed download, or failed rollback.
- Do not leave patch artifacts or patch DB rows as authoritative state after
  fallback.
- Do not let a clean scoped pending-cache check clear updates without full quick
  verification when cached targets exist.

## Diagnosing False Redownloads

When a user reports "Foxy wants to redownload addons that are already present," it is one of these distinct causes - identify which before changing code:

- **Layout / path mismatch (within a folder).** The `.pbo` files are on disk but under different relative paths/casing than the manifest declares, so `metadata()` on the computed `file.local_path` fails and the files count as missing. Every presence check uses the raw stored `file.local_path` string (`quick_scan/local_path_preflight.rs`, `quick_scan/file_state.rs`, the hashers under `tasks/calculate_hashes/`); `content_hash::normalize_path` only lowercases the cache key on Windows, not the FS call. `local_path_preflight.rs` detects the partial case (addon folder exists, holds ≥50% of expected files, but ZERO resolve → `layout_mismatch_suspected`) and pauses with diagnostics instead of flagging. Diagnostics log a bounded on-disk snapshot plus a recursive root walk (`locate_files_under_root_by_name`) that finds where the files actually live. A structural resolver is intentionally deferred until an affected user's logs pin the transform.

- **Repository-space member with an override folder.** This is distinct from standalone cross-folder installs. If a manifest addon directory already exists in the repository space's configured shared root, that member resolves the addon to the shared root. Only addons absent there resolve under the member's override folder and require one-time population. Sibling propagation remains path-scoped; the shared-root choice is made explicitly while building the repository-space member's metadata graph.

- **Non-convergent re-bootstrap (genuinely missing files).** `local_tree_hash_file_is_incomplete` in `quick_scan/readiness.rs` returns false for missing files, so they enter `ready_file_ids`, get sent to `calculate_hashes_for_files` every recheck, can't be hashed, `local_checksum` stays empty, and bootstrap repeats forever. Wasted work, not a false flag.

To confirm a *real* wrong-flag bug (addons flagged while physically present at the folder), capture a diagnostics export taken while the files are present-on-disk-yet-flagged; the bug would live in hashing/readiness convergence, not in sibling logic.

## Current Implementation Notes

The current code already contains pieces of this design:

- `models::model_tree::Tree` provides full, addon-scoped, and file-scoped load
  paths. Targeted update paths should use the scoped variants.
- `quick_scan::content_hash` computes file and addon content hashes and avoids
  refreshing content hashes for tree mismatches.
- `quick_scan::diff_addon_hash` uses shared and persistent addon folder hash
  caches.
- `quick_scan::diff_file_resolution` escalates addon folder mismatches to
  file-level checks and targeted tree verification.
- `quick_scan::unexpected_files` scopes unexpected-file scans to pending addons.
- `remote_repository` has a repository fast path using `repo.json.checksum`.
- `remote_mods` can force refresh of specific pending addon names without
  treating every addon as locally dirty.
- `remote_file_parts::batch` prunes stale file parts from `subfiles` and builds
  patch plans.
- `delta_patch` validates plan coverage and falls back to full download.
- `download_files` adjusts expected transfer bytes for patchable files and uses
  patch-first execution.

Known risk areas to keep aligned with this document:

- Remote refresh must consistently compare `repo.json.checksum` to
  `repositories.local_checksum` for early exit.
- Download mode must avoid `force_refresh` causing unnecessary full remote graph
  rebuilds when only local drift exists.
- Quick scan must remain folder/file based and must not become part based.
- Content baseline refresh must remain gated by clean tree state.
- Queue rebuild must be scoped to final pending addons/files, not stale target
  rows from previous runs.
- Scoped content-hash refresh must not update repository content hash unless the
  rollup has complete addon content state.
- Full tree loads in download mode should be treated as fallback or integrity
  behavior, not the normal pending-addon path.
- Remote refresh should grow a first-class addon-list diff stage so unchanged
  addons never fetch file manifests just because the repository checksum
  changed.
- Pending update payloads should carry addon/file scope metadata so retries and
  subsequent update clicks do not rediscover the same work from scratch.
- Cleanup and preflight should stay scoped to the current queue; broad cleanup
  belongs in explicit maintenance.
- Patch plans must be treated as optional and transient.

## Acceptance Criteria for Future Changes

A change touching this system is not complete unless these cases are considered:

1. Fresh repository with no DB state.
2. Fully up-to-date repository on startup.
3. Remote `repo.json` changed but local files untouched.
4. Local file changed while remote unchanged.
5. Local addon has unexpected file while expected files are clean.
6. File missing locally.
7. File size changed locally.
8. Remote manifest removed parts from a file.
9. Delta patch plan succeeds.
10. Delta patch plan fails preflight and full download succeeds.
11. Download fails and rollback restores touched files.
12. Cancel during patch/download restores touched files.
13. Pending update cache is stale but local repo is clean.
14. Sibling repository shares already verified local addon files.

For code changes, validate with the normal repo checks:

```text
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
