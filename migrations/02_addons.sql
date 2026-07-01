CREATE TABLE IF NOT EXISTS addons (
    id INTEGER PRIMARY KEY,
    name TEXT,
    remote_path TEXT,
    local_path TEXT,
    enabled BOOLEAN,
    local_checksum TEXT NOT NULL DEFAULT '',
    remote_checksum TEXT NOT NULL DEFAULT '',
    local_content_hash TEXT NOT NULL DEFAULT '',
    required BOOLEAN NOT NULL,  -- true = required, false = optional
    data_order INTEGER,
    CONSTRAINT addons_unique_name_remote_local UNIQUE (name, remote_path, local_path)
);

CREATE INDEX IF NOT EXISTS idx_addons_remote_local
    ON addons(remote_path, local_path);
