-- Composite index for sibling propagation JOINs on files.
-- Covers WHERE local_path = ? AND remote_checksum = ? patterns used in
-- pre_propagate_sibling_checksums and propagate_checksums_to_siblings.
CREATE INDEX IF NOT EXISTS idx_files_local_path_remote_checksum
    ON files(local_path, remote_checksum);
