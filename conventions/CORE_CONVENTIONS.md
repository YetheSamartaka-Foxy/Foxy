\## Core conventions (Turso)

\### Data access

\- Persistence is the **Turso** engine (pure-Rust, async-native, SQLite-compatible). There is **no SeaORM/sqlx and no `entities/`** - all DB access goes through the seam in `src/core/db/` (`FoxyDb`, `DbTxn`, `OwnedDbTxn`, `DbRow`, `DbValue`, `params!`, `DbErr`). Get a handle with `context.db()`; read with `query_one`/`query_all` + `DbRow` getters; write with `execute`/`execute_retry`/`transaction`. Never reach for the raw `turso` API outside `db/` and `tasks/db_turso.rs`.

\- The database is **per game space**. `db_turso::database_file_path()` resolves `database.db` from `spaces::active_game_space_dir()`, and the process-wide handle is a slot keyed by that path so a runtime space switch opens the target space's database. Anything cached per database (the shared background context, one-time maintenance passes) must be keyed the same way - never a bare `OnceLock`. See `conventions/GAME_SPACES_CONVENTIONS.md`.

\- Game-space isolation is by directory, not by a `game_id` column. Do **not** add one to `repositories`; the `(remote_url, local_path)` identity rules stay exactly as they are.

\- **One process per game space database.** Turso has no multi-process access - two processes on one file is undefined behavior, not concurrent-writer WAL. `tasks/db_process_lock.rs` holds an advisory whole-file lock on `database.lock` for the life of the process; the GUI claims it in `Foxy::new` (and again on a space switch), the CLI claims it in `cli::run_from_env` for every command that opens the database, and `open_and_prepare_database` claims it as a backstop. A contended claim retries for 5s (the installer's `/CLOSEAPPLICATIONS` handoff) and then refuses: the GUI shows a close-only prompt, the CLI exits `DATABASE_BUSY`. Commands that deliberately run beside a live GUI (`agent-gui`, `steam-helper`, `server`, `version`, `ui`) are exempt in `command_uses_database`.

\- **The sidecar is not the schema.** `db_schema_version.rs` records the intended generation, but the bootstrap is applied with `CREATE TABLE IF NOT EXISTS`, so it no-ops on an older database and leaves the old tables. `tasks/db_schema_check.rs` probes the live database on every open: column coverage parsed from `sql/turso_schema.sql`, plus a parse-only `prepare()` of `REPOSITORY_UPSERT_SQL` / `PENDING_UPDATE_UPSERT_SQL` (the only way to catch a missing composite UNIQUE behind an `ON CONFLICT`). An incompatible verdict re-raises the wipe prompt as non-dismissible. Keep those upsert constants shared between the production write and the probe so they cannot drift.

\- Keep DB logic under `src/core/`.

\- Prefer small query helpers with clear inputs and outputs.

\- Avoid over-generic helpers unless there are at least two real call sites.

\- `PRAGMA foreign\_keys = ON` is enabled on every connection (`db_turso::connect_tuned`). `ON DELETE CASCADE` (baked into `sql/turso_schema.sql`) fires automatically - keep this in mind when writing DELETE queries or reasoning about orphaned rows.

\- **Tuned connections are pooled per database** (`db_turso::connect\_pooled`); the seam borrows one and returns it on drop. A `Database::connect()` in Turso 0.7.2 builds a fresh pager, blocking-reads page 1, opens and closes a read transaction and deep-clones the schema, and the engine's compiled-statement cache dies with the connection - a per-call connection costs 61 µs on a small read against 5 µs on a pooled one with a cached program. `TunedConnection::prepare\_tuned` admits a SQL shape to that cache on its **second** use and caps it (the engine's own map is unbounded, so one-off `IN (?, ?, ...)` shapes must not pin a program each). Two rules keep this sound: a connection that is not in autocommit is dropped rather than pooled, and any DDL bumps `bump\_schema\_epoch()` so every pooled connection is retired - a cached program is only rechecked against its own connection's schema snapshot, which is refreshed only when a statement is compiled, so DDL that happened *around* an idle connection would otherwise leave it serving programs built against dropped roots. A caller that changes connection-scoped state (the purge's `foreign\_keys = OFF`) must call `retire()`. Use the unpooled `connect\_tuned` for bootstrap and compaction. `FOXY\_DB\_POOL\_IDLE` caps the idle set (default 8; `0` disables pooling for an A/B).

\- Only the PRAGMAs Turso honors are set, per-connection, in `db_turso::connect\_tuned` (`foreign\_keys`, `synchronous`, `temp\_store`, `cache\_size`, `journal\_mode`; `busy\_timeout` via the `Connection` method). The WAL-tuning PRAGMAs (`wal\_autocheckpoint`, `journal\_size\_limit`, `mmap\_size`) are no-ops in Turso and intentionally dropped. Add new PRAGMAs there.

