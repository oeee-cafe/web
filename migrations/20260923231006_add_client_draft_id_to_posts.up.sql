-- The id a browser gave a drawing when it saved it on the device.
--
-- Every save is kept locally first and uploaded afterwards (by a signed-in
-- painter at once, by a guest from the drafts page once they have an
-- account), so the same drawing can reach /draw/finish twice: two tabs
-- claiming it together, or an upload that succeeded but whose tab closed
-- before it could forget its local copy. The second arrival is answered with
-- the post the first one made instead of making another.
--
-- Deleted posts are left out of the index: a drawing whose uploaded copy was
-- thrown away but which is still on the device can be sent again, and is
-- then a new post.
--
-- Nullable, and only ever added: the release this deploy replaces keeps
-- inserting posts without it while both colours are up.
ALTER TABLE posts ADD COLUMN client_draft_id UUID;

CREATE UNIQUE INDEX posts_author_id_client_draft_id_key
    ON posts (author_id, client_draft_id)
    WHERE client_draft_id IS NOT NULL AND deleted_at IS NULL;
