-- Estimated request cost in micro-US dollars (1 USD = 1,000,000).
-- NULL means the provider/model had no usable pricing metadata.
ALTER TABLE usage_logs ADD COLUMN estimated_cost_micros INTEGER;
