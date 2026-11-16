-- Schema-version gates rebuild from this bootstrap; historical migrations are not replayed.
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

CREATE TABLE IF NOT EXISTS storage_read_measurements (
    volume_key      TEXT PRIMARY KEY,
    method          TEXT NOT NULL,
    read_bytes      INTEGER NOT NULL,
    elapsed_ns      INTEGER NOT NULL,
    read_bps        INTEGER NOT NULL,
    measured_at_utc TEXT NOT NULL
);

-- Benchmark folders are authoritative; rebuild this index from them after a wipe.
CREATE TABLE IF NOT EXISTS benchmarks (
    id             TEXT PRIMARY KEY,
    created_at     INTEGER NOT NULL,
    kind           TEXT NOT NULL,
    name           TEXT NOT NULL DEFAULT '',
    repository_url TEXT NOT NULL DEFAULT '',
    local_path     TEXT NOT NULL DEFAULT '',
    outcome        TEXT NOT NULL DEFAULT '',
    elapsed_ms     INTEGER NOT NULL DEFAULT 0,
    favourite      INTEGER NOT NULL DEFAULT 0,
    hidden         INTEGER NOT NULL DEFAULT 0
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

-- Patch rows use (file_id, data_order) identity, so rowid reuse is safe without sequences.
CREATE TABLE IF NOT EXISTS download_patch_op (
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

-- A standalone unique index lets bulk loading defer maintenance without changing ON CONFLICT targets.
CREATE UNIQUE INDEX IF NOT EXISTS idx_subfiles_file_id_path
    ON subfiles(file_id, path);

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
