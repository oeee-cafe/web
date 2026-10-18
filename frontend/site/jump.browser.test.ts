import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { userEvent } from "vitest/browser";
import { installJump } from "./jump";

type Page = Window &
  typeof globalThis & {
    oeeeJump?: { open(): void; close(): void };
    oeeeGo?: (path: string) => void;
    oeeeCommand?: (name: string) => boolean;
  };

const page = window as Page;

/** What /jump answers each q with: rows as jump_results.jinja draws them. */
const ROWS: Record<string, string[][]> = {
  "": [
    ["/communities/@drawers", "Drawers"],
    ["/@neo", "Neo"],
    ["/tags/cat", "#cat"],
  ],
  ne: [["/@neo", "Neo"]],
  cmd: [["/notifications", "Notifications", "notifications"]],
};

let asked: string[];
let went: string[];
let commanded: string[];
/** Answers held back, to be let go out of order. */
let held: (() => void)[];
let holding: boolean;

function answer(q: string): Response {
  const html = (ROWS[q] ?? [])
    .map(
      ([href, name, command]) =>
        `<li><a class="jump-item" href="${href}"${command ? ` data-command="${command}"` : ""}>${name}</a></li>`,
    )
    .join("");
  return new Response(html, { status: 200 });
}

beforeAll(() => {
  installJump();
});

beforeEach(() => {
  asked = [];
  went = [];
  commanded = [];
  held = [];
  holding = false;
  vi.stubGlobal("fetch", (url: string) => {
    const q = decodeURIComponent(url.split("?q=")[1] ?? "");
    asked.push(q);
    if (!holding) return Promise.resolve(answer(q));
    return new Promise<Response>((resolve) => held.push(() => resolve(answer(q))));
  });
  page.oeeeGo = (path) => went.push(path);
  page.oeeeCommand = (name) => {
    commanded.push(name);
    return true;
  };
  document.body.className = "ds-page";
  document.body.innerHTML = `
    <button id="before">Somewhere</button>
    <div class="jump" id="jump" hidden>
      <div class="jump-panel" role="dialog">
        <input id="jump-field" type="text" role="combobox" aria-controls="jump-list" />
        <span class="jump-key"><kbd id="jump-key-mod">Ctrl</kbd> <kbd>K</kbd></span>
        <ul id="jump-list" class="jump-list" role="listbox"></ul>
      </div>
    </div>`;
});

afterEach(() => {
  page.oeeeJump!.close();
  vi.unstubAllGlobals();
  delete page.oeeeGo;
  delete page.oeeeCommand;
  document.body.innerHTML = "";
  document.body.className = "";
});

function jump(): HTMLElement {
  return document.getElementById("jump")!;
}

function field(): HTMLInputElement {
  return document.getElementById("jump-field") as HTMLInputElement;
}

function chosen(): string | null {
  const row = document.querySelector('#jump-list .jump-item[aria-selected="true"]');
  return row ? row.getAttribute("href") : null;
}

async function openWithRows(): Promise<void> {
  await userEvent.keyboard("{Control>}k{/Control}");
  await expect.poll(chosen).toBe("/communities/@drawers");
}

it("opens on Ctrl+K with the field focused and the first row chosen, and closes on it again", async () => {
  const before = document.getElementById("before")!;
  before.focus();
  await openWithRows();
  expect(jump().hidden).toBe(false);
  expect(document.activeElement).toBe(field());
  expect(asked).toEqual([""]);
  expect(field().getAttribute("aria-activedescendant")).toBe("jump-row-0");

  await userEvent.keyboard("{Control>}k{/Control}");
  expect(jump().hidden).toBe(true);
  expect(document.activeElement).toBe(before);
});

it("is not opened from a page that is not the site's own", async () => {
  document.body.className = "";
  await userEvent.keyboard("{Control>}k{/Control}");
  expect(jump().hidden).toBe(true);
});

it("moves through the rows with the arrows and Tab, round from either end", async () => {
  await openWithRows();
  await userEvent.keyboard("{ArrowDown}");
  expect(chosen()).toBe("/@neo");
  await userEvent.keyboard("{ArrowDown}{ArrowDown}");
  expect(chosen()).toBe("/communities/@drawers");
  await userEvent.keyboard("{ArrowUp}");
  expect(chosen()).toBe("/tags/cat");
  await userEvent.keyboard("{Tab}");
  expect(chosen()).toBe("/communities/@drawers");
  await userEvent.keyboard("{Shift>}{Tab}{/Shift}");
  expect(chosen()).toBe("/tags/cat");
  // The field keeps the focus throughout.
  expect(document.activeElement).toBe(field());
});

it("goes to the chosen row on Enter", async () => {
  await openWithRows();
  await userEvent.keyboard("{ArrowDown}{Enter}");
  expect(went).toEqual(["/@neo"]);
  expect(jump().hidden).toBe(true);
});

it("runs a row's command instead of going, when it has one", async () => {
  await openWithRows();
  await userEvent.keyboard("cmd");
  await expect.poll(chosen).toBe("/notifications");
  await userEvent.keyboard("{Enter}");
  expect(commanded).toEqual(["notifications"]);
  expect(went).toEqual([]);
});

it("asks again as it is typed in, and closes on Escape", async () => {
  await openWithRows();
  await userEvent.keyboard("ne");
  await expect.poll(chosen).toBe("/@neo");
  expect(asked[asked.length - 1]).toBe("ne");
  await userEvent.keyboard("{Escape}");
  expect(jump().hidden).toBe(true);
});

it("keeps the newest answer when an older one arrives after it", async () => {
  await openWithRows();
  holding = true;
  page.oeeeJump!.close();
  page.oeeeJump!.open(); // asks for ""
  field().value = "ne";
  field().dispatchEvent(new Event("input", { bubbles: true }));
  await expect.poll(() => held.length).toBe(2);
  held[1](); // "ne" first
  await expect.poll(chosen).toBe("/@neo");
  held[0](); // then the stale ""
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(document.querySelectorAll("#jump-list .jump-item").length).toBe(1);
});

it("closes on a press outside the panel", async () => {
  await openWithRows();
  jump().dispatchEvent(new MouseEvent("click", { bubbles: true, button: 0 }));
  expect(jump().hidden).toBe(true);
});
