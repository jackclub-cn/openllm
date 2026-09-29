-- Manual USD-per-million-token price corrections for provider models.
-- NULL keeps the synchronized models.dev price for that billing category.
ALTER TABLE provider_models ADD COLUMN cost_input_override REAL;
ALTER TABLE provider_models ADD COLUMN cost_output_override REAL;
ALTER TABLE provider_models ADD COLUMN cost_cache_read_override REAL;
ALTER TABLE provider_models ADD COLUMN cost_cache_write_override REAL;
