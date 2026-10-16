-- Optional per-credential channel limits. Null/zero concurrency and RPM keep
-- the key unlimited, preserving the pre-M4 rotation behaviour. queue_limit is
-- only meaningful while a key is at its concurrency cap.
ALTER TABLE provider_api_keys ADD COLUMN max_concurrency INTEGER;
ALTER TABLE provider_api_keys ADD COLUMN requests_per_minute INTEGER;
ALTER TABLE provider_api_keys ADD COLUMN queue_limit INTEGER;
ALTER TABLE provider_api_keys ADD COLUMN queue_wait_ms INTEGER;
