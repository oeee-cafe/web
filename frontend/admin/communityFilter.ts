/**
 * The staff posts list's community filter (templates/admin/posts.jinja):
 * the shared community search in place of what used to be a `<select>` of
 * every community there is. The form still sends the community's handle in
 * `community`, so a filtered list's address reads the same as before, and
 * without this script the field is a text box for the handle.
 */
import { mountCommunitySearch } from "../shared/communitySearch";
import type { Place } from "../shared/saveDestinations";

async function fetchChoices(query: string): Promise<Place[]> {
  const search = query.trim() ? `?q=${encodeURIComponent(query.trim())}` : "";
  const response = await fetch(`/admin/community-choices${search}`, { credentials: "include" });
  if (!response.ok) throw new Error(`community choices ${response.status}`);
  return ((await response.json()) as { choices: Place[] }).choices;
}

function mount(host: HTMLElement): void {
  const field = host.querySelector<HTMLInputElement>("input[name=community]");
  if (!field) return;
  const chosen: Place | undefined = host.dataset.id
    ? { id: host.dataset.id, name: host.dataset.name ?? "", slug: host.dataset.slug ?? "", note: host.dataset.note }
    : undefined;
  fetchChoices("")
    .then((suggestions) => {
      field.type = "hidden";
      mountCommunitySearch(host, {
        fetchChoices,
        suggestions,
        value: chosen?.id ?? "",
        chosen,
        onChange: (_community, place) => {
          field.value = place ? place.slug : "";
        },
        id: "admin-community-filter",
        words: {
          search: "Search communities",
          none: "All communities",
          member: "",
          drawn: "",
          noMatches: "No matching communities",
        },
        classes: {
          root: "community-search",
          input: "ds-input community-search-input",
          list: "community-search-list",
          option: "community-search-option",
          check: "community-search-check",
          name: "community-search-name",
          meta: "community-search-meta",
        },
      });
    })
    .catch(console.error);
}

document.querySelectorAll<HTMLElement>("[data-admin-community-filter]").forEach(mount);
