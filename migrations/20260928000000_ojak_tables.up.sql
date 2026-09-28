-- feder is now ojak, and ojak-postgres's default tables are ojak_queue and
-- ojak_kv. What the old tables hold moves across, so queued deliveries, inbox
-- jobs and what the store has kept are not lost.
--
-- The server creates both tables when it starts, before it runs migrations,
-- so by now the new ones may already exist, empty or nearly so. Then the old
-- rows are copied in and the old table dropped; otherwise the old table is
-- renamed.
DO $$
BEGIN
    IF to_regclass('feder_queue') IS NOT NULL THEN
        IF to_regclass('ojak_queue') IS NOT NULL THEN
            INSERT INTO ojak_queue
                (queue, payload, attempts, run_at, last_error, failed_at, created_at)
            SELECT queue, payload, attempts, run_at, last_error, failed_at, created_at
            FROM feder_queue;
            DROP TABLE feder_queue;
        ELSE
            ALTER TABLE feder_queue RENAME TO ojak_queue;
            ALTER INDEX IF EXISTS feder_queue_due RENAME TO ojak_queue_due;
            ALTER SEQUENCE IF EXISTS feder_queue_id_seq RENAME TO ojak_queue_id_seq;
            ALTER TABLE ojak_queue RENAME CONSTRAINT feder_queue_pkey TO ojak_queue_pkey;
        END IF;
    END IF;

    IF to_regclass('feder_kv') IS NOT NULL THEN
        IF to_regclass('ojak_kv') IS NOT NULL THEN
            INSERT INTO ojak_kv (key, value, expires_at)
            SELECT key, value, expires_at FROM feder_kv
            ON CONFLICT (key) DO NOTHING;
            DROP TABLE feder_kv;
        ELSE
            ALTER TABLE feder_kv RENAME TO ojak_kv;
            ALTER INDEX IF EXISTS feder_kv_expires RENAME TO ojak_kv_expires;
            ALTER TABLE ojak_kv RENAME CONSTRAINT feder_kv_pkey TO ojak_kv_pkey;
        END IF;
    END IF;
END
$$;
