-- Small key/value settings table for runtime policy that can be changed from
-- the console without restarting the gateway.
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
