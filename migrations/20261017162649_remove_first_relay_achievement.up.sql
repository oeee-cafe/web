-- FIRST_RELAY, for drawing onto someone else's drawing, is gone: it paid
-- people to draw over others' work just to unlock it. Relays themselves stay.
--
-- The rows go with it, synced or not, so it leaves every profile and the
-- Steam sync has nothing of it to send. Steam is not told to lock it: the
-- achievement is being deleted from Steamworks, which takes it from every
-- Steam account there, and a name Steamworks no longer knows would fail
-- SetUserStatsForGame and be retried for ever.
DELETE FROM user_achievements WHERE achievement = 'FIRST_RELAY';
ALTER TABLE user_achievements DROP CONSTRAINT user_achievements_achievement_check;
ALTER TABLE user_achievements ADD CONSTRAINT user_achievements_achievement_check
  CHECK (achievement IN (
    'FIRST_DRAWING', 'FIRST_COLLABORATION', 'STEAM_SUPPORTER',
    'STREAK_7', 'STREAK_30'
  ));
