-- Per-provider request tuning. Both are optional overrides: when unset the
-- gateway keeps its global idle timeout and derived cooldown schedule.
ALTER TABLE providers ADD COLUMN timeout_seconds INTEGER;
ALTER TABLE providers ADD COLUMN cooldown_seconds INTEGER;
