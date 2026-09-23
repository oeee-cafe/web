-- The release before this one lists its packs in config and never reads the
-- table, so dropping it takes nothing that release needs. Whatever was added
-- at /admin/store alone goes with it, and has to be written into the config
-- to be sold again.
DROP TABLE store_products;
