-- A room that nobody drew in for half an hour, with a drawing nobody saved,
-- is paused rather than ended: its live state is moved out of Redis into the
-- private bucket (`collaborate::pause`), and the first person to come back
-- puts it back and carries on. A paused room is still open -- `ended_at` is
-- NULL -- but holds nothing in Redis and is not listed in the public lobby.
-- After `pause::PAUSED_RETENTION` it ends for good.
ALTER TABLE collaborative_sessions ADD COLUMN paused_at TIMESTAMPTZ;
CREATE INDEX idx_collab_sessions_paused ON collaborative_sessions(paused_at)
  WHERE paused_at IS NOT NULL;
