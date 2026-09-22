-- Supporters: accounts whose linked Steam account owns one of the apps the
-- config names in steam.supporter_app_ids -- the Supporter Pack DLC. They
-- wear a badge beside their name and are thanked in the credits on /about.
--
-- Held on the identity rather than on the account, because the identity is
-- the proof: unlinking Steam, or linking that Steam account to a different
-- Oeee Cafe account, takes the badge with it. supporter_since is NULL for
-- anyone who is not a supporter now -- including someone who was and asked
-- Steam for a refund, which the next ownership check notices.
-- supporter_checked_at is when Steam last answered that question; every
-- linked Steam account is asked again once a day, and NULL is never yet.
--
-- The STEAM_SUPPORTER achievement is separate and is never taken back.
ALTER TABLE user_identities
  ADD COLUMN supporter_since timestamptz,
  ADD COLUMN supporter_checked_at timestamptz;

CREATE INDEX user_identities_supporter_checked ON user_identities (supporter_checked_at)
  WHERE provider = 'steam';

-- Listed in the credits unless they say otherwise. Only ever read for a
-- supporter; everyone else has it and it means nothing.
ALTER TABLE users ADD COLUMN show_in_credits boolean NOT NULL DEFAULT true;
