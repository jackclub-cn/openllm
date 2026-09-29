-- Limits reported by the provider's own /models response.
--
-- models.dev does not know every model (aggregator and private models are
-- common), but providers such as CommandCode return `context_length` for all
-- of their models. Capturing it means those entries still advertise a usable
-- context window instead of no limits at all.
ALTER TABLE provider_models ADD COLUMN upstream_context_limit INTEGER;

-- JSON array of endpoints the upstream says the model serves, e.g.
-- ["/chat/completions","/responses"]. Used to avoid routing a request to a
-- model that cannot serve that endpoint.
ALTER TABLE provider_models ADD COLUMN supported_endpoints TEXT;
