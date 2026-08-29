-- Put the column back and fill it from the view, so a release rolled back to
-- one that reads it sees the right numbers rather than a table of zeroes.
--
-- Nothing keeps it true after this point, which is the whole reason it went
-- away: it starts drifting again with the next publish.
ALTER TABLE hashtags ADD COLUMN post_count int NOT NULL DEFAULT 0;

UPDATE hashtags h
SET post_count = COALESCE(
    (SELECT s.post_count FROM hashtag_stats s WHERE s.hashtag_id = h.id), 0
);
