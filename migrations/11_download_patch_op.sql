CREATE TABLE IF NOT EXISTS download_patch_op (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id         INTEGER NOT NULL,
    data_order      INTEGER NOT NULL,
    op_type         TEXT NOT NULL,
    dest_start      INTEGER NOT NULL,
    length          INTEGER NOT NULL,
    target_checksum TEXT NOT NULL,
    source_start    INTEGER,
    source_checksum TEXT,
    blob_offset     INTEGER,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    retry_count     INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
    CONSTRAINT download_patch_op_unique_file_order UNIQUE (file_id, data_order)
);

CREATE INDEX IF NOT EXISTS idx_download_patch_op_file_id
    ON download_patch_op(file_id);
