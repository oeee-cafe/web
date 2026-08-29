-- Hashtags: normalise the names, merge the rows that then collide, and stop
-- storing a post count that nothing could keep true.
--
-- Three separate problems shared one root: `hashtags.name` was whatever the
-- user typed with `-` turned into `_` and lowercased, so `#art` (leading hash
-- kept), `art` and `ART` were three different tags; and `post_count` was a
-- counter incremented by two call sites and decremented by two others, none of
-- which saw a cascade delete, a community moving, or a post being restored.

-- 1. Tag ordering. Every row a post inserted got `CURRENT_TIMESTAMP`, which is
-- transaction time and therefore identical for all of them, so ordering by it
-- returned the tags in whatever order the heap felt like. Existing rows all
-- collapse to 0, which keeps them exactly as (un)ordered as they are now.
ALTER TABLE post_hashtags ADD COLUMN position smallint NOT NULL DEFAULT 0;

-- 2. Normalise names to what parse_hashtag_input in src/models/hashtag.rs now
-- produces: `-` to `_`, drop everything that is not a letter, digit or `_`
-- (this is where a leading `#` goes), lowercase, cap at 60 characters.
-- `[[:alnum:]]` follows the database's ctype, so CJK and Hangul survive it the
-- same way Rust's char::is_alphanumeric does.
CREATE TEMPORARY TABLE hashtag_renames AS
SELECT
    id,
    left(
        regexp_replace(replace(display_name, '-', '_'), '[^[:alnum:]_]', '', 'g'),
        60
    ) AS new_display_name,
    lower(left(
        regexp_replace(replace(name, '-', '_'), '[^[:alnum:]_]', '', 'g'),
        60
    )) AS new_name
FROM hashtags;

-- 3. A tag whose name was entirely punctuation or emoji has nothing left to be
-- called. Its links go; the posts themselves are untouched.
DELETE FROM post_hashtags ph
USING hashtag_renames r
WHERE ph.hashtag_id = r.id AND r.new_name = '';
DELETE FROM hashtags h
USING hashtag_renames r
WHERE h.id = r.id AND r.new_name = '';
DELETE FROM hashtag_renames WHERE new_name = '';

-- 4. Merge. Of every group that normalises to the same name the oldest row
-- survives (its display_name is the casing the tag was first written in, which
-- is the one the site has been showing); the rest have their posts moved onto
-- the survivor and are deleted. `ON CONFLICT DO NOTHING` collapses a post that
-- carried two spellings of one tag down to a single link.
CREATE TEMPORARY TABLE hashtag_merges AS
SELECT
    r.id AS loser_id,
    first_value(r.id) OVER (
        PARTITION BY r.new_name ORDER BY h.created_at, r.id
    ) AS winner_id
FROM hashtag_renames r
JOIN hashtags h ON h.id = r.id;
DELETE FROM hashtag_merges WHERE loser_id = winner_id;

INSERT INTO post_hashtags (post_id, hashtag_id, created_at)
SELECT ph.post_id, m.winner_id, ph.created_at
FROM post_hashtags ph
JOIN hashtag_merges m ON m.loser_id = ph.hashtag_id
ON CONFLICT (post_id, hashtag_id) DO NOTHING;

DELETE FROM hashtags h USING hashtag_merges m WHERE h.id = m.loser_id;

-- 5. Rename the survivors. No two of them normalise to the same name any more,
-- and normalisation is idempotent, so no row can collide with one this
-- statement has not reached yet.
UPDATE hashtags h
SET name = r.new_name,
    -- A display_name that was pure punctuation leaves nothing to show; fall
    -- back to the normalised name rather than rendering an empty tag.
    display_name = COALESCE(NULLIF(r.new_display_name, ''), r.new_name)
FROM hashtag_renames r
WHERE h.id = r.id
  AND (h.name <> r.new_name OR h.display_name <> r.new_display_name);

DROP TABLE hashtag_renames;
DROP TABLE hashtag_merges;

-- 6. The post count, derived. This is the one definition of which posts a tag
-- counts and lists, and it matches the site's other public feeds: published,
-- not deleted, and either in a public community or in none at all (a personal
-- post — previously excluded by an inner join, so tags used only on personal
-- posts counted zero and their pages showed nothing).
CREATE VIEW hashtag_stats AS
SELECT
    ph.hashtag_id,
    count(*)::int AS post_count,
    count(*) FILTER (
        WHERE p.published_at > now() - interval '7 days'
    )::int AS recent_post_count,
    max(p.published_at) AS last_posted_at
FROM post_hashtags ph
JOIN posts p ON p.id = ph.post_id
LEFT JOIN communities c ON c.id = p.community_id
WHERE p.published_at IS NOT NULL
  AND p.deleted_at IS NULL
  AND (c.visibility = 'public' OR p.community_id IS NULL)
GROUP BY ph.hashtag_id;

-- 7. Leave `hashtags.post_count` in place but stop believing it. The release
-- being replaced is still serving while this runs and still selects the column,
-- so it cannot be dropped until the deploy after this one; back it off the
-- derived numbers so what that release shows for the length of a deploy is at
-- least right.
UPDATE hashtags h
SET post_count = COALESCE(
    (SELECT s.post_count FROM hashtag_stats s WHERE s.hashtag_id = h.id), 0
);

COMMENT ON COLUMN hashtags.post_count IS
    'Deprecated and no longer maintained: use the hashtag_stats view. Kept only so the previous release survives a blue/green deploy; drop it in the next one.';

-- 8. `hashtags_name_key` (the UNIQUE constraint) already indexes name, and
-- post_hashtags' primary key already leads with post_id, so both of these were
-- second copies of an index that existed.
DROP INDEX idx_hashtags_name;
DROP INDEX idx_post_hashtags_post_id;

-- Ranking now reads the view rather than the column, so an index on the column
-- ranks by a number nothing updates.
DROP INDEX idx_hashtags_post_count;
