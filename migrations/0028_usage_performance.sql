-- Keep lifetime dashboard totals available without repeatedly scanning the
-- entire usage_logs table as request history grows.
CREATE TABLE IF NOT EXISTS usage_lifetime_stats (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    requests INTEGER NOT NULL DEFAULT 0,
    tokens INTEGER NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost_micros INTEGER NOT NULL DEFAULT 0,
    unpriced_requests INTEGER NOT NULL DEFAULT 0,
    successful_requests INTEGER NOT NULL DEFAULT 0,
    latency_ms_sum INTEGER NOT NULL DEFAULT 0
);

INSERT OR IGNORE INTO usage_lifetime_stats (
    id, requests, tokens, prompt_tokens, completion_tokens,
    cache_read_tokens, cache_write_tokens, cost_micros,
    unpriced_requests, successful_requests, latency_ms_sum
)
SELECT
    1,
    COUNT(*),
    COALESCE(SUM(total_tokens), 0),
    COALESCE(SUM(prompt_tokens), 0),
    COALESCE(SUM(completion_tokens), 0),
    COALESCE(SUM(cache_read_tokens), 0),
    COALESCE(SUM(cache_write_tokens), 0),
    COALESCE(SUM(estimated_cost_micros), 0),
    COALESCE(SUM(estimated_cost_micros IS NULL), 0),
    COALESCE(SUM(success), 0),
    COALESCE(SUM(latency_ms), 0)
FROM usage_logs
WHERE in_flight = 0;

CREATE TRIGGER IF NOT EXISTS trg_usage_lifetime_insert
AFTER INSERT ON usage_logs
WHEN NEW.in_flight = 0
BEGIN
    INSERT INTO usage_lifetime_stats (
        id, requests, tokens, prompt_tokens, completion_tokens,
        cache_read_tokens, cache_write_tokens, cost_micros,
        unpriced_requests, successful_requests, latency_ms_sum
    )
    VALUES (
        1, 1, NEW.total_tokens, NEW.prompt_tokens, NEW.completion_tokens,
        NEW.cache_read_tokens, NEW.cache_write_tokens,
        COALESCE(NEW.estimated_cost_micros, 0),
        CASE WHEN NEW.estimated_cost_micros IS NULL THEN 1 ELSE 0 END,
        NEW.success, NEW.latency_ms
    )
    ON CONFLICT(id) DO UPDATE SET
        requests = usage_lifetime_stats.requests + excluded.requests,
        tokens = usage_lifetime_stats.tokens + excluded.tokens,
        prompt_tokens = usage_lifetime_stats.prompt_tokens + excluded.prompt_tokens,
        completion_tokens = usage_lifetime_stats.completion_tokens + excluded.completion_tokens,
        cache_read_tokens = usage_lifetime_stats.cache_read_tokens + excluded.cache_read_tokens,
        cache_write_tokens = usage_lifetime_stats.cache_write_tokens + excluded.cache_write_tokens,
        cost_micros = usage_lifetime_stats.cost_micros + excluded.cost_micros,
        unpriced_requests = usage_lifetime_stats.unpriced_requests + excluded.unpriced_requests,
        successful_requests = usage_lifetime_stats.successful_requests + excluded.successful_requests,
        latency_ms_sum = usage_lifetime_stats.latency_ms_sum + excluded.latency_ms_sum;
END;

CREATE TRIGGER IF NOT EXISTS trg_usage_lifetime_update
AFTER UPDATE ON usage_logs
WHEN OLD.in_flight <> NEW.in_flight
     OR (OLD.in_flight = 0 AND NEW.in_flight = 0)
