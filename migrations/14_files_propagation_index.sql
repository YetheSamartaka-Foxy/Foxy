CREATE INDEX IF NOT EXISTS idx_files_local_path_remote_checksum
    ON files(local_path, remote_checksum);
