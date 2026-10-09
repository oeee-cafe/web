DROP INDEX user_achievements_steam_checked;
DELETE FROM user_achievements WHERE revoked_at IS NOT NULL;
ALTER TABLE user_achievements
  DROP COLUMN steam_owner,
  DROP COLUMN steam_checked_at,
  DROP COLUMN revoked_at;
