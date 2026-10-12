-- Sessions a guest opened, and every guest's seat, have nowhere to go
-- without the guest table.
DROP TABLE collaborative_session_bans;

DELETE FROM collaborative_sessions WHERE owner_guest_id IS NOT NULL;
DROP INDEX idx_collab_sessions_owner_guest;
ALTER TABLE collaborative_sessions
  DROP COLUMN allow_guests,
  DROP COLUMN owner_participant_id,
  DROP CONSTRAINT ck_collaborative_sessions_one_owner,
  DROP COLUMN owner_guest_id;
ALTER TABLE collaborative_sessions ALTER COLUMN owner_id SET NOT NULL;

DELETE FROM collaborative_sessions_participants WHERE guest_id IS NOT NULL;
DROP INDEX idx_collab_participants_guest;
ALTER TABLE collaborative_sessions_participants
  DROP CONSTRAINT collaborative_sessions_participants_session_participant_key,
  ADD CONSTRAINT collaborative_sessions_participants_session_id_user_id_key
    UNIQUE (session_id, user_id);
ALTER TABLE collaborative_sessions_participants
  DROP COLUMN participant_id,
  DROP CONSTRAINT ck_collaborative_sessions_participants_one_identity,
  DROP COLUMN guest_id;
ALTER TABLE collaborative_sessions_participants ALTER COLUMN user_id SET NOT NULL;

DROP TABLE collaborative_guests;
