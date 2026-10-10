-- Configuration change history. Every mutating admin operation appends one
-- row so an operator can answer "what changed, when, and by which actor"
-- without scraping process logs.
CREATE TABLE IF NOT EXISTS audit_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- create / update / delete / rotate / action
    action TEXT NOT NULL,
    -- provider / route / api_key / webhook / settings / database / usage
    entity TEXT NOT NULL,
    entity_id TEXT,
    summary TEXT NOT NULL,
    -- JSON object with the fields that changed, or null when not applicable.
    detail TEXT,
    actor TEXT NOT NULL DEFAULT 'local'
);

CREATE INDEX IF NOT EXISTS idx_audit_logs_created_at ON audit_logs(created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_audit_logs_entity ON audit_logs(entity, id DESC);
