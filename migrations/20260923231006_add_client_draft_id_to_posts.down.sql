DROP INDEX IF EXISTS posts_author_id_client_draft_id_key;
ALTER TABLE posts DROP COLUMN IF EXISTS client_draft_id;
