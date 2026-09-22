-- Sign in with Apple. An Apple identity's subject is the `sub` of Apple's ID
-- token, stable for one person and this site's team.
--
-- The release this replaces never asks for an 'apple' row and lists an
-- account's identities by whatever provider they name, so it runs alongside
-- this one for the length of a deploy.
ALTER TABLE user_identities DROP CONSTRAINT user_identities_provider_check;
ALTER TABLE user_identities
  ADD CONSTRAINT user_identities_provider_check CHECK (provider IN ('steam', 'apple'));
