-- Context category token counts are persisted without prompt or tool content.
ALTER TABLE usage_logs ADD COLUMN context_breakdown_json TEXT;
ALTER TABLE usage_logs ADD COLUMN cache_usage_reported INTEGER;
