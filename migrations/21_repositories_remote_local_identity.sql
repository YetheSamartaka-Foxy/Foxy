-- Repository instances are identified by remote URL plus local path.
-- The runtime migration in init_database.rs rebuilds legacy databases that
-- still have the original remote_url-only UNIQUE constraint.
CREATE UNIQUE INDEX IF NOT EXISTS repositories_unique_remote_local
ON repositories(remote_url, local_path);
