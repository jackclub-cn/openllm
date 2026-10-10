-- Per-provider concurrency protection. A null/zero max_concurrency keeps the
-- provider unlimited; queue_timeout_seconds controls the bounded wait for a
-- slot, with null using the gateway default.
ALTER TABLE providers ADD COLUMN max_concurrency INTEGER;
ALTER TABLE providers ADD COLUMN queue_timeout_seconds INTEGER;
