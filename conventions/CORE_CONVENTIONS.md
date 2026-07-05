\## Core conventions (Turso)

\### Data access

\- Persistence is the **Turso** engine (pure-Rust, async-native, SQLite-compatible). There is **no SeaORM/sqlx and no `entities/`** - all DB access goes through the seam in `src/core/db/` (`FoxyDb`, `DbTxn`, `OwnedDbTxn`, `DbRow`, `DbValue`, `params!`, `DbErr`). Get a handle with `context.db()`; read with `query_one`/`query_all` + `DbRow` getters; write with `execute`/`execute_retry`/`transaction`. Never reach for the raw `turso` API outside `db/` and `tasks/db_turso.rs`.

\- Keep DB logic under `src/core/`.

\- Prefer small query helpers with clear inputs and outputs.

\- Avoid over-generic helpers unless there are at least two real call sites.

\- `PRAGMA foreign\_keys = ON` is enabled on every connection (`db_turso::connect_tuned`). `ON DELETE CASCADE` (baked into `sql/turso_schema.sql`) fires automatically - keep this in mind when writing DELETE queries or reasoning about orphaned rows.

\- Only the PRAGMAs Turso honors are set, per-connection, in `db_turso::connect\_tuned` (`foreign\_keys`, `synchronous`, `temp\_store`, `cache\_size`, `journal\_mode`; `busy\_timeout` via the `Connection` method). The WAL-tuning PRAGMAs (`wal\_autocheckpoint`, `journal\_size\_limit`, `mmap\_size`) are no-ops in Turso and intentionally dropped. Add new PRAGMAs there.

\- Writes run on **single-writer WAL by default; MVCC is OFF**. Turso's MVCC (`journal\_mode='mvcc'` + `BEGIN CONCURRENT`) is still beta and was measured to be much slower for this app's write-heavy metadata-rebuild / hash-persist workload: the per-Database version store accumulates across a session, so sustained sequential upserts run ~8x slower and the ~16-way concurrent mod rebuild degrades to O(N^2) (a single 16k-row batch hit 372s on TFR_40K); a 66k-row purge took ~900s under mvcc vs ~33s under WAL (the force-redownload "hang"). It also caused cross-connection read-after-write misses and the cross-runtime purge deadlock. `connect\_tuned` therefore sets `journal\_mode='wal'` explicitly (this also migrates database files persisted as mvcc during the old default-on era). The seam's write paths keep the `BEGIN CONCURRENT` + conflict-retry plumbing so `FOXY\_DB\_MVCC=1` (or `true`/`on`/`yes`) can opt back in for experiments once the engine's MVCC write path matures; do not flip the default without re-running `bench_mvcc_write_degradation` / `bench_mvcc_concurrent_writers`. MVCC mode rejects `AUTOINCREMENT` at parse time - use plain `INTEGER PRIMARY KEY`.



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