BEGIN
    UPDATE usage_lifetime_stats
    SET
        requests = MAX(0, requests - CASE WHEN OLD.in_flight = 0 THEN 1 ELSE 0 END),
        tokens = MAX(0, tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.total_tokens ELSE 0 END),
        prompt_tokens = MAX(0, prompt_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.prompt_tokens ELSE 0 END),
        completion_tokens = MAX(0, completion_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.completion_tokens ELSE 0 END),
        cache_read_tokens = MAX(0, cache_read_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.cache_read_tokens ELSE 0 END),
        cache_write_tokens = MAX(0, cache_write_tokens - CASE WHEN OLD.in_flight = 0 THEN OLD.cache_write_tokens ELSE 0 END),
        cost_micros = MAX(0, cost_micros - CASE WHEN OLD.in_flight = 0 THEN COALESCE(OLD.estimated_cost_micros, 0) ELSE 0 END),
        unpriced_requests = MAX(
            0,
            unpriced_requests - CASE
                WHEN OLD.in_flight = 0 AND OLD.estimated_cost_micros IS NULL THEN 1
                ELSE 0
            END
        ),
        successful_requests = MAX(
            0,
            successful_requests - CASE WHEN OLD.in_flight = 0 THEN OLD.success ELSE 0 END
        ),
        latency_ms_sum = MAX(
            0,
            latency_ms_sum - CASE WHEN OLD.in_flight = 0 THEN OLD.latency_ms ELSE 0 END
        )
    WHERE id = 1;

    UPDATE usage_lifetime_stats
    SET
        requests = requests + CASE WHEN NEW.in_flight = 0 THEN 1 ELSE 0 END,
        tokens = tokens + CASE WHEN NEW.in_flight = 0 THEN NEW.total_tokens ELSE 0 END,
        prompt_tokens = prompt_tokens + CASE WHEN NEW.in_flight = 0 THEN NEW.prompt_tokens ELSE 0 END,
        completion_tokens = completion_tokens + CASE WHEN NEW.in_flight = 0 THEN NEW.completion_tokens ELSE 0 END,
        cache_read_tokens = cache_read_tokens + CASE WHEN NEW.in_flight = 0 THEN NEW.cache_read_tokens ELSE 0 END,
        cache_write_tokens = cache_write_tokens + CASE WHEN NEW.in_flight = 0 THEN NEW.cache_write_tokens ELSE 0 END,
        cost_micros = cost_micros + CASE WHEN NEW.in_flight = 0 THEN COALESCE(NEW.estimated_cost_micros, 0) ELSE 0 END,
        unpriced_requests = unpriced_requests + CASE
            WHEN NEW.in_flight = 0 AND NEW.estimated_cost_micros IS NULL THEN 1
            ELSE 0
        END,
        successful_requests = successful_requests + CASE WHEN NEW.in_flight = 0 THEN NEW.success ELSE 0 END,
        latency_ms_sum = latency_ms_sum + CASE WHEN NEW.in_flight = 0 THEN NEW.latency_ms ELSE 0 END
    WHERE id = 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_usage_lifetime_delete
AFTER DELETE ON usage_logs
WHEN OLD.in_flight = 0
BEGIN
    UPDATE usage_lifetime_stats
    SET
        requests = MAX(0, requests - 1),
        tokens = MAX(0, tokens - OLD.total_tokens),
        prompt_tokens = MAX(0, prompt_tokens - OLD.prompt_tokens),
        completion_tokens = MAX(0, completion_tokens - OLD.completion_tokens),
        cache_read_tokens = MAX(0, cache_read_tokens - OLD.cache_read_tokens),
        cache_write_tokens = MAX(0, cache_write_tokens - OLD.cache_write_tokens),
        cost_micros = MAX(0, cost_micros - COALESCE(OLD.estimated_cost_micros, 0)),
        unpriced_requests = MAX(
            0,
            unpriced_requests - CASE WHEN OLD.estimated_cost_micros IS NULL THEN 1 ELSE 0 END
        ),
        successful_requests = MAX(0, successful_requests - OLD.success),
        latency_ms_sum = MAX(0, latency_ms_sum - OLD.latency_ms)
    WHERE id = 1;
END;

-- The two-column in-flight index is a prefix of the new three-column index.
DROP INDEX IF EXISTS idx_usage_api_key_in_flight;
CREATE INDEX IF NOT EXISTS idx_usage_api_key_state_created
    ON usage_logs(api_key_id, in_flight, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_usage_provider_key_stats
    ON usage_logs(provider_id, provider_api_key_id, in_flight, created_at DESC);
