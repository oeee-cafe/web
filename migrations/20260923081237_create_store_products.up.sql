-- What each store sells as a Supporter Pack, and which year each one is for.
--
-- Until now the packs were listed in the server's config
-- (`[[app_store.supporter_products]]`, `[[steam.supporter_apps]]`), which
-- meant putting a new year on sale, or taking one off, was an edit on the
-- server and a deploy. They live here instead, and /admin/store changes them.
--
-- `product` is the store's own id for the thing it sells: an App Store
-- product id, a Microsoft Store ID, or a Steam app id written out. `year` is
-- the Supporter Pack year a purchase of it counts for, and is what a
-- purchase is credited with (`supporter_purchases.year`).
--
-- A row is never deleted. A product counts for whoever bought it whether or
-- not it is still sold -- a delisted DLC stays owned, and a refund of one
-- still has to be heard -- so taking a product off sale is `on_sale = false`,
-- which only stops /supporter from offering it.
--
-- Nothing reads this table in the release that is serving while this boots,
-- so creating it leaves that release as it was. The new release fills it
-- from its config on boot (`store_product::import_configured`) before it
-- serves anything.
--
-- `product` is checked for whitespace in Rust rather than here: `\s` is a
-- POSIX class under another name, and those are the host C library's to
-- answer (CLAUDE.md).
CREATE TABLE store_products (
  store text NOT NULL CHECK (store IN ('apple', 'microsoft', 'steam')),
  product text NOT NULL CHECK (product <> ''),
  year integer NOT NULL,
  label text,
  on_sale boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (store, product)
);
