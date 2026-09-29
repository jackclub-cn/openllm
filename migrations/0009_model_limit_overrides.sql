-- Manual model-level limits. NULL means "use the synced value"; a number
-- replaces it. These survive provider re-syncs so an operator can correct bad
-- upstream or models.dev metadata without editing the database by hand.
ALTER TABLE provider_models ADD COLUMN context_override INTEGER;
ALTER TABLE provider_models ADD COLUMN input_override INTEGER;
ALTER TABLE provider_models ADD COLUMN output_override INTEGER;

-- CallAI's model endpoint omits limit metadata for its GPT-6 family, while
-- models.dev advertises a 1.05M window. The provider's own aggregator caps
-- these models at 400K, and ignoring that made Codex compact too late. Seed a
-- conservative override for existing rows so the fix applies immediately.
UPDATE provider_models
SET context_override = 400000,
    input_override = 400000
WHERE model_name IN (
    'codex-auto-review',
    'gpt-6-astra',
    'gpt-6-luna',
    'gpt-6-sol'
)
AND provider_id IN (
    SELECT id
    FROM providers
    WHERE lower(base_url) LIKE '%callai.one%'
);
