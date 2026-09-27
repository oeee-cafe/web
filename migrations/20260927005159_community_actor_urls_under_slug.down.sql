-- Back to the form a rename wrote, which redirects to the same page.
UPDATE actors
SET url = substring(actors.iri FROM '^https?://[^/]+') || '/communities/@' || communities.slug
FROM communities
WHERE actors.community_id = communities.id
AND actors.iri ~ '^https?://[^/]+/ap/communities/';
