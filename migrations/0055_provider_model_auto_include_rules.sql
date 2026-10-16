-- Per-provider rules for automatic and scheduled model discovery. Manual sync
-- still performs a full replacement; these rules only constrain incremental
-- syncs so a provider refresh cannot delete curated or disabled models.
ALTER TABLE providers
    ADD COLUMN model_auto_include_rules TEXT NOT NULL DEFAULT '[]';
