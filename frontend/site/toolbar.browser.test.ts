import { afterEach, beforeAll, beforeEach, expect, it } from "vitest";
import { userEvent } from "vitest/browser";
import { COMMAND_NAMES, installToolbar, showToolbar } from "./toolbarControls";

type Page = Window &
  typeof globalThis & {
    oeeeApp?: { command?: (name: string) => boolean };
    oeeeJump?: { open(): void };
    oeeeLoadingBar?: { leave(then: () => void): void };
    oeeeMarkCurrent?: (here: string) => void;
    oeeeCommand?: (name: string) => boolean;
    oeeeGo?: (path: string) => void;
  };

const page = window as Page;

/** Every page the toolbar was asked to leave for, in order. */
let left: number;
let jumped: number;

/** The bar, as toolbar.jinja draws it signed out, over a page of content. */
const BAR = `
  <header>
    <nav class="nav-bar">
      <div id="menubar">
        <a class="toolbar-name" href="/">oeee</a>
        <div class="toolbar-links">
          <div class="toolbar-sections ds-segmented">
            <a href="/">Home</a><a href="/communities">Community</a><a href="/tags">Tags</a>
          </div>
          <a class="toolbar-live" href="/collaborate">Live</a>
        </div>
        <div class="toolbar-end">
          <a class="toolbar-square toolbar-button toolbar-search" href="/search">Search</a>
          <a class="toolbar-square toolbar-button toolbar-sign-in" href="/login">Sign in</a>
          <details class="toolbar-menu" id="gear">
            <summary>Gear</summary>
            <div class="toolbar-menu-items">
              <button type="button" data-theme-choice="light" aria-pressed="false">Light</button>
              <button type="button" data-theme-choice="dark" aria-pressed="false">Dark</button>
              <button type="button" data-theme-choice="system" aria-pressed="false">System</button>
            </div>
          </details>
        </div>
      </div>
      <div class="ds-shortcuts" id="ds-shortcuts" hidden>
        <button type="button" data-shortcuts-close>Close</button>
      </div>
    </nav>
  </header>
  <main class="ds-content"><p id="here">The page</p><input id="field" /></main>`;

beforeAll(() => {
  // The bridge is there before the toolbar, as theme_head.jinja puts it.
  page.oeeeApp = {};
  installToolbar();
});

beforeEach(() => {
  left = 0;
  jumped = 0;
  page.oeeeLoadingBar = {
    leave() {
      // Recorded, never followed: the test stays on its page.
      left++;
    },
  };
  page.oeeeJump = {
    open() {
      jumped++;
    },
  };
  document.body.className = "ds-page";
  document.body.innerHTML = BAR;
  showToolbar();
});

afterEach(() => {
  document.body.innerHTML = "";
  document.body.className = "";
  document.documentElement.removeAttribute("data-theme");
  try {
    localStorage.removeItem("neo-cucumber-theme");
  } catch {
    // Nothing kept.
  }
  delete page.oeeeLoadingBar;
  delete page.oeeeJump;
});

function skeleton(): HTMLElement | null {
  return document.querySelector("main.ds-content .ds-skeleton");
}

it("hands the apps its commands, and the page its helpers", () => {
  expect(typeof page.oeeeApp!.command).toBe("function");
  expect(page.oeeeCommand).toBe(page.oeeeApp!.command);
  expect(typeof page.oeeeGo).toBe("function");
  expect(page.oeeeApp!.command!("no-such-command")).toBe(false);
  expect(page.oeeeApp!.command!("toString")).toBe(false);
  expect(COMMAND_NAMES).toContain("new-drawing");
});

it("marks the section a path is in, with every feed under Home", () => {
  const current = () =>
    [...document.querySelectorAll("#menubar a[aria-current=page]")].map((a) => a.getAttribute("href"));
  page.oeeeMarkCurrent!("/following/comments");
  expect(current()).toEqual(["/", "/"]);
  page.oeeeMarkCurrent!("/communities/@drawers");
  expect(current()).toEqual(["/communities"]);
  page.oeeeMarkCurrent!("/communitiesque");
  expect(current()).toEqual([]);
});

it("sends signing in back to the page it was pressed on", () => {
  const signIn = document.querySelector(".toolbar-sign-in")!;
  expect(signIn.getAttribute("href")).toBe("/login?next=" + encodeURIComponent(location.pathname + location.search));
});

it("goes to a section on G and its letter, drawing the section at once", async () => {
  await userEvent.keyboard("gc");
  expect(left).toBe(1);
  expect(skeleton()!.querySelector(".community-grid")).not.toBeNull();
  expect(document.getElementById("here")!.style.visibility).toBe("hidden");
  expect(document.querySelector('#menubar a[href="/communities"]')!.getAttribute("aria-current")).toBe("page");

  // Going back to a page kept in htmx's history cache takes the skeleton down
  // before the page is kept.
  document.dispatchEvent(new Event("htmx:history:cache:before:save"));
  expect(skeleton()).toBeNull();
  expect(document.getElementById("here")!.style.visibility).toBe("");
});

it("leaves the keys alone while typing", async () => {
  document.getElementById("field")!.focus();
  await userEvent.keyboard("gc?/");
  expect(left).toBe(0);
  expect(jumped).toBe(0);
  expect(document.getElementById("ds-shortcuts")!.hidden).toBe(true);
});

it("lists the shortcuts on ?, and puts them away on Escape or Close", async () => {
  const panel = document.getElementById("ds-shortcuts")!;
  await userEvent.keyboard("?");
  expect(panel.hidden).toBe(false);
  expect(document.activeElement).toBe(panel.querySelector("[data-shortcuts-close]"));
  await userEvent.keyboard("{Escape}");
  expect(panel.hidden).toBe(true);
  await userEvent.keyboard("?");
  await userEvent.click(panel.querySelector("[data-shortcuts-close]")!);
  expect(panel.hidden).toBe(true);
});

it("opens the quick switcher from / and from the bar's search button", async () => {
  await userEvent.keyboard("/");
  expect(jumped).toBe(1);
  const search = document.querySelector<HTMLAnchorElement>(".toolbar-search")!;
  const click = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
  search.dispatchEvent(click);
  expect(jumped).toBe(2);
  expect(click.defaultPrevented).toBe(true);
  // Not a trip to /search, so no skeleton of one.
  expect(skeleton()).toBeNull();
});

it("closes a menu on a click elsewhere and on Escape", async () => {
  const gear = document.getElementById("gear") as HTMLDetailsElement;
  gear.open = true;
  await userEvent.click(document.getElementById("here")!);
  expect(gear.open).toBe(false);
  gear.open = true;
  await userEvent.keyboard("{Escape}");
  expect(gear.open).toBe(false);
});

it("switches the theme and shows which is chosen", async () => {
  const pressed = () =>
    [...document.querySelectorAll("[data-theme-choice][aria-pressed=true]")].map((b) =>
      b.getAttribute("data-theme-choice"),
    );
  expect(pressed()).toEqual(["system"]);
  (document.getElementById("gear") as HTMLDetailsElement).open = true;
  await userEvent.click(document.querySelector('[data-theme-choice="dark"]')!);
  expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  expect(pressed()).toEqual(["dark"]);
  page.oeeeCommand!("theme-system");
  expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
  expect(pressed()).toEqual(["system"]);
});
