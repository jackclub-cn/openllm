-- Analytics filters commonly combine a dimension with the completed request
-- window. Keep those scans index-backed without indexing in-flight rows that
-- are still updated by the request hot path.
CREATE INDEX IF NOT EXISTS idx_usage_completed_endpoint_created
    ON usage_logs(endpoint, created_at DESC)
    WHERE in_flight = 0;

CREATE INDEX IF NOT EXISTS idx_usage_completed_success_created
    ON usage_logs(success, created_at DESC)
    WHERE in_flight = 0;
