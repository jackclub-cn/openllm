-- Tracks whether an OpenAI-compatible provider accepts the Responses
-- `tool_search` tool. A rejected request flips this off automatically.
ALTER TABLE providers
    ADD COLUMN tool_search_supported INTEGER NOT NULL DEFAULT 1;
