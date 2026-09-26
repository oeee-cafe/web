-- A remote handle is printed as @name@host beside our own @login_name, so
-- its name must not be able to hide or move the host: padding that pushes
-- it past a line's ellipsis, a bidirectional override, an invisible
-- character. The same range as is_plain_remote_username (models/actor.rs),
-- written out rather than as a POSIX class (see CLAUDE.md). Local login
-- names and community slugs are narrower still, so every actor of ours
-- already passes.
--
-- NOT VALID: new and updated rows are held to it, and an actor stored
-- before it keeps its row -- its comments hang off it -- and is drawn with
-- its host kept in view instead (person_macro.jinja).
ALTER TABLE actors
    ADD CONSTRAINT actors_username_is_plain
    CHECK (username ~ '^[A-Za-z0-9_.~-]{1,128}$') NOT VALID;
