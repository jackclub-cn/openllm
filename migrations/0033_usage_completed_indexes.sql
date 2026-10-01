-- Keep dashboard aggregates and completed-request filters fast as the
-- in-flight request log grows. Partial indexes avoid indexing rows while they
-- are still being updated by the request hot path.
CREATE INDEX IF NOT EXISTS idx_usage_completed_created
    ON usage_logs(created_at DESC)
    WHERE in_flight = 0;

CREATE INDEX IF NOT EXISTS idx_usage_completed_provider_created
    ON usage_logs(provider_id, created_at DESC)
    WHERE in_flight = 0;

CREATE INDEX IF NOT EXISTS idx_usage_completed_route_created
    ON usage_logs(route_id, created_at DESC)
    WHERE in_flight = 0;

CREATE INDEX IF NOT EXISTS idx_usage_completed_model_created
    ON usage_logs(requested_model, created_at DESC)
    WHERE in_flight = 0;

CREATE INDEX IF NOT EXISTS idx_usage_completed_provider_key_created
    ON usage_logs(provider_api_key_id, created_at DESC)
    WHERE in_flight = 0;
