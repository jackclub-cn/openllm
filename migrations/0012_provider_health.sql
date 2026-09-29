-- Most recent connection test result, kept for the provider list after the
-- immediate toast is gone.
ALTER TABLE providers ADD COLUMN last_test_at TEXT;
ALTER TABLE providers ADD COLUMN last_test_ok INTEGER;
ALTER TABLE providers ADD COLUMN last_test_latency_ms INTEGER;
ALTER TABLE providers ADD COLUMN last_test_checked TEXT;
ALTER TABLE providers ADD COLUMN last_test_message TEXT;
