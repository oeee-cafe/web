-- Google Play sells the Supporter Pack too, in the Android app, so it joins
-- the stores a purchase, a mark and a catalogue product may name, as
-- `google`. That is also the name of a way of signing in, which is a
-- different thing: a store is not a sign-in (models/supporter.rs).
--
-- Only ever widens a list. The release serving while this boots writes
-- none of these rows with 'google' in them, and every value it does write
-- is still allowed, so it runs on as it was.
ALTER TABLE supporter_purchases DROP CONSTRAINT supporter_purchases_store_check;
ALTER TABLE supporter_purchases
  ADD CONSTRAINT supporter_purchases_store_check
  CHECK (store IN ('apple', 'google', 'microsoft', 'steam'));

ALTER TABLE users DROP CONSTRAINT users_supporter_mark_check;
ALTER TABLE users
  ADD CONSTRAINT users_supporter_mark_check
  CHECK (supporter_mark IN ('apple', 'google', 'microsoft', 'steam'));

ALTER TABLE store_products DROP CONSTRAINT store_products_store_check;
ALTER TABLE store_products
  ADD CONSTRAINT store_products_store_check
  CHECK (store IN ('apple', 'google', 'microsoft', 'steam'));

-- The Google Play recheck's worklist, as supporter_purchases_store_checked
-- is the App Store's: every purchase is asked about again once a day, by
-- its purchase token, oldest check first (google_play::recheck_supporters).
CREATE INDEX supporter_purchases_google_checked ON supporter_purchases (checked_at)
  WHERE store = 'google';
