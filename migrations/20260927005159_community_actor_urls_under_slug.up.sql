-- A community's Group actor gives its page as its url, and that page is
-- /@{slug}. Actors were written with /communities/{id} when created and
-- /communities/@{slug} when renamed, both of which only redirect there. The
-- scheme and host are the iri's, which is this server's for every community.
UPDATE actors
SET url = substring(actors.iri FROM '^https?://[^/]+') || '/@' || communities.slug,
    updated_at = now()
FROM communities
WHERE actors.community_id = communities.id
AND actors.iri ~ '^https?://[^/]+/ap/communities/';
