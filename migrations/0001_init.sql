PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS providers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    provider_type TEXT NOT NULL CHECK (provider_type IN ('openai', 'anthropic', 'ollama', 'custom')),
    base_url TEXT NOT NULL,
    api_key TEXT,
    headers TEXT NOT NULL DEFAULT '{}',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS provider_models (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    model_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(provider_id, model_name)
);

CREATE TABLE IF NOT EXISTS routes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    model_pattern TEXT NOT NULL,
    strategy TEXT NOT NULL DEFAULT 'priority'
        CHECK (strategy IN ('priority', 'weighted', 'round_robin')),
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_routes_model_pattern ON routes(model_pattern);
CREATE INDEX IF NOT EXISTS idx_routes_enabled ON routes(enabled);

CREATE TABLE IF NOT EXISTS route_targets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    route_id INTEGER NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE RESTRICT,
    upstream_model TEXT NOT NULL,
    weight INTEGER NOT NULL DEFAULT 100 CHECK (weight > 0),
    priority INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(route_id, provider_id, upstream_model)
);

CREATE INDEX IF NOT EXISTS idx_route_targets_route ON route_targets(route_id, enabled, priority);

CREATE TABLE IF NOT EXISTS api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    key_prefix TEXT NOT NULL,
    key_suffix TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_used_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash, enabled);

CREATE TABLE IF NOT EXISTS usage_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL UNIQUE,
    api_key_id INTEGER REFERENCES api_keys(id) ON DELETE SET NULL,
    route_id INTEGER REFERENCES routes(id) ON DELETE SET NULL,
    provider_id INTEGER REFERENCES providers(id) ON DELETE SET NULL,
    requested_model TEXT NOT NULL,
    upstream_model TEXT,
    endpoint TEXT NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    latency_ms INTEGER NOT NULL DEFAULT 0,
    status_code INTEGER NOT NULL,
    success INTEGER NOT NULL,
    streamed INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_usage_created ON usage_logs(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_usage_provider ON usage_logs(provider_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_usage_route ON usage_logs(route_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_usage_model ON usage_logs(requested_model, created_at DESC);

