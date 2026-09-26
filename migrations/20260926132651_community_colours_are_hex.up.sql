-- A community's two colours go into style attributes on its page and on
-- its drawings' pages, and into the painter's settings. Nothing on the site
-- writes them -- they are set by hand -- so this is the one place that
-- holds them to a colour: # and six hex digits, written out rather than as
-- a POSIX class (see CLAUDE.md).
--
-- NOT VALID, so a row set by hand before this is not a reason for a deploy
-- to fail at boot; every new or changed row is held to it.
ALTER TABLE communities
    ADD CONSTRAINT communities_colours_are_hex
    CHECK (
        (foreground_color IS NULL OR foreground_color ~ '^#[0-9a-fA-F]{6}$')
        AND (background_color IS NULL OR background_color ~ '^#[0-9a-fA-F]{6}$')
    ) NOT VALID;
