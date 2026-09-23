-- A purchase is named by the store that sold it, not by a sign-in provider.
--
-- `provider` was the word for both, because until now the two stores that
-- sold a pack were also two ways of signing in. They are not the same thing:
-- Google signs people in and sells nothing, the Microsoft Store will sell and
-- signs nobody in, and even where the names agree the App Store's `owner` is
-- a transaction and not an Apple ID. So the column that says where a pack
-- was bought is `store`, and `provider` is left to user_identities.
--
-- This is the first half, for one deploy. The release serving while this
-- one boots writes `provider` and nothing else, and says ON CONFLICT
-- (provider, owner, product); this one writes `store` and nothing else, and
-- says ON CONFLICT (store, owner, product). Both run at once for the length
-- of a deploy, so the table has to answer both:
--
-- - the two columns are kept equal by a trigger, whichever one a writer
--   filled in, so every row has both whichever release wrote it;
-- - the primary key stays on `provider`, and `store` gets a unique index of
--   its own that the new release's ON CONFLICT can name.
--
-- 20260923071925_key_supporter_purchases_by_store drops `provider`, the
-- trigger and the old key a deploy later, once nothing serving writes it.

ALTER TABLE supporter_purchases ADD COLUMN store text;
UPDATE supporter_purchases SET store = provider;
ALTER TABLE supporter_purchases ALTER COLUMN store SET NOT NULL;
ALTER TABLE supporter_purchases
  ADD CONSTRAINT supporter_purchases_store_check
  CHECK (store IN ('apple', 'microsoft', 'steam'));

-- The trigger copies a Microsoft Store purchase into `provider` as well, so
-- that list has to allow it for as long as the column is there.
ALTER TABLE supporter_purchases DROP CONSTRAINT supporter_purchases_provider_check;
ALTER TABLE supporter_purchases
  ADD CONSTRAINT supporter_purchases_provider_check
  CHECK (provider IN ('apple', 'microsoft', 'steam'));

-- A mark is the store a supporter bought this year's pack in, compared with
-- `store` above, so the two lists name the same stores (a test in
-- models/supporter.rs holds them together). The previous release only ever
-- writes 'steam' or 'apple' here, both of which are still allowed.
ALTER TABLE users DROP CONSTRAINT users_supporter_mark_check;
ALTER TABLE users
  ADD CONSTRAINT users_supporter_mark_check
  CHECK (supporter_mark IN ('apple', 'microsoft', 'steam'));

-- A BEFORE trigger runs before the NOT NULL checks and before INSERT ... ON
-- CONFLICT looks for a conflict, so a row arriving with only one of the two
-- is whole by the time either is asked about. Nothing changes a purchase's
-- store after it is made, but an update that did would carry the other
-- column with it rather than leave the two disagreeing.
CREATE FUNCTION supporter_purchases_store_is_provider() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF NEW.store IS DISTINCT FROM OLD.store THEN
      NEW.provider := NEW.store;
    ELSIF NEW.provider IS DISTINCT FROM OLD.provider THEN
      NEW.store := NEW.provider;
    END IF;
  ELSE
    NEW.store := COALESCE(NEW.store, NEW.provider);
    NEW.provider := COALESCE(NEW.provider, NEW.store);
  END IF;
  IF NEW.store IS DISTINCT FROM NEW.provider THEN
    RAISE EXCEPTION 'a supporter purchase from store % cannot name provider %',
      NEW.store, NEW.provider;
  END IF;
  RETURN NEW;
END
$$;

CREATE TRIGGER supporter_purchases_store_is_provider
  BEFORE INSERT OR UPDATE ON supporter_purchases
  FOR EACH ROW EXECUTE FUNCTION supporter_purchases_store_is_provider();

-- What the new release's ON CONFLICT names. It becomes the primary key when
-- `provider` goes.
CREATE UNIQUE INDEX supporter_purchases_store_key
  ON supporter_purchases (store, owner, product);

-- The App Store recheck's worklist, as supporter_purchases_checked is, asked
-- by store. The old index stays for the release still asking by provider.
CREATE INDEX supporter_purchases_store_checked ON supporter_purchases (checked_at)
  WHERE store = 'apple';
