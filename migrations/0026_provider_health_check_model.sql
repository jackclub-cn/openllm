-- Optional model used by provider health checks. When empty, the gateway
-- falls back to the first enabled synced model.
ALTER TABLE providers ADD COLUMN health_check_model TEXT;
