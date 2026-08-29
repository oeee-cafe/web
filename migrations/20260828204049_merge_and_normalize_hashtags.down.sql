-- Only the schema comes back. Names normalised and rows merged by the up
-- migration cannot be unmerged: which of `#Art`, `art` and `ART` a given post
-- was tagged with is not recorded anywhere once they are one row.
DROP VIEW IF EXISTS hashtag_stats;

CREATE INDEX idx_hashtags_name ON hashtags(name);
CREATE INDEX idx_hashtags_post_count ON hashtags(post_count DESC);
CREATE INDEX idx_post_hashtags_post_id ON post_hashtags(post_id);

COMMENT ON COLUMN hashtags.post_count IS NULL;

ALTER TABLE post_hashtags DROP COLUMN position;
