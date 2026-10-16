-- Provider-side quota readings are kept separately from the gateway's own
-- rolling allocation pools. A snapshot is an observation, not a hard rule:
-- missing, stale or reset-due data must stay distinguishable from exhausted
-- capacity so routing can keep unknown targets neutral.
CREATE TABLE IF NOT EXISTS provider_quota_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    provider_key_id INTEGER REFERENCES provider_api_keys(id) ON DELETE CASCADE,
    model TEXT NOT NULL DEFAULT '',
    window_key TEXT NOT NULL,
    used REAL,
    limit_value REAL,
    remaining REAL,
    unit TEXT NOT NULL,
    reset_at TEXT,
    source TEXT NOT NULL,
    confidence TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    valid_until TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_provider_quota_snapshots_scope
    ON provider_quota_snapshots(provider_id, provider_key_id, model, window_key, unit);

CREATE INDEX IF NOT EXISTS idx_provider_quota_snapshots_valid_until
    ON provider_quota_snapshots(valid_until);
