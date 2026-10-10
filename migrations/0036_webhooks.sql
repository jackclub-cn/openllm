-- Outbound event subscriptions. A webhook receives signed JSON POSTs for the
-- request events it subscribes to, so operators can wire the gateway into
-- alerting, audit, or billing pipelines without polling the API.
CREATE TABLE IF NOT EXISTS webhooks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    url TEXT NOT NULL UNIQUE,
    secret TEXT NOT NULL DEFAULT '',
    -- JSON array of subscribed event types, e.g. ["request.completed"].
    event_types TEXT NOT NULL DEFAULT '["request.completed","request.failed"]',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_webhooks_enabled ON webhooks(enabled, id);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    webhook_id INTEGER NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    request_id TEXT,
    status_code INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_webhook
    ON webhook_deliveries(webhook_id, created_at DESC);
