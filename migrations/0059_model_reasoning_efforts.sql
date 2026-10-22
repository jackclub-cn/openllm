-- models.dev publishes accepted effort values separately from the boolean
-- reasoning flag. Stores a JSON array so runtime compatibility can map
-- unsupported client values without making a catalog request in the hot path.
ALTER TABLE provider_models ADD COLUMN reasoning_efforts TEXT;
