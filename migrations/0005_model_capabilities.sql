-- models.dev provider id used to look up capability metadata. NULL means the
-- provider could not be matched and no capabilities are recorded for it.
ALTER TABLE providers ADD COLUMN models_dev_id TEXT;

-- Capability metadata mirrored from models.dev for each upstream model.
-- All of these are nullable: an unknown value must stay distinguishable from
-- a known-false one so intersections do not silently invent limits.
ALTER TABLE provider_models ADD COLUMN context_limit INTEGER;
ALTER TABLE provider_models ADD COLUMN output_limit INTEGER;
ALTER TABLE provider_models ADD COLUMN input_limit INTEGER;
ALTER TABLE provider_models ADD COLUMN attachment INTEGER;
ALTER TABLE provider_models ADD COLUMN reasoning INTEGER;
ALTER TABLE provider_models ADD COLUMN tool_call INTEGER;
ALTER TABLE provider_models ADD COLUMN structured_output INTEGER;
ALTER TABLE provider_models ADD COLUMN temperature INTEGER;
ALTER TABLE provider_models ADD COLUMN open_weights INTEGER;
ALTER TABLE provider_models ADD COLUMN modalities TEXT;
ALTER TABLE provider_models ADD COLUMN cost TEXT;
ALTER TABLE provider_models ADD COLUMN family TEXT;
ALTER TABLE provider_models ADD COLUMN knowledge TEXT;
ALTER TABLE provider_models ADD COLUMN release_date TEXT;
ALTER TABLE provider_models ADD COLUMN last_updated TEXT;
ALTER TABLE provider_models ADD COLUMN canonical_model_id TEXT;
ALTER TABLE provider_models ADD COLUMN capabilities_synced_at TEXT;
