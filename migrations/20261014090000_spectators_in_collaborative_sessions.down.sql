-- Somebody who only watched was never in the session as far as anything
-- before this knew.
DELETE FROM collaborative_sessions_participants WHERE is_spectator;
ALTER TABLE collaborative_sessions_participants DROP COLUMN is_spectator;
ALTER TABLE collaborative_sessions DROP COLUMN allow_spectators;
