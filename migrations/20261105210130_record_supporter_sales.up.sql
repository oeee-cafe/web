-- What a Supporter Pack sold for, as the store that sold it reports it
-- (models/supporter.rs, `Sale`). Every column is empty where the store
-- says nothing about money: the Microsoft Store and Steam answer only
-- whether a pack is owned, and a pack granted by hand was never sold.
--
-- Amounts are micros of `currency` -- a millionth of its major unit -- the
-- one scale both the App Store (milliunits) and Google Play (units and
-- nanos) can be written in exactly.
ALTER TABLE supporter_purchases
  -- A purchase in Apple's sandbox or by a Play licence tester. It counts
  -- for its buyer like any other, and for nothing in the takings.
  ADD COLUMN test boolean NOT NULL DEFAULT false,
  -- The store's order, where it has one apart from `owner`: Google Play's
  -- GPA.… id, which is what Play Console and a refund are looked up by.
  ADD COLUMN order_id text,
  ADD COLUMN currency text CHECK (currency ~ '^[A-Z]{3}$'),
  -- What the buyer paid, tax included where the store includes it.
  ADD COLUMN price_micros bigint CHECK (price_micros >= 0),
  -- What the store pays the site for it, where the store says.
  ADD COLUMN proceeds_micros bigint CHECK (proceeds_micros >= 0),
  -- Where it was bought, as the store names it: Apple's storefront is
  -- ISO 3166 alpha-3 (KOR), Google Play's region alpha-2 (KR).
  ADD COLUMN country text,
  ADD CONSTRAINT supporter_purchases_price_has_currency
    CHECK ((price_micros IS NULL) = (currency IS NULL)),
  ADD CONSTRAINT supporter_purchases_proceeds_have_price
    CHECK (proceeds_micros IS NULL OR price_micros IS NOT NULL);
