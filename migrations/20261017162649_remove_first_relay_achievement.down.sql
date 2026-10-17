ALTER TABLE user_achievements DROP CONSTRAINT user_achievements_achievement_check;
ALTER TABLE user_achievements ADD CONSTRAINT user_achievements_achievement_check
  CHECK (achievement IN (
    'FIRST_DRAWING', 'FIRST_RELAY', 'FIRST_COLLABORATION', 'STEAM_SUPPORTER',
    'STREAK_7', 'STREAK_30'
  ));

-- Given back to everyone who has published a relay, dated from the first,
-- as 20261017115348_backfill_first_achievements gave it. Unsynced, as
-- award_achievements would have left it.
INSERT INTO user_achievements (user_id, achievement, earned_at)
SELECT author_id, 'FIRST_RELAY', min(published_at)
FROM posts
WHERE published_at IS NOT NULL AND parent_post_id IS NOT NULL
GROUP BY author_id
ON CONFLICT DO NOTHING;
