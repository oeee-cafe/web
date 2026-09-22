-- STEAM_SUPPORTER: for an account whose linked Steam account bought Oeee Cafe
-- on Steam -- owns it outright, not borrowed through Family Sharing. Granted
-- when that Steam account signs in or is linked, not derived from anything
-- the site stores.
ALTER TABLE user_achievements DROP CONSTRAINT user_achievements_achievement_check;
ALTER TABLE user_achievements ADD CONSTRAINT user_achievements_achievement_check
  CHECK (achievement IN ('FIRST_DRAWING', 'FIRST_RELAY', 'FIRST_COLLABORATION', 'STEAM_SUPPORTER'));
