-- Tracks activity separately from row creation so long-running streams are
-- not mistaken for abandoned requests.
ALTER TABLE usage_logs ADD COLUMN last_activity_at TEXT;

UPDATE usage_logs
SET last_activity_at = created_at
WHERE last_activity_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_usage_in_flight_activity
    ON usage_logs(in_flight, last_activity_at);
