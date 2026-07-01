CREATE TABLE IF NOT EXISTS file_subfiles (
    file_id INTEGER NOT NULL,
    subfile_id INTEGER NOT NULL,
    PRIMARY KEY (file_id, subfile_id),
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
    FOREIGN KEY (subfile_id) REFERENCES subfiles(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_file_subfiles_subfile_id
    ON file_subfiles(subfile_id);
