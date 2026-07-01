-- Composite index for sibling propagation self-JOINs on addons.
-- Covers JOIN ... ON source.name = sibling.name AND source.local_path = sibling.local_path
-- AND source.remote_checksum = sibling.remote_checksum.
CREATE INDEX IF NOT EXISTS idx_addons_name_local_path_remote_checksum
    ON addons(name, local_path, remote_checksum);
