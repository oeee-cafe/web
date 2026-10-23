// New drawing, from the toolbar's Draw button: the canvas size, asked for
// first (draw_dialog.jinja). The button stays a form of its own at 300 × 300,
// which is what a press does without script. With it, the press opens the
// dialog instead (`oeeeDrawDialog`), as do the `new-drawing` command and its
// key.
//
// It also asks where the drawing is for: a personal post, or a community
// found by the shared search box (`GET /draw/communities`). The box is built
// the first time the dialog opens, so a page nobody draws from asks the
// server nothing; without script the hidden field stays empty, which is a
// personal post.
//
// A boosted swap brings a new dialog with the body. Its own listeners are
// added to it each time; the document's, once.
import { mountCommunitySearch, type CommunitySearchWords } from "../shared/communitySearch";
import type { Place } from "../shared/saveDestinations";

type DrawDialogWindow = Window & typeof globalThis & { oeeeDrawDialog?: { open(): boolean; close(): void } };

const page = window as DrawDialogWindow;

let opener: Element | null = null;

/** The search's words, from the dialog's line (draw_dialog.jinja). */
let searchWords: Partial<CommunitySearchWords> = {};

async function fetchChoices(query: string): Promise<Place[]> {
  const search = query.trim() ? `?q=${encodeURIComponent(query.trim())}` : "";
  const response = await fetch(`/draw/communities${search}`, { credentials: "include" });
  if (!response.ok) throw new Error(`draw communities ${response.status}`);
  return ((await response.json()) as { choices: Place[] }).choices;
}

/**
 * Builds the community search into `modal`, once: at once, with Personal
 * post alone, so the dialog opens at its full height rather than growing
 * under the pointer when the server answers. The suggestions are built in
 * when they come, into a list of fixed height (ds.css), unless a search has
 * been typed meanwhile, which is then what the list shows anyway.
 */
function offerCommunities(modal: HTMLElement): void {
  const host = modal.querySelector<HTMLElement>("[data-draw-dialog-search]");
  const field = modal.querySelector<HTMLInputElement>("[data-draw-dialog-community]");
  if (!host || !field || host.dataset.mounted) return;
  host.dataset.mounted = "1";
  const mount = (suggestions: Place[]) =>
    mountCommunitySearch(host, {
      fetchChoices,
      suggestions,
      value: field.value,
      onChange: (community) => {
        field.value = community;
      },
      id: "draw-dialog-community-search",
      words: {
        search: searchWords.search || "Search communities",
        none: searchWords.none || "Personal post",
        member: searchWords.member || "member",
        drawn: searchWords.drawn || "drawn here",
        noMatches: searchWords.noMatches || "No matching communities",
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
  let unmount = mount([]);
  host.hidden = false;
  fetchChoices("")
    .then((suggestions) => {
      const input = host.querySelector<HTMLInputElement>("input");
      // A boosted swap may have taken this dialog away meanwhile, or the
      // reader has typed a search, whose answer is the list now.
      if (!document.body.contains(host) || !input || input.value.trim() || !suggestions.length) return;
      const focused = document.activeElement === input;
      unmount();
      unmount = mount(suggestions);
      if (focused) host.querySelector<HTMLInputElement>("input")?.focus();
    })
    .catch(console.error);
}

function dialog(): HTMLElement | null {
  return document.getElementById("draw-dialog");
}

function open(): boolean {
  const modal = dialog();
  if (!modal) return false;
  opener = document.activeElement;
  modal.style.display = "flex";
  offerCommunities(modal);
  // The heading, not the first field: on iOS a focused <select> opens its
  // picker, so the width wheel would come up with the dialog.
  document.getElementById("draw-dialog-title")!.focus();
  return true;
}

function close(): void {
  const modal = dialog();
  if (!modal || modal.style.display === "none") return;
  modal.style.display = "none";
  const back = opener as HTMLElement | null;
  if (back && document.body.contains(back)) back.focus();
  opener = null;
}

/** Wires the dialog the page has now, and the first time, the Draw button. */
export function attachDrawDialog(words?: Partial<CommunitySearchWords>): void {
  if (words) searchWords = words;
  const modal = dialog();
  if (modal) {
    modal.querySelector("[data-draw-dialog-cancel]")!.addEventListener("click", close);
    modal.addEventListener("mousedown", (event) => {
      // Pressing the dimmed page is a Cancel.
      if (event.target === modal) close();
    });
    modal.addEventListener("keydown", (event) => {
      if (event.key === "Escape" || event.key === "Esc") {
        event.preventDefault();
        close();
      }
    });
  }

  if (page.oeeeDrawDialog) return;
  page.oeeeDrawDialog = { open, close };
  document.addEventListener("click", (event) => {
    if (event.defaultPrevented || event.button !== 0) return;
    const target = event.target as Element | null;
    const button = target && target.closest ? target.closest("#nav-draw-button") : null;
    if (button && open()) event.preventDefault();
  });
}
