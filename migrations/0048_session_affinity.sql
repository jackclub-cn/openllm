-- Prompt-cache affinity survives a restart without retaining raw session IDs
-- or prompt text. The application stores a SHA-256 scope hash and one target
-- per gateway key + route + session.
CREATE TABLE IF NOT EXISTS session_affinity (
    api_key_id INTEGER NOT NULL,
    route_id INTEGER NOT NULL,
    session_hash TEXT NOT NULL,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    upstream_model TEXT NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (api_key_id, route_id, session_hash)
);

CREATE INDEX IF NOT EXISTS idx_session_affinity_expiry
    ON session_affinity(expires_at_unix_ms);
