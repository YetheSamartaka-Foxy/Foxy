-- Clear repository
UPDATE repositories
SET local_checksum = ''
WHERE id = 1;

-- Clear addons
UPDATE addons
SET local_checksum = ''
WHERE id IN (
    SELECT addon_id FROM repository_addons WHERE repository_id = 1
);

-- Clear addon files
UPDATE files
SET local_checksum = ''
WHERE id IN (
    SELECT file_id FROM addon_files
    WHERE addon_id IN (
        SELECT addon_id FROM repository_addons WHERE repository_id = 1
    )
);

-- Clear subfiles
UPDATE subfiles
SET local_checksum = ''
WHERE file_id IN (
    SELECT file_id FROM addon_files
    WHERE addon_id IN (
        SELECT addon_id FROM repository_addons WHERE repository_id = 1
    )
);
