CREATE TABLE IF NOT EXISTS subfiles (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL,
    path TEXT,
    local_length INTEGER,
    local_start INTEGER,
    remote_length INTEGER,
    remote_start INTEGER,
    local_checksum TEXT,
    remote_checksum TEXT,
    data_order INTEGER,
    FOREIGN KEY (file_id) REFERENCES files(id),
    CONSTRAINT subfiles_unique_file_id_path UNIQUE (file_id, path)
);
