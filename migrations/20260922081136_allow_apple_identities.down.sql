DELETE FROM user_identities WHERE provider = 'apple';
ALTER TABLE user_identities DROP CONSTRAINT user_identities_provider_check;
ALTER TABLE user_identities
  ADD CONSTRAINT user_identities_provider_check CHECK (provider IN ('steam'));
