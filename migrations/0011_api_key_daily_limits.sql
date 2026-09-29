-- Optional daily soft limits per gateway API key. Enforcement uses the UTC
-- calendar day and happens before the request is sent upstream.
ALTER TABLE api_keys ADD COLUMN daily_token_limit INTEGER;
ALTER TABLE api_keys ADD COLUMN daily_cost_limit_micros INTEGER;
