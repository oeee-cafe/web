-- The version of the lobby preview a paused room took into the private bucket
-- with it (`paused/{id}.preview`), or NULL when it had none. Set and cleared
-- with paused_at.
ALTER TABLE collaborative_sessions ADD COLUMN paused_preview_version BIGINT;
