-- Somebody watching a collaborative session rather than drawing in it. They
-- have a participant row -- so the owner can send them out, and a ban keeps
-- them out -- but no seat: the room's capacity counts drawers only.
--
-- `is_spectator` is true of somebody who has only ever watched. Drawing makes
-- it false for good, since a drawer is credited on the post and earns the
-- achievement whether or not they later sit back and watch.
ALTER TABLE collaborative_sessions_participants
  ADD COLUMN is_spectator BOOLEAN NOT NULL DEFAULT FALSE;

-- The owner's: whether the session can be watched. On unless they turn it off.
ALTER TABLE collaborative_sessions
  ADD COLUMN allow_spectators BOOLEAN NOT NULL DEFAULT TRUE;
