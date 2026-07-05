-- Foxy authoritative bootstrap schema for the Turso data layer (plan.md §6).
--
-- This folds migrations/01..21 into a single final-state schema. Because the
-- auto-DB-wipe gate (db_schema_version.rs) lets the Turso upgrade require a
-- clean rebuild, there is no need to replay 21 incremental migrations: a fresh
-- database is created directly in its final shape.
--
-- Folding notes (what each historical migration contributes):
--   01 repositories + pending_updates   12 repositories.foxy_mode (inlined)
--   21 repositories (remote_url, local_path) composite identity (inlined as the
--      table UNIQUE, replacing the old remote_url-only UNIQUE - no v21 rename)
--   pending_updates (repository_url, local_path) composite PK (inlined; replaces
--      the runtime migrate_pending_updates_local_path fix-up)
--   02 addons   18 addons.display_name   19 addons.client_side (inlined)
--   03 files    04 subfiles
--   05 repository_addons   06 addon_files
--   07 file_subfiles is intentionally ABSENT - dropped by migration 20.
--   08 download_target_file   09 download_target_file_part
--   10 download_patch_file    11 download_patch_op
--   13-17 covering/propagation indexes are pre-created below.
--
-- Invariants preserved (plan.md §6, AGENTS.md, BACKEND_CONVENTIONS.md):
--   * (remote_url, local_path) repository identity.
--   * ON DELETE CASCADE chains from migrations 05-07/10/11.
--   * strftime('%s','now') / CURRENT_TIMESTAMP defaults.
--   * All UNIQUE constraints that back ON CONFLICT upserts.
--
-- The `turso` crate exposes `Connection::execute_batch` (0.6.1+), so this whole
-- file can be applied in one call; the runner also tolerates statement-by-
-- statement application by splitting on `;` for portability.

-- ── Parent tables ──────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS repositories (
    id                 INTEGER PRIMARY KEY,
    name               TEXT NOT NULL,
    remote_url         TEXT NOT NULL,
    local_path         TEXT NOT NULL DEFAULT '',
    image              TEXT NOT NULL DEFAULT '',
    local_checksum     TEXT NOT NULL DEFAULT '',
    remote_checksum    TEXT NOT NULL DEFAULT '',
    local_content_hash TEXT NOT NULL DEFAULT '',
    foxy_mode          TEXT NOT NULL DEFAULT '',
    CONSTRAINT repositories_unique_remote_local UNIQUE (remote_url, local_path)
);

CREATE TABLE IF NOT EXISTS pending_updates (
    repository_url TEXT NOT NULL,
    local_path     TEXT NOT NULL DEFAULT '',
    diff_json      TEXT NOT NULL,
    updated_at     INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    PRIMARY KEY (repository_url, local_path)
);

CREATE TABLE IF NOT EXISTS addons (
    id                 INTEGER PRIMARY KEY,
    name               TEXT,
    remote_path        TEXT,
    local_path         TEXT,
    enabled            BOOLEAN,
    local_checksum     TEXT NOT NULL DEFAULT '',
    remote_checksum    TEXT NOT NULL DEFAULT '',
    local_content_hash TEXT NOT NULL DEFAULT '',
    required           BOOLEAN NOT NULL,           -- true = required, false = optional
    data_order         INTEGER,
    display_name       TEXT NOT NULL DEFAULT '',
    client_side        BOOLEAN NOT NULL DEFAULT 0,
    CONSTRAINT addons_unique_name_remote_local UNIQUE (name, remote_path, local_path)
);

CREATE TABLE IF NOT EXISTS files (
    id                 INTEGER PRIMARY KEY,
    name               TEXT,
    remote_path        TEXT,
    local_path         TEXT,
    local_checksum     TEXT NOT NULL DEFAULT '',
    remote_checksum    TEXT NOT NULL DEFAULT '',
    local_content_hash TEXT NOT NULL DEFAULT '',
    length             INTEGER,
    data_order         INTEGER,
    CONSTRAINT files_unique_name_remote_local UNIQUE (name, remote_path, local_path)
);

-- ── Child tables (FK parents above must exist first) ────────────────────────

-- subfiles uniqueness (file_id, path) is a STANDALONE unique index
-- (idx_subfiles_file_id_path below), NOT an inline `CONSTRAINT … UNIQUE`. A
-- named index is a first-class object the bulk-load path can DROP before a
-- whole-wipe force-redownload load and CREATE once afterward, so the 66k-row
-- INSERT maintains only the rowid PK instead of four B-trees
-- (after_turso_regression_analysis5.md P0-d). `ON CONFLICT (file_id, path)`
-- resolves against this index identically to the old inline constraint.
CREATE TABLE IF NOT EXISTS subfiles (
    id              INTEGER PRIMARY KEY,
    file_id         INTEGER NOT NULL,
    path            TEXT,
    local_length    INTEGER,
    local_start     INTEGER,
    remote_length   INTEGER,
    remote_start    INTEGER,
    local_checksum  TEXT,
    remote_checksum TEXT,
    data_order      INTEGER,
    FOREIGN KEY (file_id) REFERENCES files(id)
);

