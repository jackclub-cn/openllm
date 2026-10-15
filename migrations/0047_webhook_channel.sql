-- Alert-channel rendering for webhooks.
--
-- A webhook began as a generic JSON event sink: every delivery posted the
-- gateway's own envelope to whatever URL the operator configured. That suits an
-- integration the operator writes themselves, but a chat channel (Slack,
-- Discord, Telegram, Feishu, DingTalk) only accepts its own body shape, so
-- pointing a webhook at one produced a message the channel rejects and a
-- delivery history full of client errors.
--
-- `kind` selects the body shape. `generic` keeps the original envelope and
-- stays the default, so existing rows keep delivering exactly as before.
-- `body_fields` carries the extra static fields a channel needs that are not
-- derived from the event (Telegram's `chat_id`, a DingTalk @-mention list) and
-- is merged into the rendered body.
ALTER TABLE webhooks ADD COLUMN kind TEXT NOT NULL DEFAULT 'generic';
ALTER TABLE webhooks ADD COLUMN body_fields TEXT NOT NULL DEFAULT '{}';
