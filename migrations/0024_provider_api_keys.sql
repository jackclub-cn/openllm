-- Providers can rotate across multiple upstream credentials. The legacy
-- providers.api_key column is retained for compatibility and mirrors the first
-- enabled pool entry.
CREATE TABLE IF NOT EXISTS provider_api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    name TEXT NOT NULL DEFAULT '',
    secret TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_used_at TEXT,
    last_error_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(provider_id, secret)
);

CREATE INDEX IF NOT EXISTS idx_provider_api_keys_provider
    ON provider_api_keys(provider_id, enabled, id);

INSERT INTO provider_api_keys (provider_id, name, secret, enabled)
SELECT id, 'Default', trim(api_key), 1
FROM providers
WHERE api_key IS NOT NULL
  AND trim(api_key) <> ''
  AND NOT EXISTS (
      SELECT 1
      FROM provider_api_keys k
      WHERE k.provider_id = providers.id
        AND k.secret = trim(providers.api_key)
  );
