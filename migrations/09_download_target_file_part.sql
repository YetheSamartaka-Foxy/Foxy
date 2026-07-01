CREATE TABLE IF NOT EXISTS download_target_file_part (
    subfile_id          INTEGER PRIMARY KEY,
    download_remote_url TEXT NOT NULL,
    download_local_path TEXT NOT NULL,
    size                INTEGER NOT NULL,
    offset              INTEGER NOT NULL,
    download_total      INTEGER NOT NULL DEFAULT 0,
    download_cycle      INTEGER NOT NULL DEFAULT 0
);
