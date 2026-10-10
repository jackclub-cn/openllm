-- Optional per-model concurrency protection. This prevents one hot model
-- from consuming every provider-wide concurrency slot and starving siblings.
ALTER TABLE provider_models ADD COLUMN max_concurrency INTEGER;
ALTER TABLE provider_models ADD COLUMN queue_timeout_seconds INTEGER;
