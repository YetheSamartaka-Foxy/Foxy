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

CREATE INDEX IF NOT EXISTS idx_download_patch_file_status
    ON download_patch_file(status);
