-- The column comes back empty: what it held was who supported and since
-- when, which supporter_purchases has held since the deploy before this one.
ALTER TABLE user_identities ADD COLUMN supporter_since timestamptz;
