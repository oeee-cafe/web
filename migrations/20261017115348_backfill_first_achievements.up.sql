-- FIRST_DRAWING, FIRST_RELAY and FIRST_COLLABORATION were only ever given by
-- award_achievements (models::achievement) when it ran -- on publishing,
-- saving a collaboration, linking Steam or adopting a guest -- and dated
-- the moment it did. So someone who drew before they existed and has not
-- published since has none, and someone who has them shows the day they
-- next published rather than the day they first did.
--
-- Give them to everyone who qualifies, by award_achievements' own rules,
-- and date each from what earned it: the first published post, the first
-- published post that answers another, and the earliest saved session they
-- owned or drew in (not watched), by its post's publication. A row already
-- there keeps the earlier of the two, and its revoked_at and steam_*
-- columns as they are. New rows are unsynced, so the Steam sync sends them
-- to those who have linked Steam.
WITH earned AS (
  SELECT author_id AS user_id, 'FIRST_DRAWING' AS achievement, min(published_at) AS earned_at
  FROM posts
  WHERE published_at IS NOT NULL
  GROUP BY author_id
  UNION ALL
  SELECT author_id, 'FIRST_RELAY', min(published_at)
  FROM posts
  WHERE published_at IS NOT NULL AND parent_post_id IS NOT NULL
  GROUP BY author_id
  UNION ALL
  SELECT drawer.user_id, 'FIRST_COLLABORATION', min(saved.published_at)
  FROM collaborative_sessions sessions
  JOIN posts saved ON saved.id = sessions.saved_post_id
  CROSS JOIN LATERAL (
    -- The owner, who may not have a participant row, and everyone who drew.
    SELECT sessions.owner_id
    UNION
    SELECT participants.user_id
    FROM collaborative_sessions_participants participants
    WHERE participants.session_id = sessions.id
      AND NOT participants.is_spectator
  ) drawer (user_id)
  WHERE drawer.user_id IS NOT NULL AND saved.published_at IS NOT NULL
  GROUP BY drawer.user_id
)
INSERT INTO user_achievements (user_id, achievement, earned_at)
SELECT user_id, achievement, earned_at FROM earned
ON CONFLICT (user_id, achievement) DO UPDATE
SET earned_at = LEAST(user_achievements.earned_at, EXCLUDED.earned_at);