CREATE TABLE IF NOT EXISTS repository_addons (
    repository_id INTEGER NOT NULL,
    addon_id      INTEGER NOT NULL,
    PRIMARY KEY (repository_id, addon_id),
    FOREIGN KEY (repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY (addon_id)      REFERENCES addons(id)       ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS addon_files (
    addon_id INTEGER NOT NULL,
    file_id  INTEGER NOT NULL,
    PRIMARY KEY (addon_id, file_id),
    FOREIGN KEY (addon_id) REFERENCES addons(id) ON DELETE CASCADE,
    FOREIGN KEY (file_id)  REFERENCES files(id)  ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS download_target_file (
    file_id             INTEGER PRIMARY KEY,
    download_remote_url TEXT NOT NULL,
    download_local_path TEXT NOT NULL,
    size                INTEGER NOT NULL,
    download_total      INTEGER NOT NULL DEFAULT 0,
    download_cycle      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS download_target_file_part (
    subfile_id          INTEGER PRIMARY KEY,
    download_remote_url TEXT NOT NULL,
    download_local_path TEXT NOT NULL,
    size                INTEGER NOT NULL,
    offset              INTEGER NOT NULL,
    download_total      INTEGER NOT NULL DEFAULT 0,
    download_cycle      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS download_patch_file (
    file_id                INTEGER PRIMARY KEY,
    patch_json_path        TEXT NOT NULL,
    patch_blob_path        TEXT NOT NULL,
    planned_copy_bytes     INTEGER NOT NULL DEFAULT 0,
    planned_download_bytes INTEGER NOT NULL DEFAULT 0,
    status                 TEXT NOT NULL DEFAULT 'planned',
    last_error             TEXT,
    created_at             TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at             TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS download_patch_op (
    -- Plain INTEGER PRIMARY KEY (rowid alias) rather than AUTOINCREMENT: Turso's
    -- MVCC mode (journal_mode='mvcc', default-on) rejects AUTOINCREMENT at parse
    -- time, and `id` is not semantically consumed - rows are keyed by the
    -- (file_id, data_order) UNIQUE constraint, so rowid reuse after deletes is
    -- harmless. (plan.md §6/§11.)
    id               INTEGER PRIMARY KEY,
    file_id          INTEGER NOT NULL,
    data_order       INTEGER NOT NULL,
    op_type          TEXT NOT NULL,
    dest_start       INTEGER NOT NULL,
    length           INTEGER NOT NULL,
    target_checksum  TEXT NOT NULL,
    source_start     INTEGER,
    source_checksum  TEXT,
    blob_offset      INTEGER,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    retry_count      INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
    CONSTRAINT download_patch_op_unique_file_order UNIQUE (file_id, data_order)
);

-- ── Indexes (migrations 02-19, all covering/propagation indexes pre-created) ─

CREATE INDEX IF NOT EXISTS idx_addons_remote_local
    ON addons(remote_path, local_path);
CREATE INDEX IF NOT EXISTS idx_addons_name_local_path_remote_checksum
    ON addons(name, local_path, remote_checksum);
CREATE INDEX IF NOT EXISTS idx_addons_display_name
    ON addons("display_name");
CREATE INDEX IF NOT EXISTS idx_addons_client_side
    ON addons(client_side);

CREATE INDEX IF NOT EXISTS idx_files_remote_path
    ON files(remote_path);
CREATE INDEX IF NOT EXISTS idx_files_remote_local
    ON files(remote_path, local_path);
CREATE INDEX IF NOT EXISTS idx_files_local_path_remote_checksum
    ON files(local_path, remote_checksum);

-- Backs the (file_id, path) uniqueness + the ON CONFLICT upsert target
-- (replaces the former inline `CONSTRAINT subfiles_unique_file_id_path`).
CREATE UNIQUE INDEX IF NOT EXISTS idx_subfiles_file_id_path
    ON subfiles(file_id, path);
CREATE INDEX IF NOT EXISTS idx_subfiles_file_id_data_order
    ON subfiles(file_id, data_order, id);
-- (schema v24) idx_subfiles_path_remote_checksum (path, remote_checksum) removed:
-- every subfiles query filters by file_id (covered above), so it had no primary
-- user; dropping it cuts each part write from 4 -> 3 B-trees. See
-- after_turso_regression_analysis6.md.

CREATE INDEX IF NOT EXISTS idx_repository_addons_addon_id
    ON repository_addons(addon_id);
CREATE INDEX IF NOT EXISTS idx_addon_files_file_id
    ON addon_files(file_id);

CREATE INDEX IF NOT EXISTS idx_download_target_file_part_cycle
    ON download_target_file_part(download_cycle);
CREATE INDEX IF NOT EXISTS idx_download_patch_file_status
    ON download_patch_file(status);
CREATE INDEX IF NOT EXISTS idx_download_patch_op_file_id
    ON download_patch_op(file_id);
