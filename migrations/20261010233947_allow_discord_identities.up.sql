-- Sign in with Discord. A Discord identity's subject is the account's id
-- (a snowflake, as text), which Discord's /users/@me answers with: stable
-- for one account whatever its username becomes.
--
-- The release this replaces never asks for a 'discord' row and lists an
-- account's identities by whatever provider they name, so it runs alongside
-- this one for the length of a deploy.
ALTER TABLE user_identities DROP CONSTRAINT user_identities_provider_check;
ALTER TABLE user_identities
  ADD CONSTRAINT user_identities_provider_check CHECK (provider IN ('steam', 'apple', 'google', 'discord'));
