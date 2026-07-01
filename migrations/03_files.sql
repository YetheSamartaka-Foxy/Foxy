CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    name TEXT,
    remote_path TEXT,
    local_path TEXT,
    local_checksum TEXT NOT NULL DEFAULT '',
    remote_checksum TEXT NOT NULL DEFAULT '',
    local_content_hash TEXT NOT NULL DEFAULT '',
    length INTEGER,
    data_order INTEGER,
    CONSTRAINT files_unique_name_remote_local UNIQUE (name, remote_path, local_path)
);

CREATE INDEX IF NOT EXISTS idx_files_remote_path
    ON files(remote_path);

CREATE INDEX IF NOT EXISTS idx_files_remote_local
    ON files(remote_path, local_path);
