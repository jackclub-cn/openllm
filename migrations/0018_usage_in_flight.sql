-- Marks a request-log row as still processing so the console can show work
-- that is in flight before its upstream response has completed.
ALTER TABLE usage_logs ADD COLUMN in_flight INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_usage_in_flight
    ON usage_logs(in_flight, created_at DESC);
