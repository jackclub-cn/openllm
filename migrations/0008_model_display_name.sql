-- Human-friendly model label reported by the provider's own /models response,
-- e.g. "DeepSeek V4.1 Flash" for "deepseek/deepseek-v4.1-flash".
--
-- Anthropic's model list exposes this as `display_name`, and clients such as
-- Claude Code show it in their model picker, so it is worth keeping rather
-- than falling back to the raw upstream id.
ALTER TABLE provider_models ADD COLUMN display_name TEXT;
