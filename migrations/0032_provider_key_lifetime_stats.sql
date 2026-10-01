-- Keep provider-key health statistics constant-time as usage_logs grows.
ALTER TABLE provider_api_keys ADD COLUMN lifetime_requests INTEGER NOT NULL DEFAULT 0;
ALTER TABLE provider_api_keys ADD COLUMN lifetime_successes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE provider_api_keys ADD COLUMN lifetime_latency_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE provider_api_keys ADD COLUMN lifetime_prompt_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE provider_api_keys ADD COLUMN lifetime_completion_tokens INTEGER NOT NULL DEFAULT 0;

CREATE TEMP TABLE provider_api_key_lifetime_backfill (
    provider_api_key_id INTEGER PRIMARY KEY,
    requests INTEGER NOT NULL,
    successes INTEGER NOT NULL,
    latency_ms INTEGER NOT NULL,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL
);

INSERT INTO provider_api_key_lifetime_backfill
SELECT
    provider_api_key_id,
    COUNT(*),
    COALESCE(SUM(success), 0),
    COALESCE(SUM(latency_ms), 0),
    COALESCE(SUM(prompt_tokens), 0),
    COALESCE(SUM(completion_tokens), 0)
FROM usage_logs
WHERE provider_api_key_id IS NOT NULL AND in_flight = 0
GROUP BY provider_api_key_id;

UPDATE provider_api_keys
SET
    lifetime_requests = COALESCE((SELECT requests FROM provider_api_key_lifetime_backfill WHERE provider_api_key_id = provider_api_keys.id), 0),
    lifetime_successes = COALESCE((SELECT successes FROM provider_api_key_lifetime_backfill WHERE provider_api_key_id = provider_api_keys.id), 0),
    lifetime_latency_ms = COALESCE((SELECT latency_ms FROM provider_api_key_lifetime_backfill WHERE provider_api_key_id = provider_api_keys.id), 0),
    lifetime_prompt_tokens = COALESCE((SELECT prompt_tokens FROM provider_api_key_lifetime_backfill WHERE provider_api_key_id = provider_api_keys.id), 0),
    lifetime_completion_tokens = COALESCE((SELECT completion_tokens FROM provider_api_key_lifetime_backfill WHERE provider_api_key_id = provider_api_keys.id), 0);

DROP TABLE provider_api_key_lifetime_backfill;

CREATE TRIGGER IF NOT EXISTS trg_provider_api_key_lifetime_insert
AFTER INSERT ON usage_logs
WHEN NEW.in_flight = 0 AND NEW.provider_api_key_id IS NOT NULL
BEGIN
    UPDATE provider_api_keys
    SET
        lifetime_requests = lifetime_requests + 1,
        lifetime_successes = lifetime_successes + NEW.success,
        lifetime_latency_ms = lifetime_latency_ms + NEW.latency_ms,
        lifetime_prompt_tokens = lifetime_prompt_tokens + NEW.prompt_tokens,
        lifetime_completion_tokens = lifetime_completion_tokens + NEW.completion_tokens
    WHERE id = NEW.provider_api_key_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_provider_api_key_lifetime_update
AFTER UPDATE ON usage_logs
WHEN (OLD.provider_api_key_id IS NOT NULL OR NEW.provider_api_key_id IS NOT NULL)
     AND (
         OLD.provider_api_key_id IS NOT NEW.provider_api_key_id
         OR OLD.in_flight <> NEW.in_flight
         OR (OLD.in_flight = 0 AND NEW.in_flight = 0)
     )
BEGIN
    UPDATE provider_api_keys
    SET
        lifetime_requests = MAX(0, lifetime_requests - CASE WHEN OLD.in_flight = 0 THEN 1 ELSE 0 END),
        lifetime_successes = MAX(0, lifetime_successes - CASE WHEN OLD.in_flight = 0 THEN OLD.success ELSE 0 END),
        lifetime_latency_ms = MAX(0, lifetime_latency_ms - CASE WHEN OLD.in_flight = 0 THEN OLD.latency_ms ELSE 0 END),
        lifetime_prompt_tokens = MAX(0, lifetime_prompt_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.prompt_tokens ELSE 0 END),
        lifetime_completion_tokens = MAX(0, lifetime_completion_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.completion_tokens ELSE 0 END)
    WHERE OLD.provider_api_key_id IS NOT NULL AND id = OLD.provider_api_key_id;

    UPDATE provider_api_keys
    SET
        lifetime_requests = lifetime_requests + 1,
        lifetime_successes = lifetime_successes + NEW.success,
        lifetime_latency_ms = lifetime_latency_ms + NEW.latency_ms,
        lifetime_prompt_tokens = lifetime_prompt_tokens + NEW.prompt_tokens,
        lifetime_completion_tokens = lifetime_completion_tokens + NEW.completion_tokens
    WHERE NEW.in_flight = 0 AND id = NEW.provider_api_key_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_provider_api_key_lifetime_delete
AFTER DELETE ON usage_logs
WHEN OLD.in_flight = 0 AND OLD.provider_api_key_id IS NOT NULL
BEGIN
    UPDATE provider_api_keys
    SET
        lifetime_requests = MAX(0, lifetime_requests - 1),
        lifetime_successes = MAX(0, lifetime_successes - OLD.success),
        lifetime_latency_ms = MAX(0, lifetime_latency_ms - OLD.latency_ms),
        lifetime_prompt_tokens = MAX(0, lifetime_prompt_tokens - OLD.prompt_tokens),
        lifetime_completion_tokens = MAX(0, lifetime_completion_tokens - OLD.completion_tokens)
    WHERE id = OLD.provider_api_key_id;
END;
