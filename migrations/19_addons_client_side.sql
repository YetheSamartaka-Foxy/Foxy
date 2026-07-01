ALTER TABLE addons ADD COLUMN client_side BOOLEAN NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_addons_client_side
    ON addons(client_side);
