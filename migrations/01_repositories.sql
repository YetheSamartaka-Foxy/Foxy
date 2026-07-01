CREATE TABLE IF NOT EXISTS repositories (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    remote_url TEXT UNIQUE ,
    local_path TEXT,
    image TEXT,
    local_checksum TEXT NOT NULL DEFAULT '',
    remote_checksum TEXT NOT NULL DEFAULT '',
    local_content_hash TEXT NOT NULL DEFAULT '',
    foxy_mode TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS pending_updates (
    repository_url TEXT PRIMARY KEY,
    diff_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
