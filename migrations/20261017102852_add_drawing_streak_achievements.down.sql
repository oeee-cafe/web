DELETE FROM user_achievements WHERE achievement IN ('STREAK_7', 'STREAK_30');
ALTER TABLE user_achievements DROP CONSTRAINT user_achievements_achievement_check;
ALTER TABLE user_achievements ADD CONSTRAINT user_achievements_achievement_check
  CHECK (achievement IN ('FIRST_DRAWING', 'FIRST_RELAY', 'FIRST_COLLABORATION', 'STEAM_SUPPORTER'));
