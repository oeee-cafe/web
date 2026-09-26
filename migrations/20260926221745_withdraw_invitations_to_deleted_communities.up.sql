-- Deleting a community now withdraws its pending invitations
-- (soft_delete_community). The ones left behind by deletions before that
-- still count on their invitees' badges, for a community there is nothing
-- left of to join; they go the same way.
DELETE FROM community_invitations i
USING communities c
WHERE c.id = i.community_id
  AND c.deleted_at IS NOT NULL
  AND i.status = 'pending';
