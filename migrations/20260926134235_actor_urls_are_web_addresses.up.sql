-- A remote actor's url is printed as the href of their name, beside their
-- comments and reactions. It is whatever their server said, and before the
-- check in activitypub.rs nothing held it to being a web address: a
-- javascript: url would have run in a reader's page when they clicked the
-- name. An actor whose url is not http(s) is pointed at its iri, the id it
-- was fetched from, which is.
UPDATE actors
SET url = iri
WHERE url !~ '^https?://';

-- Written out rather than as a POSIX class (see CLAUDE.md). NOT VALID, in
-- case an actor's iri is no better than its url -- none should be, since an
-- iri is what the actor was fetched from -- so that one such row cannot stop
-- a deploy at boot; every new or changed row is held to it.
ALTER TABLE actors
    ADD CONSTRAINT actors_url_is_web
    CHECK (url ~ '^https?://') NOT VALID;
