ALTER TABLE providers ADD COLUMN model_prefix TEXT NOT NULL DEFAULT '';
ALTER TABLE providers ADD COLUMN models_synced_at TEXT;
ALTER TABLE providers ADD COLUMN models_sync_error TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_providers_model_prefix
    ON providers(model_prefix)
    WHERE model_prefix <> '';

