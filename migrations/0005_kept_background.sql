-- The picture the stored layout used before the latest upload stays until a
-- saved layout stops using it: a Cancel after an upload must not cost the user
-- the picture they had. `kept_*` describe that picture (the same as `id`, `mime`
-- and `bytes` do for the current one); all three are NULL when none is kept.
ALTER TABLE backgrounds ADD COLUMN kept_id TEXT;
ALTER TABLE backgrounds ADD COLUMN kept_mime TEXT;
ALTER TABLE backgrounds ADD COLUMN kept_bytes INTEGER;
