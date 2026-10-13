import { afterEach, describe, expect, it } from "vitest";
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
 * community, and its owner picks one when saving; a room opened on a
 * community's page goes there, and the dialog only says so.
 */

const ROOM = "9c881320-2b43-4afa-b2bb-7128c8a3e985";
const OPEN = { id: "00000000-0000-0000-0000-000000000005", name: "Open Studio", slug: "open" };

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

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  host = null;
  root = null;
});

describe("where the owner's save posts", () => {
  it("offers a room in no community the owner's own posts or a public community", async () => {
    const chosen = await render({ community: null, choices: [OPEN] });
    const select = document.querySelector<HTMLSelectElement>("#save-destination");
    expect(select).not.toBeNull();
    expect([...select!.options].map((option) => option.textContent)).toEqual(["My own posts", "Open Studio"]);
    expect(select!.value).toBe("");

    await act(async () => {
      select!.value = OPEN.id;
      select!.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(chosen()).toBe(OPEN.id);
    expect(saveUrl(ROOM, chosen())).toBe(`/collaborate/${ROOM}?community=${OPEN.id}`);
    expect(saveUrl(ROOM, "")).toBe(`/collaborate/${ROOM}`);
  });

  it("names the community a room opened on its page goes to, with nothing to choose", async () => {
    await render({ community: OPEN, choices: [] });
    expect(document.querySelector("#save-destination")).toBeNull();
    expect(document.body.textContent).toContain("Posts to Open Studio");
  });

  it("asks nothing of a guest owner, whose drawing stays on the device", async () => {
    await render({ community: null, choices: [OPEN] }, true);
    expect(document.querySelector("#save-destination")).toBeNull();
  });
});
