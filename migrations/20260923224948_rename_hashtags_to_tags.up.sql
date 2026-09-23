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
ALTER TABLE tags RENAME CONSTRAINT hashtags_pkey TO tags_pkey;
ALTER TABLE tags RENAME CONSTRAINT hashtags_name_key TO tags_name_key;
ALTER INDEX idx_hashtags_name RENAME TO idx_tags_name;

ALTER TABLE post_hashtags RENAME TO post_tags;
ALTER TABLE post_tags RENAME COLUMN hashtag_id TO tag_id;
ALTER TABLE post_tags RENAME CONSTRAINT post_hashtags_pkey TO post_tags_pkey;
ALTER TABLE post_tags RENAME CONSTRAINT post_hashtags_hashtag_id_fkey TO post_tags_tag_id_fkey;
ALTER TABLE post_tags RENAME CONSTRAINT post_hashtags_post_id_fkey TO post_tags_post_id_fkey;
ALTER INDEX idx_post_hashtags_hashtag_id RENAME TO idx_post_tags_tag_id;

ALTER VIEW hashtag_stats RENAME TO tag_stats;
ALTER VIEW tag_stats RENAME COLUMN hashtag_id TO tag_id;
