# Core agent notes

- Load `../../conventions/CORE_CONVENTIONS.md` before changing the Turso data layer (the `src/core/db/` seam, `tasks/db_turso.rs`, schema, transactions), core tasks, or filesystem/network handling.
- Load `../../conventions/SYNC_ALGO_CONVENTION.md` before changing sync pipeline, quick scan, remote refresh, tree hashing, download queue construction, delta patching, or pending updates.
- Load `../../conventions/TESTING_CONVENTIONS.md` before changing pure core helpers, parsers, hashing, planning, or algorithms.
- Preserve repository URL normalization, sync checksum semantics, patch-first download fallback, and the bootstrap-schema + `DB_SCHEMA_VERSION`-gated approach to schema changes.
- The scoped repository purge must run with `foreign_keys=OFF` via `FoxyDb::transaction_exclusive` (used by `tasks/purge_repository.rs`); it deletes children before parents, so FK enforcement is redundant and, when ON, makes Turso rescan the large child tables once per deleted parent row (the force-redownload "hang"). Do not route purge through the normal `transaction` helper or otherwise re-enable FK enforcement during purge.
- Do not edit runtime `database.db`, logs, caches, backups, or temp patch artifacts unless explicitly requested.
