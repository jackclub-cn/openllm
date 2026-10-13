-- Runtime resilience state that must survive a process restart.
--
-- Provider circuit state, per-model cooldowns and per-credential cooldowns live
-- in memory on the request hot path, so the gateway never takes a SQLite write
-- lock per failure. Without a persisted mirror a restart (deploy, crash, host
-- reboot) silently clears every cooldown and the next request is sent straight
-- back to the provider that was just failing — the exact stampede the circuit
-- breaker exists to prevent.
--
-- Deadlines are absolute (unix milliseconds) so a row expires on its own even
-- while the process is down. A provider row with an elapsed deadline is
-- meaningful: it means "half-open, admit exactly one recovery probe".
CREATE TABLE IF NOT EXISTS provider_circuit_state (
    provider_id INTEGER PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
    failure_streak INTEGER NOT NULL,
    -- 0 when the cooldown has already elapsed (half-open).
    cooldown_until_unix_ms INTEGER NOT NULL,
    -- Last state change, not last flush; bounds how long a half-open marker is
    -- restorable after a long shutdown.
    updated_at_unix_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS target_cooldown_state (
    provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    cooldown_until_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model)
);

CREATE INDEX IF NOT EXISTS idx_target_cooldown_state_until
    ON target_cooldown_state(cooldown_until_unix_ms);

CREATE TABLE IF NOT EXISTS provider_key_cooldown_state (
    provider_key_id INTEGER PRIMARY KEY REFERENCES provider_api_keys(id) ON DELETE CASCADE,
    cooldown_until_unix_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_provider_key_cooldown_state_until
    ON provider_key_cooldown_state(cooldown_until_unix_ms);
