-- Record which credential in a provider's key pool served each request.
ALTER TABLE usage_logs
    ADD COLUMN provider_api_key_id INTEGER REFERENCES provider_api_keys(id) ON DELETE SET NULL;
ALTER TABLE usage_logs
    ADD COLUMN provider_api_key_name TEXT;

CREATE INDEX IF NOT EXISTS idx_usage_logs_provider_api_key
    ON usage_logs(provider_api_key_id, created_at);
