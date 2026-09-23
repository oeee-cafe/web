-- The second half of 20260923071924_name_supporter_purchases_by_store, a
-- deploy later.
--
-- That migration named purchases by `store` and kept `provider` beside it,
-- equal by a trigger, for the release that was still serving while it
-- booted and wrote nothing else. Every release since writes and reads
-- `store` alone -- including the one serving while this boots, whose ON
-- CONFLICT (store, owner, product) the new primary key answers exactly as
-- the unique index it replaces did -- so `provider` goes, and with it the
-- trigger, the old key and the old recheck index.
DROP TRIGGER supporter_purchases_store_is_provider ON supporter_purchases;
DROP FUNCTION supporter_purchases_store_is_provider();

ALTER TABLE supporter_purchases DROP CONSTRAINT supporter_purchases_pkey;
DROP INDEX supporter_purchases_checked;
ALTER TABLE supporter_purchases DROP COLUMN provider;

-- The unique index becomes the key in place, and takes the key's name.
ALTER TABLE supporter_purchases
  ADD CONSTRAINT supporter_purchases_pkey PRIMARY KEY USING INDEX supporter_purchases_store_key;
