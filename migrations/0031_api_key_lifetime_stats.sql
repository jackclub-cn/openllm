-- Keep per-key lifetime totals constant-time as usage_logs grows.
ALTER TABLE api_keys ADD COLUMN lifetime_requests INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN lifetime_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN lifetime_prompt_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN lifetime_completion_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN lifetime_cost_micros INTEGER NOT NULL DEFAULT 0;
ALTER TABLE api_keys ADD COLUMN lifetime_unpriced_requests INTEGER NOT NULL DEFAULT 0;

CREATE TEMP TABLE api_key_lifetime_backfill (
    api_key_id INTEGER PRIMARY KEY,
    requests INTEGER NOT NULL,
    tokens INTEGER NOT NULL,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL,
    cost_micros INTEGER NOT NULL,
    unpriced_requests INTEGER NOT NULL
);

INSERT INTO api_key_lifetime_backfill
SELECT
    api_key_id,
    COUNT(*),
    COALESCE(SUM(total_tokens), 0),
    COALESCE(SUM(prompt_tokens), 0),
    COALESCE(SUM(completion_tokens), 0),
    COALESCE(SUM(estimated_cost_micros), 0),
    COALESCE(SUM(estimated_cost_micros IS NULL), 0)
FROM usage_logs
WHERE api_key_id IS NOT NULL AND in_flight = 0
GROUP BY api_key_id;

UPDATE api_keys
SET
    lifetime_requests = COALESCE((SELECT requests FROM api_key_lifetime_backfill WHERE api_key_id = api_keys.id), 0),
    lifetime_tokens = COALESCE((SELECT tokens FROM api_key_lifetime_backfill WHERE api_key_id = api_keys.id), 0),
    lifetime_prompt_tokens = COALESCE((SELECT prompt_tokens FROM api_key_lifetime_backfill WHERE api_key_id = api_keys.id), 0),
    lifetime_completion_tokens = COALESCE((SELECT completion_tokens FROM api_key_lifetime_backfill WHERE api_key_id = api_keys.id), 0),
    lifetime_cost_micros = COALESCE((SELECT cost_micros FROM api_key_lifetime_backfill WHERE api_key_id = api_keys.id), 0),
    lifetime_unpriced_requests = COALESCE((SELECT unpriced_requests FROM api_key_lifetime_backfill WHERE api_key_id = api_keys.id), 0);

DROP TABLE api_key_lifetime_backfill;

CREATE TRIGGER IF NOT EXISTS trg_api_key_lifetime_insert
AFTER INSERT ON usage_logs
WHEN NEW.in_flight = 0 AND NEW.api_key_id IS NOT NULL
BEGIN
    UPDATE api_keys
    SET
        lifetime_requests = lifetime_requests + 1,
        lifetime_tokens = lifetime_tokens + NEW.total_tokens,
        lifetime_prompt_tokens = lifetime_prompt_tokens + NEW.prompt_tokens,
        lifetime_completion_tokens = lifetime_completion_tokens + NEW.completion_tokens,
        lifetime_cost_micros = lifetime_cost_micros + COALESCE(NEW.estimated_cost_micros, 0),
        lifetime_unpriced_requests = lifetime_unpriced_requests
            + CASE WHEN NEW.estimated_cost_micros IS NULL THEN 1 ELSE 0 END
    WHERE id = NEW.api_key_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_api_key_lifetime_update
AFTER UPDATE ON usage_logs
WHEN (OLD.api_key_id IS NOT NULL OR NEW.api_key_id IS NOT NULL)
     AND (
         OLD.api_key_id IS NOT NEW.api_key_id
         OR OLD.in_flight <> NEW.in_flight
         OR (OLD.in_flight = 0 AND NEW.in_flight = 0)
     )
BEGIN
    UPDATE api_keys
    SET
        lifetime_requests = MAX(0, lifetime_requests - CASE WHEN OLD.in_flight = 0 THEN 1 ELSE 0 END),
        lifetime_tokens = MAX(0, lifetime_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.total_tokens ELSE 0 END),
        lifetime_prompt_tokens = MAX(0, lifetime_prompt_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.prompt_tokens ELSE 0 END),
        lifetime_completion_tokens = MAX(0, lifetime_completion_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.completion_tokens ELSE 0 END),
        lifetime_cost_micros = MAX(
            0,
            lifetime_cost_micros - CASE
                WHEN OLD.in_flight = 0 THEN COALESCE(OLD.estimated_cost_micros, 0)
                ELSE 0
            END
        ),
        lifetime_unpriced_requests = MAX(
            0,
            lifetime_unpriced_requests - CASE
                WHEN OLD.in_flight = 0 AND OLD.estimated_cost_micros IS NULL THEN 1
                ELSE 0
            END
        )
    WHERE OLD.api_key_id IS NOT NULL AND id = OLD.api_key_id;

    UPDATE api_keys
    SET
        lifetime_requests = lifetime_requests + 1,
        lifetime_tokens = lifetime_tokens + NEW.total_tokens,
        lifetime_prompt_tokens = lifetime_prompt_tokens + NEW.prompt_tokens,
        lifetime_completion_tokens = lifetime_completion_tokens + NEW.completion_tokens,
        lifetime_cost_micros = lifetime_cost_micros + COALESCE(NEW.estimated_cost_micros, 0),
        lifetime_unpriced_requests = lifetime_unpriced_requests
            + CASE WHEN NEW.estimated_cost_micros IS NULL THEN 1 ELSE 0 END
    WHERE NEW.in_flight = 0 AND id = NEW.api_key_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_api_key_lifetime_delete
AFTER DELETE ON usage_logs
WHEN OLD.in_flight = 0 AND OLD.api_key_id IS NOT NULL
BEGIN
    UPDATE api_keys
    SET
        lifetime_requests = MAX(0, lifetime_requests - 1),
        lifetime_tokens = MAX(0, lifetime_tokens - OLD.total_tokens),
        lifetime_prompt_tokens = MAX(0, lifetime_prompt_tokens - OLD.prompt_tokens),
        lifetime_completion_tokens = MAX(0, lifetime_completion_tokens - OLD.completion_tokens),
        lifetime_cost_micros = MAX(
            0,
            lifetime_cost_micros - COALESCE(OLD.estimated_cost_micros, 0)
        ),
        lifetime_unpriced_requests = MAX(
            0,
            lifetime_unpriced_requests - CASE
                WHEN OLD.estimated_cost_micros IS NULL THEN 1
                ELSE 0
            END
        )
    WHERE id = OLD.api_key_id;
END;
