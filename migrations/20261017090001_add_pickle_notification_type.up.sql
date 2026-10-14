-- Somebody gave your drawing a pickle (20261017090000_add_pickles). Its own
-- migration, because a value added to an enum cannot be used in the
-- transaction that adds it, and the next one's constraint uses it.
ALTER TYPE notification_type ADD VALUE 'pickle';
