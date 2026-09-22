-- Achievements: a first drawing, a first relay, a first collaboration.
--
-- Earned on the site, whatever it was drawn in, and kept here whether or not
-- the account has Steam; an account that links Steam later is given what it
-- already earned. steam_synced_at is when Steam last accepted it for the
-- account's linked Steam account, and NULL until then -- a background task
-- works through the NULLs, so an unreachable Steam delays an achievement and
-- never loses one.
--
-- `achievement` is text under a CHECK for the same reason
-- user_identities.provider is: a new achievement is a constraint swapped in a
-- migration. The names are the achievements' API names in Steamworks.
CREATE TABLE user_achievements (
  user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  achievement text NOT NULL CONSTRAINT user_achievements_achievement_check
    CHECK (achievement IN ('FIRST_DRAWING', 'FIRST_RELAY', 'FIRST_COLLABORATION')),
  earned_at timestamptz NOT NULL DEFAULT now(),
  steam_synced_at timestamptz,
  PRIMARY KEY (user_id, achievement)
);

CREATE INDEX user_achievements_unsynced ON user_achievements (earned_at)
  WHERE steam_synced_at IS NULL;
