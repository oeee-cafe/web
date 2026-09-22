ALTER TABLE users DROP COLUMN show_in_credits;
DROP INDEX user_identities_supporter_checked;
ALTER TABLE user_identities
  DROP COLUMN supporter_checked_at,
  DROP COLUMN supporter_since;
