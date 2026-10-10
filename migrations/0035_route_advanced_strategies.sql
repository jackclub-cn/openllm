-- Preserve the original constrained column for compatibility with existing
-- databases, and keep newly introduced strategy names in an extension column.
ALTER TABLE routes ADD COLUMN strategy_ext TEXT NOT NULL DEFAULT '';
