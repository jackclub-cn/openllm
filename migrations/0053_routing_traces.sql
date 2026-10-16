CREATE TABLE IF NOT EXISTS routing_traces (
    request_id TEXT PRIMARY KEY REFERENCES usage_logs(request_id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    route_id INTEGER,
    strategy TEXT NOT NULL,
    session_id TEXT,
    session_decision TEXT NOT NULL DEFAULT 'no_affinity',
    candidates_json TEXT NOT NULL DEFAULT '[]',
    attempt_count INTEGER NOT NULL DEFAULT 0,
    fallback_count INTEGER NOT NULL DEFAULT 0,
    final_provider_id INTEGER,
    final_provider_api_key_id INTEGER,
    final_upstream_model TEXT,
    final_status_code INTEGER,
    success INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_routing_traces_created
    ON routing_traces(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_routing_traces_route
    ON routing_traces(route_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_routing_traces_final_provider
    ON routing_traces(final_provider_id, created_at DESC);

CREATE TABLE IF NOT EXISTS routing_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL REFERENCES routing_traces(request_id) ON DELETE CASCADE,
    attempt_index INTEGER NOT NULL,
    round INTEGER NOT NULL DEFAULT 0,
    candidate_order INTEGER NOT NULL,
    provider_id INTEGER,
    provider_api_key_id INTEGER,
    upstream_model TEXT,
    started_at TEXT NOT NULL,
    latency_ms INTEGER NOT NULL DEFAULT 0,
    status_code INTEGER,
    success INTEGER NOT NULL DEFAULT 0,
    retryable INTEGER NOT NULL DEFAULT 0,
    reason_code TEXT NOT NULL,
    failure_scope TEXT,
    failure_retry TEXT,
    failure_confidence TEXT,
    UNIQUE(request_id, attempt_index)
);

CREATE INDEX IF NOT EXISTS idx_routing_attempts_request
    ON routing_attempts(request_id, attempt_index);
