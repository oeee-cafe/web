-- Somebody drawing in a collaborative session without an account: a name
-- they chose and an id their browser session holds. Kept apart from `users`
-- on purpose -- a user has a profile, is an ActivityPub actor and is found
-- by search, and a guest is none of those.
--
-- `claimed_by` is the account they went on to sign in to. Their sessions
-- move to it then (`collaborate::guest::adopt`); the row stays, so that a
-- recording made while they were a guest still has a name to show.
CREATE TABLE collaborative_guests (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  nickname TEXT NOT NULL CHECK (char_length(nickname) BETWEEN 1 AND 30),
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  claimed_by UUID REFERENCES users(id) ON DELETE SET NULL,
  claimed_at TIMESTAMPTZ
);
CREATE INDEX idx_collaborative_guests_claimed_by ON collaborative_guests(claimed_by)
  WHERE claimed_by IS NOT NULL;

-- A participant is an account or a guest, never both. `participant_id` is
-- whichever it is: the id the wire protocol carries, the key the room's
-- Redis seats are kept under, and what a row is unique by.
ALTER TABLE collaborative_sessions_participants
  ALTER COLUMN user_id DROP NOT NULL,
  ADD COLUMN guest_id UUID REFERENCES collaborative_guests(id) ON DELETE CASCADE,
  ADD CONSTRAINT ck_collaborative_sessions_participants_one_identity
    CHECK (num_nonnulls(user_id, guest_id) = 1),
  ADD COLUMN participant_id UUID GENERATED ALWAYS AS (COALESCE(user_id, guest_id)) STORED NOT NULL;
ALTER TABLE collaborative_sessions_participants
  DROP CONSTRAINT collaborative_sessions_participants_session_id_user_id_key,
  ADD CONSTRAINT collaborative_sessions_participants_session_participant_key
    UNIQUE (session_id, participant_id);
CREATE INDEX idx_collab_participants_guest ON collaborative_sessions_participants(guest_id)
  WHERE guest_id IS NOT NULL;

-- The same for whoever opened the session. `allow_guests` is the owner's
-- to turn off; it starts on.
ALTER TABLE collaborative_sessions
  ALTER COLUMN owner_id DROP NOT NULL,
  ADD COLUMN owner_guest_id UUID REFERENCES collaborative_guests(id),
  ADD CONSTRAINT ck_collaborative_sessions_one_owner
    CHECK (num_nonnulls(owner_id, owner_guest_id) = 1),
  ADD COLUMN owner_participant_id UUID GENERATED ALWAYS AS (COALESCE(owner_id, owner_guest_id)) STORED NOT NULL,
  ADD COLUMN allow_guests BOOLEAN NOT NULL DEFAULT TRUE;
CREATE INDEX idx_collab_sessions_owner_guest ON collaborative_sessions(owner_guest_id)
  WHERE owner_guest_id IS NOT NULL;

-- Somebody the owner sent out of a session, who may not come back into it.
-- `participant_id` is an account's or a guest's id. A guest can become a
-- new guest by clearing a cookie, so the address a guest was banned from is
-- kept too, and refuses guests -- never accounts -- arriving from it.
CREATE TABLE collaborative_session_bans (
  session_id UUID NOT NULL REFERENCES collaborative_sessions(id) ON DELETE CASCADE,
  participant_id UUID NOT NULL,
  ip TEXT,
  banned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (session_id, participant_id)
);
