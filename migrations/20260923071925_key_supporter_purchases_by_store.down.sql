-- Back to the table 20260923071924_name_supporter_purchases_by_store left:
-- `provider` beside `store`, equal to it, holding the primary key, with
-- `store` keyed by a unique index and the trigger keeping the two together
-- for a release that writes only `provider`.
ALTER TABLE supporter_purchases ADD COLUMN provider text;
UPDATE supporter_purchases SET provider = store;
ALTER TABLE supporter_purchases ALTER COLUMN provider SET NOT NULL;
ALTER TABLE supporter_purchases
  ADD CONSTRAINT supporter_purchases_provider_check
  CHECK (provider IN ('apple', 'microsoft', 'steam'));

ALTER TABLE supporter_purchases DROP CONSTRAINT supporter_purchases_pkey;
CREATE UNIQUE INDEX supporter_purchases_store_key
  ON supporter_purchases (store, owner, product);
ALTER TABLE supporter_purchases ADD PRIMARY KEY (provider, owner, product);

CREATE INDEX supporter_purchases_checked ON supporter_purchases (checked_at)
  WHERE provider = 'apple';

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
