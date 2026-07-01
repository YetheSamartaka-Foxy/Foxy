ALTER TABLE addons ADD COLUMN display_name TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_addons_display_name
    ON addons(display_name);
