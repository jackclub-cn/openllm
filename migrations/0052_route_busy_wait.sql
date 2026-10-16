-- Route-level bounded patience and short-term target sinks.
--
-- A sink is deliberately separate from a cooldown: a short rate-limit does not
-- prove the credential or provider is unhealthy, so it only moves the target
-- to the back of the candidate order for a short period. The bounded wait is
-- opt-in per route and defaults to zero.
ALTER TABLE routes ADD COLUMN all_targets_busy_wait_seconds INTEGER NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN retry_jitter_ms INTEGER NOT NULL DEFAULT 250;
ALTER TABLE routes ADD COLUMN max_waiting_requests INTEGER NOT NULL DEFAULT 32;
ALTER TABLE routes ADD COLUMN keepalive_during_wait INTEGER NOT NULL DEFAULT 1;

CREATE TABLE IF NOT EXISTS target_sink_state (
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    provider_key_id INTEGER NOT NULL DEFAULT 0,
    model TEXT NOT NULL,
    sink_until_unix_ms INTEGER NOT NULL,
    reason_code TEXT NOT NULL,
    PRIMARY KEY (provider_id, provider_key_id, model)
);

CREATE INDEX IF NOT EXISTS idx_target_sink_state_until
    ON target_sink_state(sink_until_unix_ms);
