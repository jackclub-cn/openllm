-- One row per running gateway instance.
--
-- The gateway is single-instance by design: everything lives in one SQLite
-- file, and each process keeps its own in-memory cooldowns, pool windows and
-- concurrency slots. Two processes on the same file therefore do not fail
-- loudly, they silently disagree. This lease makes that condition visible:
-- the row is refreshed by a heartbeat, expires on a TTL, and a starting process
-- refuses to serve while a fresh foreign lease exists.
--
-- The same DDL is executed before migrations run (`INSTANCE_LEASE_DDL` in
-- src/startup.rs) so a second instance is detected before it touches the
-- schema; this migration records it for databases created by older binaries.
CREATE TABLE IF NOT EXISTS instance_leases (
    id TEXT PRIMARY KEY,
    pid INTEGER NOT NULL,
    host TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    heartbeat_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_instance_leases_heartbeat
    ON instance_leases(heartbeat_at);
