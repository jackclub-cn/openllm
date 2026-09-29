-- Optional automatic model-list sync interval in minutes. NULL or 0 disables it.
ALTER TABLE providers ADD COLUMN models_sync_interval_minutes INTEGER;
-- Last attempt, successful or not, so scheduled failures do not retry every
-- scheduler tick.
ALTER TABLE providers ADD COLUMN models_sync_attempted_at TEXT;
