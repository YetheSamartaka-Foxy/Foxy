-- Part rows already store their owning file in subfiles.file_id.
-- Drop the redundant junction table so part refresh no longer needs to maintain it.
DROP TABLE IF EXISTS file_subfiles;
