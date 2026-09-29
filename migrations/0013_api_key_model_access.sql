-- Optional JSON array of model names or glob patterns an API key may call.
-- NULL or [] means unrestricted.
ALTER TABLE api_keys ADD COLUMN allowed_models TEXT;
