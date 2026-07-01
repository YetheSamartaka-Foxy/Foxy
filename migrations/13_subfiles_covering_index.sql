-- Covering index for the subfiles query pattern used by Tree::load.
-- The query filters directly by file_id and orders by data_order, id.
-- This index lets SQLite satisfy the ORDER BY from the index without a filesort.
CREATE INDEX IF NOT EXISTS idx_subfiles_file_id_data_order
    ON subfiles(file_id, data_order, id);
