-- Back to the names feder gave its tables.
ALTER TABLE IF EXISTS ojak_queue RENAME TO feder_queue;
ALTER INDEX IF EXISTS ojak_queue_due RENAME TO feder_queue_due;
ALTER INDEX IF EXISTS ojak_queue_pkey RENAME TO feder_queue_pkey;
ALTER SEQUENCE IF EXISTS ojak_queue_id_seq RENAME TO feder_queue_id_seq;
ALTER TABLE IF EXISTS ojak_kv RENAME TO feder_kv;
ALTER INDEX IF EXISTS ojak_kv_expires RENAME TO feder_kv_expires;
ALTER INDEX IF EXISTS ojak_kv_pkey RENAME TO feder_kv_pkey;
