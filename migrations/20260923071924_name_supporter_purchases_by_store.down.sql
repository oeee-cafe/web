-- Back to naming purchases by provider alone. `provider` has been kept equal
-- to `store` throughout, so nothing is lost by dropping `store`. A
-- Microsoft Store purchase or mark would not fit the narrower lists, and
-- the release this goes back to could not read one, so the constraints
-- below refuse to come back while there is one.
DROP INDEX supporter_purchases_store_checked;
DROP INDEX supporter_purchases_store_key;
DROP TRIGGER supporter_purchases_store_is_provider ON supporter_purchases;
DROP FUNCTION supporter_purchases_store_is_provider();

ALTER TABLE users DROP CONSTRAINT users_supporter_mark_check;
ALTER TABLE users
  ADD CONSTRAINT users_supporter_mark_check CHECK (supporter_mark IN ('steam', 'apple'));

ALTER TABLE supporter_purchases DROP CONSTRAINT supporter_purchases_provider_check;
ALTER TABLE supporter_purchases
  ADD CONSTRAINT supporter_purchases_provider_check CHECK (provider IN ('steam', 'apple'));

ALTER TABLE supporter_purchases DROP COLUMN store;
