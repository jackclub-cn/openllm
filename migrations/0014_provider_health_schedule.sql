-- Optional automatic health-check interval in minutes. NULL or 0 disables it.
ALTER TABLE providers ADD COLUMN health_check_interval_minutes INTEGER;
