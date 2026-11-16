CREATE INDEX IF NOT EXISTS idx_subfiles_file_id_data_order
    ON subfiles(file_id, data_order, id);
