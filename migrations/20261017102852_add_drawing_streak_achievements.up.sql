-- STREAK_7 and STREAK_30: for having drawn seven, and thirty, days in a
-- row. A day is Seoul's and counts when it has a published drawing anyone
-- can see -- outside any community, or in a public one -- which is what the
-- profile's streak line counts (models::post::drawing_streaks). Earned by
-- the longest run there has ever been, so a run that has ended keeps it.
ALTER TABLE user_achievements DROP CONSTRAINT user_achievements_achievement_check;
ALTER TABLE user_achievements ADD CONSTRAINT user_achievements_achievement_check
  CHECK (achievement IN (
    'FIRST_DRAWING', 'FIRST_RELAY', 'FIRST_COLLABORATION', 'STEAM_SUPPORTER',
    'STREAK_7', 'STREAK_30'
  ));

-- Everyone who has already drawn the run, earned on the day it reached the
-- length: the first drawing of its seventh (or thirtieth) day. Unsynced, so
-- the Steam sync sends them to those who have linked Steam.
WITH days AS (
  SELECT
    posts.author_id,
    (posts.published_at AT TIME ZONE 'Asia/Seoul')::date AS day,
    min(posts.published_at) AS first_published_at
  FROM posts
  JOIN images ON posts.image_id = images.id
  LEFT JOIN communities ON posts.community_id = communities.id
  WHERE posts.published_at IS NOT NULL
    AND posts.deleted_at IS NULL
    AND (posts.community_id IS NULL OR communities.visibility = 'public')
  GROUP BY 1, 2
), numbered AS (
  -- Days of one run share day minus its place in the author's days.
  SELECT
    author_id,
    day,
    first_published_at,
    day - (row_number() OVER (PARTITION BY author_id ORDER BY day))::int AS run
  FROM days
), placed AS (
  SELECT
    author_id,
    first_published_at,
    row_number() OVER (PARTITION BY author_id, run ORDER BY day) AS nth
  FROM numbered
)
INSERT INTO user_achievements (user_id, achievement, earned_at)
SELECT placed.author_id, goal.achievement, min(placed.first_published_at)
FROM placed
JOIN (VALUES ('STREAK_7', 7), ('STREAK_30', 30)) AS goal (achievement, days)
  ON placed.nth = goal.days
GROUP BY placed.author_id, goal.achievement
ON CONFLICT DO NOTHING;
