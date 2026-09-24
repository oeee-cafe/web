ALTER TABLE store_products
  DROP CONSTRAINT store_products_sale_window_check,
  DROP COLUMN sale_ends_at,
  DROP COLUMN sale_starts_at;
