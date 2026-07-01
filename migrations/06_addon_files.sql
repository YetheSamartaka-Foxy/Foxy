CREATE TABLE IF NOT EXISTS addon_files (
    addon_id INTEGER NOT NULL,
    file_id INTEGER NOT NULL,
    PRIMARY KEY (addon_id, file_id),
    FOREIGN KEY (addon_id) REFERENCES addons(id) ON DELETE CASCADE,
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_addon_files_file_id
    ON addon_files(file_id);
