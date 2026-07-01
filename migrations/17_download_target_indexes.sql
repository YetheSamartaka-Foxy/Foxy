-- Index for download_target_file JOIN queries against addon_files.
-- The download orchestrator frequently JOINs download_target_file with
-- addon_files on file_id; while file_id is the PK, this index on the
-- download_local_path column speeds up ORDER BY and filtered lookups.
-- More importantly, download_target_file_part currently has no secondary
-- index at all; queries filtering by download_cycle benefit from this.
CREATE INDEX IF NOT EXISTS idx_download_target_file_part_cycle
    ON download_target_file_part(download_cycle);
