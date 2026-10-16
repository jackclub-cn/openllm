CREATE TABLE IF NOT EXISTS provider_capability_profiles (
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    model TEXT NOT NULL DEFAULT '',
    endpoint TEXT NOT NULL DEFAULT '',
    capability TEXT NOT NULL,
    supported INTEGER NOT NULL,
    source TEXT NOT NULL
        CHECK (source IN ('runtime', 'template', 'manual')),
    confidence TEXT NOT NULL DEFAULT 'inferred'
        CHECK (confidence IN ('exact', 'inferred', 'unknown')),
    profile_version TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    valid_until TEXT NOT NULL,
    PRIMARY KEY (provider_id, model, endpoint, capability)
);

CREATE INDEX IF NOT EXISTS idx_provider_capability_profiles_provider
    ON provider_capability_profiles(provider_id, model, endpoint);
