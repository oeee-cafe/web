-- A paused room's drawing is in the bucket, which nothing before this reads:
-- it ends here. Its object is left for the bucket's own lifecycle.
UPDATE collaborative_sessions SET ended_at = NOW()
  WHERE paused_at IS NOT NULL AND ended_at IS NULL;
DROP INDEX idx_collab_sessions_paused;
ALTER TABLE collaborative_sessions DROP COLUMN paused_at;
