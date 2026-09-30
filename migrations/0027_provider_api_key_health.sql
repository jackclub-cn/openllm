ALTER TABLE provider_api_keys ADD COLUMN last_test_at TEXT;
ALTER TABLE provider_api_keys ADD COLUMN last_test_ok INTEGER;
ALTER TABLE provider_api_keys ADD COLUMN last_test_latency_ms INTEGER;
ALTER TABLE provider_api_keys ADD COLUMN last_test_checked TEXT;
ALTER TABLE provider_api_keys ADD COLUMN last_test_message TEXT;
