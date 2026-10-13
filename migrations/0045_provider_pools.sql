-- Provider allocation pools.
--
-- One upstream provider's capacity (requests and/or tokens per rolling window)
-- is shared by every gateway access key that routes to it. Without an explicit
-- distribution a single client's burst can consume the whole upstream window,
-- after which every other key gets 429s from the provider even though they
-- barely used it.
--
-- A pool declares the capacity; zero or more allocations reserve a weighted
-- share of it for specific gateway keys. Keys without an allocation row may
-- borrow idle capacity (work-conserving) but can never push the pool past its
-- limit. Enforcement is in-memory on the request path, so these tables are the
-- configuration only.
CREATE TABLE IF NOT EXISTS provider_pools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    -- Rolling window the limits apply to.
    window_seconds INTEGER NOT NULL DEFAULT 60,
    -- Pool capacity per window. NULL means "this dimension is not limited".
    request_limit INTEGER,
    token_limit INTEGER,
    -- Usage ratio (percent of the limit) at which fair shares become strict:
    -- below it keys borrow idle capacity, at or above it each key is held to
    -- its own share.
    saturation_threshold_pct INTEGER NOT NULL DEFAULT 50,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(provider_id, name)
);

CREATE INDEX IF NOT EXISTS idx_provider_pools_provider
    ON provider_pools(provider_id, enabled);

CREATE TABLE IF NOT EXISTS provider_pool_allocations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pool_id INTEGER NOT NULL REFERENCES provider_pools(id) ON DELETE CASCADE,
    api_key_id INTEGER NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    -- Reserved share of the pool capacity, as a percentage of the limit.
    weight_pct INTEGER NOT NULL DEFAULT 100,
    -- hard: block above the share while saturated.
    -- soft: allow but try this key's traffic after keys inside their share.
    -- burst: allow while the pool still has headroom.
    policy TEXT NOT NULL DEFAULT 'hard'
        CHECK (policy IN ('hard', 'soft', 'burst')),
    UNIQUE(pool_id, api_key_id)
);

CREATE INDEX IF NOT EXISTS idx_provider_pool_allocations_key
    ON provider_pool_allocations(api_key_id);
