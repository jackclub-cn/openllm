-- Optional per-key routing policy stored as JSON: {"strategy": "...",
-- "provider": "...", "exclude_providers": ["..."]}. Request headers still
-- override individual fields for a single call.
ALTER TABLE api_keys ADD COLUMN routing_policy TEXT;
