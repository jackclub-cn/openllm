-- Optional per-key request rate and concurrency limits. NULL means unlimited.
ALTER TABLE api_keys ADD COLUMN requests_per_minute INTEGER;
ALTER TABLE api_keys ADD COLUMN max_concurrency INTEGER;

CREATE INDEX IF NOT EXISTS idx_usage_api_key_created
    ON usage_logs(api_key_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_usage_api_key_in_flight
    ON usage_logs(api_key_id, in_flight);
