/**
 * The search box a collaborative room's owner picks where its drawing is
 * posted with, in the room's save dialog and on the drafts page: a field,
 * and under it -- in the page, not a drop-down over it -- the owner's own
 * posts and the communities that match. Untyped, those are the communities
 * the owner belongs to or has drawn in; typed, the public communities the
 * server finds (`GET /collaborate/{id}/destinations?q=`).
 *
 * Plain DOM, so the drafts page, which has no React, uses the same one. Each
 * host passes its own class names and words.
 */
import { fetchSaveDestinations, type Place } from "./saveDestinations";

export interface CommunitySearchWords {
  /** The field's placeholder and accessible name. */
  search: string;
  /** The choice that posts to the owner's own posts. */
  ownPosts: string;
  /** Beside a community the owner belongs to. */
  member: string;
  /** Beside one they have drawn in. */
  drawn: string;
  /** Under the own-posts choice when a search found nothing. */
  noMatches: string;
}

export interface CommunitySearchClasses {
  root: string;
  input: string;
  list: string;
  /** Each choice. It carries aria-selected and, under the keyboard, data-active. */
  option: string;
  /** The checkbox at the start of each choice, ticked on the chosen one. */
  check: string;
  /** The choice's name, which gives way first when the row is narrow. */
  name: string;
  /** The note beside a community's name, and the no-matches line. */
  meta: string;
}

export interface CommunitySearchOptions {
  sessionId: string;
  /** What to list before anything is typed: the owner's suggestions. */
  suggestions: Place[];
  /** The chosen community's id, or "" for the owner's own posts. */
  value: string;
  onChange: (community: string) => void;
  words: CommunitySearchWords;
  classes: CommunitySearchClasses;
  /** For the field's id, so a label can point at it. */
  id: string;
}

/** How long typing has to pause before the server is asked. */
const SEARCH_DELAY_MS = 200;

/** Builds the box into `container`. Returns what takes it down again. */
export function mountCommunitySearch(container: HTMLElement, options: CommunitySearchOptions): () => void {
  const { words, classes } = options;
  const listId = `${options.id}-list`;

  const root = document.createElement("div");
  root.className = classes.root;
  const input = document.createElement("input");
  input.type = "text";
  input.id = options.id;
  input.className = classes.input;
  input.placeholder = words.search;
  input.autocomplete = "off";
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-expanded", "true");
  input.setAttribute("aria-controls", listId);
  input.setAttribute("aria-autocomplete", "list");
  input.setAttribute("aria-label", words.search);
  const list = document.createElement("div");
  list.id = listId;
  list.className = classes.list;
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", words.search);
  root.append(input, list);
  container.append(root);

  let value = options.value;
  // The community chosen, kept so it stays listed while a search that does
  // not find it is showing.
  let chosen: Place | null = options.suggestions.find((place) => place.id === value) ?? null;
  let results = options.suggestions;
  let searched = "";
  let active = -1;
  let shown: Array<Place | null> = [];
  let asked = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function render(): void {
    shown = [null];
    if (chosen && !results.some((place) => place.id === chosen!.id)) shown.push(chosen);
    shown.push(...results);
    list.textContent = "";
    shown.forEach((place, index) => {
      const option = document.createElement("div");
      option.id = `${options.id}-option-${index}`;
      option.className = classes.option;
      option.setAttribute("role", "option");
      option.setAttribute("aria-selected", String((place?.id ?? "") === value));
      if (index === active) option.setAttribute("data-active", "");
      // Which one is chosen, said the way a form says it: a ticked box. A
      // highlighted row read as the keyboard's place as much as the choice.
      // The option is what is clicked and what a screen reader hears, so the
      // box is only drawn.
      const check = document.createElement("input");
      check.type = "checkbox";
      check.className = classes.check;
      check.checked = (place?.id ?? "") === value;
      check.tabIndex = -1;
      check.setAttribute("aria-hidden", "true");
      const name = document.createElement("span");
      name.className = classes.name;
      name.textContent = place ? place.name : words.ownPosts;
      option.append(check, name);
      const note = place?.member ? words.member : place?.drawn ? words.drawn : "";
      if (note) {
        const meta = document.createElement("span");
        meta.className = classes.meta;
        meta.textContent = note;
        option.append(meta);
      }
      // mousedown, not click: the field keeps its focus, and with it the
      // keyboard's place in the list.
      option.addEventListener("mousedown", (event) => {
        event.preventDefault();
        // The keyboard's outline is for the keyboard; a click's choice is
        // the ticked box alone.
        active = -1;
        choose(index);
      });
      list.append(option);
    });
    if (searched && !results.length) {
      const empty = document.createElement("div");
      empty.className = classes.meta;
      empty.textContent = words.noMatches;
      list.append(empty);
    }
    if (active >= 0) input.setAttribute("aria-activedescendant", `${options.id}-option-${active}`);
    else input.removeAttribute("aria-activedescendant");
  }

  function choose(index: number): void {
    const place = shown[index];
    if (place === undefined) return;
    value = place?.id ?? "";
    chosen = place;
    options.onChange(value);
    render();
  }

  function search(query: string): void {
    const ticket = ++asked;
    fetchSaveDestinations(options.sessionId, query)
      .then((destinations) => {
        // An answer to something since typed over is not this one's.
        if (ticket !== asked) return;
        searched = query.trim();
        results = searched ? destinations.choices : options.suggestions;
        active = -1;
        render();
      })
      .catch(console.error);
  }

  input.addEventListener("input", () => {
    if (timer) clearTimeout(timer);
    const query = input.value;
    if (!query.trim()) {
      // Back to the suggestions at once: there is nothing to ask.
      asked++;
      searched = "";
      results = options.suggestions;
      active = -1;
      render();
      return;
    }
    timer = setTimeout(() => search(query), SEARCH_DELAY_MS);
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      active = (active + step + shown.length) % shown.length;
      render();
      list.children[active]?.scrollIntoView?.({ block: "nearest" });
    } else if (event.key === "Enter") {
      // Never the dialog's own Enter: a choice is not a confirmation.
      event.preventDefault();
      if (active >= 0) choose(active);
    }
  });

  render();
  return () => {
    if (timer) clearTimeout(timer);
    asked++;
    root.remove();
  };
}
