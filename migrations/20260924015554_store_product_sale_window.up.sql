-- When a product is sold: from `sale_starts_at`, until `sale_ends_at`. Either
-- left null is open at that end, so a product with neither is sold for as
-- long as it is on sale, which is every product until now.
--
-- The window narrows `on_sale` and does not replace it. /supporter offers a
-- product that is on sale *and* inside its window; taking it off sale still
-- stops it at once, whatever the window says. Like `on_sale`, the window
-- only decides what is offered -- a purchase made outside it, a refund heard
-- after it, are still the product's (`store_product::packs`).
--
-- Two nullable columns, and nothing reads them in the release serving while
-- this boots, so that release runs on as it was.
ALTER TABLE store_products
  ADD COLUMN sale_starts_at timestamptz,
  ADD COLUMN sale_ends_at timestamptz,
  ADD CONSTRAINT store_products_sale_window_check
    CHECK (sale_starts_at IS NULL OR sale_ends_at IS NULL OR sale_starts_at < sale_ends_at);
