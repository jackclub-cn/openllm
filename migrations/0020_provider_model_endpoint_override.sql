-- Manual correction for providers whose /models response omits or
-- misreports the endpoints a model supports. NULL keeps the synchronized
-- value, while a JSON array overrides it for routing and /v1/models.
ALTER TABLE provider_models ADD COLUMN supported_endpoints_override TEXT;
