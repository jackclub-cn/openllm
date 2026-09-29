-- Prompt-cache accounting.
--
-- Providers report cache traffic in different places:
--   OpenAI-compatible: usage.prompt_tokens_details.cached_tokens (read) and
--                      cache_write_tokens / cache_creation_input_tokens (write)
--   Anthropic:         usage.cache_read_input_tokens / cache_creation_input_tokens
--
-- Cache reads are billed far below fresh input tokens, so separating them makes
-- cost reporting meaningful instead of hiding everything in prompt_tokens.
ALTER TABLE usage_logs ADD COLUMN cache_read_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usage_logs ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0;
