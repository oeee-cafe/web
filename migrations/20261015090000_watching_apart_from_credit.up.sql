-- What somebody is doing in a session now, apart from what they have done.
--
-- `is_spectator` says somebody has only ever watched: it is credit, and it
-- decides who is on the saved post, who earns the achievement, and who is
-- let back into a full room (anybody who has drawn is). `watching` is the
-- role they are in right now, and decides whether they hold a seat: a
-- drawer who sits back to watch gives theirs up, and keeps their credit.
--
-- A seat is the right to draw right now. A room has max_participants of
-- them; a person holds one while they are in the room drawing, however many
-- tabs they have it open in, and idle or not.
ALTER TABLE collaborative_sessions_participants
  ADD COLUMN watching BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE collaborative_sessions_participants SET watching = is_spectator;
