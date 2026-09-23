ALTER VIEW tag_stats RENAME COLUMN tag_id TO hashtag_id;
ALTER VIEW tag_stats RENAME TO hashtag_stats;

ALTER INDEX idx_post_tags_tag_id RENAME TO idx_post_hashtags_hashtag_id;
ALTER TABLE post_tags RENAME CONSTRAINT post_tags_post_id_fkey TO post_hashtags_post_id_fkey;
ALTER TABLE post_tags RENAME CONSTRAINT post_tags_tag_id_fkey TO post_hashtags_hashtag_id_fkey;
ALTER TABLE post_tags RENAME CONSTRAINT post_tags_pkey TO post_hashtags_pkey;
ALTER TABLE post_tags RENAME COLUMN tag_id TO hashtag_id;
ALTER TABLE post_tags RENAME TO post_hashtags;

ALTER INDEX idx_tags_name RENAME TO idx_hashtags_name;
ALTER TABLE tags RENAME CONSTRAINT tags_name_key TO hashtags_name_key;
ALTER TABLE tags RENAME CONSTRAINT tags_pkey TO hashtags_pkey;
ALTER TABLE tags RENAME TO hashtags;
