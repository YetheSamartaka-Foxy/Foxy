CREATE TABLE IF NOT EXISTS repository_addons (
    repository_id INTEGER NOT NULL,
    addon_id INTEGER NOT NULL,
    PRIMARY KEY (repository_id, addon_id),
    FOREIGN KEY (repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY (addon_id) REFERENCES addons(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_repository_addons_addon_id
    ON repository_addons(addon_id);