\- Writes run on **single-writer WAL by default; MVCC is OFF**. `connect\_tuned` sets `journal\_mode='wal'` (and migrates files persisted as mvcc from the old default-on era; the wal -> mvcc -> wal round trip is a test). The seam keeps `BEGIN CONCURRENT` + conflict-retry so `FOXY\_DB\_MVCC=1` can opt in for experiments. Do not flip the default; the measured record is **WAL vs MVCC** below. Prefer plain `INTEGER PRIMARY KEY` over `AUTOINCREMENT` (rowid reuse is harmless for Foxy's keyed tables). Turso 0.7 allows only one in-flight write per connection: await writes sequentially on `OwnedDbTxn` / `copy\_table` / purge, and do not retry `SQL statements in progress` as lock Busy.

\### WAL vs MVCC (Turso 0.7.2)

\- **Verdict: MVCC is worse for Foxy.** Leave WAL as the shipping journal. Engine microbenchmarks can look kind to MVCC; every real sync and purge we have measured is worse on `db\_write\_time\_ms`, and none of them is faster on wall clock. Re-open only after a Turso crate bump, then re-run the benches below **and** the same test-kit WAL vs MVCC pair.

\- **Stack that this record applies to:** Foxy 1.2.0, `turso = "0.7.2"` (`Cargo.toml` / lock checksum `f9491d7a...`), shipping write-gate 4, `FOXY\_DB\_MVCC` unset (WAL) vs `FOXY\_DB\_MVCC=1` (`journal\_mode='mvcc'` + `BEGIN CONCURRENT`). Compare only through `foxy-testkit compare` (never mix debug/release, GUI/CLI, or different gates). Cases: `perf-redownload-small-ssd` and `perf-redownload-big-ssd` on NVMe (GUI harness, 2026-09-09).

\- **Engine benches (0.7.2, `bench_mvcc_write_degradation` / `bench_mvcc_concurrent_writers`):** the 0.6 pathologies are gone (sequential upserts no longer degrade batch over batch; a 16-writer fan-out is ~7.7x *faster* than WAL at the engine). Write-gate 1 to 8: MVCC `db\_write\_time\_ms` stays flat (296 -> 426 ms) while WAL grows ~7x (300 -> 2 180 ms). Exclusive purge is still ~70% *slower* under MVCC (359 ms against 211 ms).

\- **Test kit, small redownload (217 files, warm n=2, gate 4):** force-redownload 51.17 s WAL vs 51.35 s MVCC (`no-difference`). `database.write\_time\_ms` 125 vs 210 (+68% candidate-worse). Recheck wall clock on this case is noise (WAL warm min 0.47 s / max 30.5 s vs MVCC ~0.44 s); do not treat that as an MVCC win.

\- **Test kit, big redownload (3738 files, ~92 GB, cold, gate 4):** 801 s WAL vs 805 s MVCC (`no-difference`, `compare` +0.22%). `database.write\_time\_ms` 11.6 s vs 37.4 s (+168%, about 3x candidate-worse). Recheck after a successful download is ~0.45 s on both. Download `elapsed\_s` is network-bound; a slower writer does not show up in wall clock until persistence is a large share of the run (refresh/purge, not a 92 GB transfer).

\- **How to re-measure:** `foxy-testkit run --case .\\testkit\\cases\\perf-redownload-small-ssd.json --database-mode wal --db-write-gate 4` then the same with `--database-mode mvcc`; repeat for `perf-redownload-big-ssd`; `foxy-testkit compare --case-id <id> --baseline wal --candidate mvcc --baseline-gate 4 --candidate-gate 4` (big cases are cold-only; pass `--cache-state cold`). Also re-run a real-sized exclusive purge. Isolated benches alone are not enough to flip the default.



\### Schema changes

\- Never edit `database.db` directly unless explicitly instructed.

\- The schema is a single folded bootstrap file, `sql/turso_schema.sql`, applied to a fresh database (no incremental migration replay; the legacy `migrations/*.sql` files are historical only). Edit that file for schema changes and keep its `ON DELETE CASCADE` chains and `(remote_url, local_path)` identity intact.

\- A change that an existing local database cannot keep using must bump `DB\_SCHEMA\_VERSION` in `tasks/db\_schema\_version.rs` by one. That triggers the startup wipe-and-rebuild prompt (the Turso cutover itself bumped 21 -> 22).

\- Update models and any query code that depends on the schema.

\### Repository instance identity is `(remote_url, local_path)`

\- A repository row is identified by the composite `(remote_url, local_path)`, not by URL alone (migration 21, `upsert_repository_entry` `on_conflict`). The same URL downloaded to two folders is two independent rows. `addons`/`files` are per-instance too: conflict-keyed on `(Name, RemotePath, LocalPath)`; `remote_files.rs` separates "matching remote paths but different local paths".

\- Any DELETE/purge/wipe must be scoped by `local_path` unless it is intentionally URL-wide. `purge_repository_internal` takes `scope_local_path: Option<&str>`; `purge_repository_db_only_by_url_and_path` and `Foxy::wipe_repository_database_entries_by_url_and_path` exist for the scoped path. A `WHERE remote_url = ?` wipe will destroy a same-URL sibling in another folder (this caused real data loss). Only delete-repo uses URL-wide, and only when no other UI repo uses the URL.

\- `pending_updates` is composite-keyed `(repository_url, local_path)`; read/write through `context.target_local_path`. Quick-scan expands each input URL to its DB instances (`load_repository_instance_paths`) and scans each with a path-scoped `FoxyContext`.

\- Cross-repo sibling checksum propagation (`tasks/calculate_hashes/propagation.rs`) joins on `source.local_path = sibling.local_path` (+ same `remote_checksum`) and only ever sets `local_checksum = remote_checksum` (marks synced, never unsynced). Standalone different-folder repos remain independent. The explicit exception is a repository-space member with its own override folder: manifest addons already present under that space's configured shared root resolve there, while addons absent from the shared root resolve under the member's override folder.

\- Every `local_path` used as a key must funnel through `content_hash::normalize_path` (idempotent) so core-emitted paths, the saved `pending_updates` key, and the UI's `repo.path` canonicalize identically.

