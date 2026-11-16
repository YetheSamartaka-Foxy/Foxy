CREATE INDEX IF NOT EXISTS idx_subfiles_path_remote_checksum
    ON subfiles(path, remote_checksum);
