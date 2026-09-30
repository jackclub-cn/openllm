ALTER TABLE usage_logs ADD COLUMN session_id TEXT;

CREATE INDEX IF NOT EXISTS idx_usage_session
    ON usage_logs(session_id, created_at DESC);
