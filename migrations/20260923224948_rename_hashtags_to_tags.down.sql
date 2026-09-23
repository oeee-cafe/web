DO $$
DECLARE
    r record;
BEGIN
    FOR r IN
        SELECT conrelid::regclass AS tbl, conname
        FROM pg_constraint
        WHERE conrelid IN ('tags'::regclass, 'post_tags'::regclass)
    LOOP
        EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I', r.tbl, r.conname,
            regexp_replace(regexp_replace(r.conname, '(^|_)tag_id(_|$)', '\1hashtag_id\2', 'g'),
                '(^|_)tags(_|$)', '\1hashtags\2', 'g'));
    END LOOP;
    FOR r IN
        SELECT c.relname
        FROM pg_index i
        JOIN pg_class c ON c.oid = i.indexrelid
        WHERE i.indrelid IN ('tags'::regclass, 'post_tags'::regclass)
        AND c.relname NOT LIKE '%hashtag%'
    LOOP
        EXECUTE format('ALTER INDEX %I RENAME TO %I', r.relname,
            regexp_replace(regexp_replace(r.relname, '(^|_)tag_id(_|$)', '\1hashtag_id\2', 'g'),
                '(^|_)tags(_|$)', '\1hashtags\2', 'g'));
    END LOOP;
END
$$;

ALTER VIEW tag_stats RENAME COLUMN tag_id TO hashtag_id;
ALTER VIEW tag_stats RENAME TO hashtag_stats;
ALTER TABLE post_tags RENAME COLUMN tag_id TO hashtag_id;
ALTER TABLE post_tags RENAME TO post_hashtags;
ALTER TABLE tags RENAME TO hashtags;
