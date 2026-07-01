-- Index for part-level sibling propagation JOINs.
-- Covers: JOIN subfiles ON path = ? AND remote_checksum = ?
CREATE INDEX IF NOT EXISTS idx_subfiles_path_remote_checksum
    ON subfiles(path, remote_checksum);
