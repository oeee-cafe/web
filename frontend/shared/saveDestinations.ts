/**
 * Where the owner of a collaborative room may post its drawing: asked by the
 * room's Save to Gallery (frontend/collaborate) and by the drafts page, which
 * posts a room a guest owner kept on the device.
 */

/** A community a drawing may be posted to. */
export interface Place {
  id: string;
  name: string;
  slug: string;
  /** The owner belongs to it. Only on a choice, not a room's own community. */
  member?: boolean;
  /** The owner has posted a drawing there. */
  drawn?: boolean;
}

/**
 * Where the owner's save may post the drawing (`GET
 * /collaborate/{id}/destinations`). A room opened on a community's page goes
 * there, `community`; one opened from the lobby offers the owner's own posts
 * or a public community, `choices`.
 */
export interface SaveDestinations {
  community: Place | null;
  choices: Place[];
}

/**
 * With no `query`, the owner's suggestions: the public communities they
 * belong to or have drawn in. With one, any public community whose name or
 * handle has it in, theirs first.
 */
export async function fetchSaveDestinations(sessionId: string, query = ""): Promise<SaveDestinations> {
  const search = query.trim() ? `?q=${encodeURIComponent(query.trim())}` : "";
  const response = await fetch(`/collaborate/${encodeURIComponent(sessionId)}/destinations${search}`, {
    credentials: "include",
  });
  if (!response.ok) throw new Error(`destinations ${response.status}`);
  return (await response.json()) as SaveDestinations;
}

/** The save's address, with the community chosen when there was a choice. */
export function saveUrl(sessionId: string, community: string): string {
  const base = `/collaborate/${encodeURIComponent(sessionId)}`;
  return community ? `${base}?community=${encodeURIComponent(community)}` : base;
}
