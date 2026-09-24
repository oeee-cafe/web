-- Back to the three stores. A Google Play purchase, mark or product would
-- not fit the narrower lists, and the release this goes back to could not
-- read one, so the constraints below refuse to come back while there is one.
DROP INDEX supporter_purchases_google_checked;

ALTER TABLE store_products DROP CONSTRAINT store_products_store_check;
ALTER TABLE store_products
  ADD CONSTRAINT store_products_store_check
  CHECK (store IN ('apple', 'microsoft', 'steam'));

ALTER TABLE users DROP CONSTRAINT users_supporter_mark_check;
ALTER TABLE users
  ADD CONSTRAINT users_supporter_mark_check
  CHECK (supporter_mark IN ('apple', 'microsoft', 'steam'));

ALTER TABLE supporter_purchases DROP CONSTRAINT supporter_purchases_store_check;
ALTER TABLE supporter_purchases
  ADD CONSTRAINT supporter_purchases_store_check
  CHECK (store IN ('apple', 'microsoft', 'steam'));
