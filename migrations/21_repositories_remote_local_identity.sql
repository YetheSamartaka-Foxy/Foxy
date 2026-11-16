-- The same URL installed in two folders must retain independent repository state.
CREATE UNIQUE INDEX IF NOT EXISTS repositories_unique_remote_local
ON repositories(remote_url, local_path);
