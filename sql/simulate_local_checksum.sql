UPDATE repositories
SET local_checksum = remote_checksum
WHERE id = 1;

UPDATE addons
SET local_checksum = remote_checksum
WHERE id IN (
    SELECT addon_id FROM repository_addons WHERE repository_id = 1
);

UPDATE files
SET local_checksum = remote_checksum
WHERE id IN (
    SELECT file_id FROM addon_files
    WHERE addon_id IN (
        SELECT addon_id FROM repository_addons WHERE repository_id = 1
    )
);

UPDATE subfiles
SET local_checksum = remote_checksum
WHERE file_id IN (
    SELECT file_id FROM addon_files
    WHERE addon_id IN (
        SELECT addon_id FROM repository_addons WHERE repository_id = 1
    )
);
