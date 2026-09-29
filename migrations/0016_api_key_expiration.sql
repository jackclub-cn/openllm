-- Optional UTC expiration timestamp. NULL means the key does not expire.
ALTER TABLE api_keys ADD COLUMN expires_at TEXT;
