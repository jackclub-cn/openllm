-- Final phase timings and retry totals, without request or response content.
ALTER TABLE usage_logs ADD COLUMN performance_json TEXT;
