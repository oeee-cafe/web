-- Rename hashtags to tags, to match what the site now calls them.
--
-- Done in one step, so the colour still serving during the deploy loses its
-- tables when this runs and errors on tag queries until the switch. That
-- downtime was accepted rather than splitting this across two deploys behind
-- compatibility views.
--
-- hashtag_stats follows its tables by OID, so only its own name and its
-- column need renaming.
ALTER TABLE hashtags RENAME TO tags;
ALTER TABLE post_hashtags RENAME TO post_tags;
ALTER TABLE post_tags RENAME COLUMN hashtag_id TO tag_id;
ALTER VIEW hashtag_stats RENAME TO tag_stats;
ALTER VIEW tag_stats RENAME COLUMN hashtag_id TO tag_id;

-- Constraints and indexes by what the catalog holds rather than by a list:
-- PostgreSQL 18 names NOT NULL constraints (hashtags_name_not_null) and 17
-- does not, and merge_and_normalize_hashtags dropped idx_hashtags_name, which
-- a list written against a local database still had. Constraints go first,
-- since renaming a primary key or unique constraint renames its index too.
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN
        SELECT conrelid::regclass AS tbl, conname
        FROM pg_constraint
        WHERE conrelid IN ('tags'::regclass, 'post_tags'::regclass)
        AND conname LIKE '%hashtag%'
    LOOP
        EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I', r.tbl, r.conname,
            replace(replace(r.conname, 'hashtag_id', 'tag_id'), 'hashtags', 'tags'));
    END LOOP;
    FOR r IN
        SELECT c.relname
        FROM pg_index i
        JOIN pg_class c ON c.oid = i.indexrelid
        WHERE i.indrelid IN ('tags'::regclass, 'post_tags'::regclass)
        AND c.relname LIKE '%hashtag%'
    LOOP
        EXECUTE format('ALTER INDEX %I RENAME TO %I', r.relname,
            replace(replace(r.relname, 'hashtag_id', 'tag_id'), 'hashtags', 'tags'));
    END LOOP;
END
$$;
