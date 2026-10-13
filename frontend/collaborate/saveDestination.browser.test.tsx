import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import { DefaultI18n } from "neo-cucumber";
import { SaveConfirmationModal } from "./components/modals/SaveConfirmationModal";
import { saveUrl, type SaveDestinations } from "../shared/saveDestinations";
import { setupI18n } from "./i18n";

/**
 * Where Save to Gallery posts. A room opened from the lobby is in no
 * community, and its owner picks one in a search box: their own communities
 * before anything is typed, any public one the server finds after. A room
 * opened on a community's page goes there, and the dialog only says so.
 */

const ROOM = "9c881320-2b43-4afa-b2bb-7128c8a3e985";
const MINE = { id: "00000000-0000-0000-0000-000000000005", name: "Open Studio", slug: "open", member: true, drawn: false };
const FOUND = { id: "00000000-0000-0000-0000-000000000006", name: "Quiet Corner", slug: "quiet", member: false, drawn: false };

let host: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(destinations: SaveDestinations | null, toDevice = false) {
  setupI18n("en");
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  let chosen = "";
  const draw = async () => {
    await act(async () => {
      root!.render(
        <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
          <SaveConfirmationModal
            isOpen
            toDevice={toDevice}
            destination={{
              sessionId: ROOM,
              destinations,
              value: chosen,
              onChange: (community) => {
                chosen = community;
                void draw();
              },
            }}
            onConfirm={() => {}}
            onCancel={() => {}}
          />
        </I18nProvider>,
      );
    });
  };
  await draw();
  return () => chosen;
}

const options = () =>
  [...document.querySelectorAll<HTMLElement>('[role="option"]')].map((option) => option.textContent);
const selected = () => document.querySelector('[role="option"][aria-selected="true"]')?.textContent;

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  host = null;
  root = null;
  vi.unstubAllGlobals();
});

describe("where the owner's save posts", () => {
  it("suggests the owner's communities, then searches public ones as they type", async () => {
    const asked: string[] = [];
    vi.stubGlobal("fetch", async (url: string) => {
      asked.push(url);
      return new Response(JSON.stringify({ community: null, choices: [FOUND] }));
    });
    const chosen = await render({ community: null, choices: [MINE] });
    const input = document.querySelector<HTMLInputElement>("#save-destination")!;
    expect(input.getAttribute("role")).toBe("combobox");
    expect(options()).toEqual(["My own posts", "Open Studiomember"]);
    expect(selected()).toBe("My own posts");

    // A click on a suggestion chooses it.
    const studio = document.querySelectorAll<HTMLElement>('[role="option"]')[1];
    await act(async () => {
      studio.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
    });
    expect(chosen()).toBe(MINE.id);
    expect(saveUrl(ROOM, chosen())).toBe(`/collaborate/${ROOM}?community=${MINE.id}`);

    // Typing asks the server, once the typing pauses.
    await act(async () => {
      input.value = "qui";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await vi.waitFor(() => expect(options()).toContain("Quiet Corner"));
    expect(asked).toEqual([`/collaborate/${ROOM}/destinations?q=qui`]);
    // What was chosen stays listed, and chosen, beside what was found.
    expect(options()).toEqual(["My own posts", "Open Studiomember", "Quiet Corner"]);
    expect(selected()).toBe("Open Studiomember");

    // The keyboard chooses too, and Enter does not confirm the dialog.
    const down = () =>
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }));
    await act(async () => {
      down();
      down();
      down();
    });
    const enter = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    await act(async () => {
      input.dispatchEvent(enter);
    });
    expect(enter.defaultPrevented).toBe(true);
    expect(chosen()).toBe(FOUND.id);

    // Cleared, it is back to the suggestions without asking.
    await act(async () => {
      input.value = "";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(asked).toHaveLength(1);
    expect(options()).toEqual(["My own posts", "Quiet Corner", "Open Studiomember"]);
  });

  it("says when a search finds nothing", async () => {
    vi.stubGlobal("fetch", async () => new Response(JSON.stringify({ community: null, choices: [] })));
    await render({ community: null, choices: [] });
    const input = document.querySelector<HTMLInputElement>("#save-destination")!;
    await act(async () => {
      input.value = "nowhere";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await vi.waitFor(() => expect(document.body.textContent).toContain("No matching communities"));
    expect(options()).toEqual(["My own posts"]);
  });

  it("names the community a room opened on its page goes to, with nothing to choose", async () => {
    await render({ community: MINE, choices: [] });
    expect(document.querySelector("#save-destination")).toBeNull();
    expect(document.body.textContent).toContain("Posts to Open Studio");
  });

  it("asks nothing of a guest owner, whose drawing stays on the device", async () => {
    await render({ community: null, choices: [MINE] }, true);
    expect(document.querySelector("#save-destination")).toBeNull();
  });
});
