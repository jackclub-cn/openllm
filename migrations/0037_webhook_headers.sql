-- Optional headers sent with every webhook delivery. Stored as a JSON object
-- of string pairs so receivers can require bearer auth or similar metadata.
ALTER TABLE webhooks ADD COLUMN headers TEXT NOT NULL DEFAULT '{}';
