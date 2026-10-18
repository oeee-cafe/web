// What the toolbar (toolbar.jinja) does with script: which section is the
// page you are on, signing in coming back here, fitting the bar to one row
// or two, keeping the reader's place, the theme switch, the commands and
// keys the site and the apps share, closing the menus, and the skeleton of
// the next page laid down at the press.
//
// Two halves, as the inline script it was had them. showToolbar() is for
// each toolbar the page gets -- on a full load, and again after every
// boosted swap, which brings a new one with the body -- and the template
// calls it from an inline script that htmx re-runs in place. installToolbar()
// is for the document: the listeners on it and on the window, which outlive
// a swap, are added once.

import { FROM_ANYWHERE, skeletonFor } from "./skeletons";

interface OeeeApp {
  command?: (name: string) => boolean;
  wouldLoseWork?: () => boolean;
}

interface Htmx {
  ajax(verb: string, path: string, options: { source: Element; target: Element; swap: string }): unknown;
}

/** What the toolbar finds on the window, and what it puts there. */
type ToolbarWindow = Window &
  typeof globalThis & {
    htmx?: Htmx;
    oeeeApp?: OeeeApp;
    oeeeJump?: { open(): void };
    oeeeDrawDialog?: { open(): boolean };
    oeeeLoadingBar?: { leave(then: () => void): void };
    oeeeMarkCurrent?: (here: string) => void;
    oeeeFitToolbar?: () => void;
    oeeeCommand?: (name: string) => boolean;
    oeeeGo?: (path: string) => void;
    oeeeShowSkeleton?: (path?: string) => void;
  };

const page = window as ToolbarWindow;

// Also called with the path being gone to, the moment it is asked for, so the
// tab moves on the click rather than when the page arrives -- and with the
// page's own path again if it never does.
const HOME = /^\/((following|joined)(\/comments)?|comments)?$/;

function markCurrent(here: string): void {
  const links = document.querySelectorAll("#menubar a[href]");
  for (let i = 0; i < links.length; i++) {
    const path = links[i].getAttribute("href")!;
    // Home is every one of its feeds (feed_switch.jinja).
    if (path === "/" ? HOME.test(here) : here === path || here.indexOf(path + "/") === 0) {
      links[i].setAttribute("aria-current", "page");
    } else {
      links[i].removeAttribute("aria-current");
    }
  }
}

// One row, or two: try the name, the sections and the account on one line,
// and if the account or the sections wrapped below the name -- or would sit
// closer than 16px to the sections -- give the sections a row of their own
// (ds.css, .is-stacked). Measured rather than set at a width, because the
// width that fits depends on the language and, in the desktop app, on the
// window's own controls.
function fitToolbar(): void {
  const nav = document.querySelector(".nav-bar");
  const bar = document.getElementById("menubar");
  if (!nav || !bar) return;
  const name = bar.querySelector(".toolbar-name");
  const links = bar.querySelector(".toolbar-links");
  const end = bar.querySelector(".toolbar-end");
  if (!name || !links || !end) return;
  nav.classList.remove("is-stacked", "is-compact", "is-bare");
  const line = name.getBoundingClientRect();
  const sections = links.getBoundingClientRect();
  const account = end.getBoundingClientRect();
  const oneRow = sections.top < line.bottom && account.top < line.bottom && account.left - sections.right >= 16;
  if (oneRow) return;
  nav.classList.add("is-stacked");
  // Stacked, the first line still has to hold the name and the account. If
  // it does not, first Draw keeps only its pencil and the buttons close up;
  // then, if even that wraps, the name steps aside and the mark stays, still
  // the way home. Each step only where it is needed.
  const wrapped = () => end.getBoundingClientRect().top >= name.getBoundingClientRect().bottom;
  if (!wrapped()) return;
  nav.classList.add("is-compact");
  if (!wrapped()) return;
  nav.classList.add("is-bare");
}

// On a wide screen the content area scrolls, not the window, and browsers
// only restore the window's own scroll when you go back -- so where you were
// is kept here, per page, and put back on a full load that came from the
// history or a reload. Going back within the site is htmx's history cache
// (base.jinja), which keeps every scrolled element's place itself. And the
// content area takes the keyboard's focus when nothing else has it, so Page
// Down scrolls it without a click first. The toolbar comes before <main> in
// the page, so on a full load this waits for it; after a boosted swap it is
// already there.

// What scrolls: the content area on a wide screen, the window on a phone and
// in the handheld apps (ds.css, "The content area").
function scrollerOf(content: Element): Element {
  const overflow = window.getComputedStyle(content).overflowY;
  return overflow === "auto" || overflow === "scroll" ? content : document.scrollingElement || document.documentElement;
}

function scrollKey(): string {
  return "oeee-scroll:" + location.pathname + location.search;
}

// An infinite list's sentinel that pulls more of the page itself, not one at
// the foot of a list that scrolls on its own inside it (the comments beside a
// feed, comments_fragment.jinja).
function pageSentinel(content: Element): Element | null {
  const sentinels = content.querySelectorAll(".infinite-scroll-sentinel[hx-get]");
  outer: for (let i = sentinels.length - 1; i >= 0; i--) {
    for (let up = sentinels[i].parentElement; up && up !== content; up = up.parentElement) {
      const overflow = window.getComputedStyle(up).overflowY;
      if (overflow === "auto" || overflow === "scroll") continue outer;
    }
    return sentinels[i];
  }
  return null;
}

// A full load of an infinitely scrolling page has only its first batch, too
// short to scroll to where the reader was, and putting it back there clamps
// it to the foot of that batch. So the batches they had seen are fetched
// again, one after another as the sentinel would have, until the page is long
// enough -- and the place put back then. Given up the moment the reader
// scrolls for themselves.
function returnTo(content: Element, top: number): void {
  const scroller = scrollerOf(content);
  let stopped = false;
  let batches = 0;
  const events = ["wheel", "touchstart", "keydown", "mousedown"];
  function stop(): void {
    stopped = true;
    for (let e = 0; e < events.length; e++) window.removeEventListener(events[e], stop, true);
  }
  for (let e = 0; e < events.length; e++) window.addEventListener(events[e], stop, true);
  function step(): void {
    if (stopped) return;
    const sentinel = pageSentinel(content);
    if (scroller.scrollHeight - scroller.clientHeight >= top || !sentinel || !page.htmx || batches++ >= 50) {
      scroller.scrollTop = top;
      stop();
      return;
    }
    // The sentinel may already be on its way, revealed by the reader or by
    // htmx; then this only waits for it.
    if (!sentinel.classList.contains("htmx-request")) {
      page.htmx.ajax("GET", sentinel.getAttribute("hx-get")!, {
        source: sentinel,
        target: sentinel,
        swap: "outerHTML",
      });
    }
    let waited = 0;
    (function wait(): void {
      if (stopped) return;
      if (!sentinel.isConnected) return step();
      if ((waited += 50) > 10000) return stop();
      setTimeout(wait, 50);
    })();
  }
  // The browser's own restore would clamp the window to the foot of the
  // first batch, reveal the sentinel and send it for the batch asked for here
  // a second time. This page puts its place back itself.
  try {
    history.scrollRestoration = "manual";
  } catch {
    // An engine without it restores as it will.
  }
  step();
}

let settledOnce = false;
let remembersWindowScroll = false;

function settleContent(): void {
  const content = document.querySelector<HTMLElement>("main.ds-content");
  if (!content) return;
  const entry =
    window.performance && performance.getEntriesByType
      ? (performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined)
      : undefined;
  const returning = !settledOnce && entry && (entry.type === "back_forward" || entry.type === "reload");
  settledOnce = true;
  if (returning) {
    let top = 0;
    try {
      top = parseInt(sessionStorage.getItem(scrollKey())!, 10);
    } catch {
      // No storage, no place kept.
    }
    if (top > 0) returnTo(content, top);
  }
  let pending: ReturnType<typeof setTimeout> | undefined;
  function remember(): void {
    clearTimeout(pending);
    pending = setTimeout(() => {
      const scroller = document.querySelector("main.ds-content");
      if (!scroller) return;
      try {
        sessionStorage.setItem(scrollKey(), String(scrollerOf(scroller).scrollTop));
      } catch {
        // No storage, nothing kept.
      }
    }, 100);
  }
  content.addEventListener("scroll", remember);
  if (!remembersWindowScroll) {
    remembersWindowScroll = true;
    window.addEventListener("scroll", remember);
  }
  // Where the page scrolls, not the content area -- a phone's browser and the
  // handheld apps (ds.css) -- there is nothing for the keyboard to scroll, and
  // the content area is not focusable at all: a tap inside it would focus it,
  // and WebKit scrolls a focused element back into view when the page comes
  // back from the back/forward cache -- to its top, over where the reader
  // was. Safari on an iPhone has no data-form, and going back to a feed there
  // landed at its head.
  if (document.documentElement.getAttribute("data-form") === "handheld" || scrollerOf(content) !== content) {
    content.removeAttribute("tabindex");
  } else if (!document.activeElement || document.activeElement === document.body) {
    try {
      content.focus({ preventScroll: true });
    } catch {
      content.focus();
    }
  }
}

function showThemeChoice(): void {
  let chosen = "system";
  try {
    const stored = localStorage.getItem("neo-cucumber-theme");
    if (stored === "light" || stored === "dark") chosen = stored;
  } catch {
    // No storage: the system's.
  }
  const buttons = document.querySelectorAll("[data-theme-choice]");
  for (let k = 0; k < buttons.length; k++) {
    buttons[k].setAttribute("aria-pressed", String(buttons[k].getAttribute("data-theme-choice") === chosen));
  }
}

// Signed out the server does not know whether the page is in this language
// because it was chosen or because the browser asked for it, so it marks the
// language in use (toolbar.jinja, preference_choices); with no choice kept in
// the cookie, the mark moves to Auto.
function markGuestLanguage(): void {
  const select = document.querySelector<HTMLSelectElement>("#toolbar-language[data-guest]");
  if (select && !/(?:^|;\s*)lang=/.test(document.cookie)) select.value = "auto";
}

// Signing in from here comes back here. Templates are not handed the request
// path, so the link learns it from the page -- as it is when the link is
// followed, since a page can change its own address after it loads (a
// collaborative room adds ?watch=1). Without script it is plain /login. The
// sign-in pages are nowhere to come back to.
function comeBackAfterSigningIn(): void {
  const signIn = document.querySelector(".toolbar-sign-in");
  if (!signIn) return;
  const comeBack = () => {
    if (/^\/(login|signup|auth)(\/|$)/.test(location.pathname)) return;
    signIn.setAttribute("href", "/login?next=" + encodeURIComponent(location.pathname + location.search));
  };
  comeBack();
  signIn.addEventListener("click", comeBack);
}

// Drawings kept in this browser (frontend/shared/localDrafts.ts), added to
// what the server counted: the account's own and every guest's, since those
// are what the drafts page offers to post. The database is opened without
// being made -- a first visit aborts the upgrade that would create it, so the
// painter still gets to create it with its store.
function countLocalDrafts(): void {
  const square = document.querySelector<HTMLElement>(".nav-bar .toolbar-drafts");
  if (!square || !window.indexedDB) return;
  const nav = square.closest ? square.closest(".nav-bar") : null;
  const userId = (nav && nav.getAttribute("data-user-id")) || "";
  let opening: IDBOpenDBRequest;
  try {
    opening = indexedDB.open("oeee-local-drafts", 1);
  } catch {
    return;
  }
  opening.onupgradeneeded = () => {
    opening.transaction!.abort();
  };
  opening.onerror = (event) => {
    if (event.preventDefault) event.preventDefault();
  };
  opening.onsuccess = () => {
    const db = opening.result;
    let index: IDBIndex;
    try {
      index = db.transaction("drafts", "readonly").objectStore("drafts").index("owner");
    } catch {
      db.close();
      return;
    }
    const owners = userId ? ["", userId] : [""];
    let local = 0;
    let pending = owners.length;
    owners.forEach((owner) => {
      const counting = index.count(owner);
      counting.onsuccess = () => {
        local += counting.result;
        if (--pending === 0) {
          db.close();
          show(local);
        }
      };
    });
  };
  function show(local: number): void {
    if (!local) return;
    const total = (parseInt(square!.getAttribute("data-server-count")!, 10) || 0) + local;
    // The square: shown, and counted in its words.
    const label = square!.getAttribute("title")!.replace(/ \(\d+\)$/, "") + " (" + total + ")";
    square!.setAttribute("aria-label", label);
    square!.setAttribute("title", label);
    square!.hidden = false;
  }
}

/**
 * Everything one toolbar needs: run for the toolbar a page loads with, and
 * for each one a boosted swap brings in its place.
 */
export function showToolbar(): void {
  markGuestLanguage();
  markCurrent(location.pathname);
  comeBackAfterSigningIn();
  fitToolbar();
  showThemeChoice();
  if (document.querySelector("main.ds-content")) settleContent();
  else document.addEventListener("DOMContentLoaded", settleContent);
  countLocalDrafts();
}

// Commands, by name: what the keyboard shortcuts below do, and what the apps'
// menus and keys ask for (they call window.oeeeApp.command). Going somewhere
// is a plain page load, so a page that holds a drawing still asks before it
// is left.

function setTheme(choice: string): void {
  try {
    if (choice === "system") localStorage.removeItem("neo-cucumber-theme");
    else localStorage.setItem("neo-cucumber-theme", choice);
  } catch {
    // Applied to this page all the same.
  }
  if (choice === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", choice);
  showThemeChoice();
}

function go(path: string): void {
  loading(path);
  if (page.oeeeLoadingBar) {
    page.oeeeLoadingBar.leave(() => {
      location.href = path;
    });
  } else {
    location.href = path;
  }
}

function profilePath(): string {
  const link = document.querySelector('.toolbar-menu-items a[href^="/@"]');
  return link ? link.getAttribute("href")! : "/login";
}

function toggleShortcuts(open?: boolean): void {
  const panel = document.getElementById("ds-shortcuts");
  if (!panel) return;
  panel.hidden = open === undefined ? !panel.hidden : !open;
  if (!panel.hidden) {
    const close = panel.querySelector<HTMLElement>("[data-shortcuts-close]");
    if (close) close.focus();
  }
}

const commands: Record<string, () => void> = {
  recent: () => go("/"),
  following: () => go("/following"),
  communities: () => go("/communities"),
  together: () => go("/collaborate"),
  tags: () => go("/tags"),
  // The quick switcher, whose last row searches drawings -- except on
  // /search itself, where the page's own field is the one wanted. With
  // neither, /search is the one to go to.
  search: () => {
    const field =
      location.pathname === "/search" && document.querySelector<HTMLInputElement>('.search-page input[name="q"]');
    if (field) {
      field.focus();
      field.select();
    } else if (page.oeeeJump) {
      page.oeeeJump.open();
    } else {
      go("/search");
    }
  },
  notifications: () => go("/notifications"),
  drafts: () => go("/posts/drafts"),
  profile: () => go(profilePath()),
  account: () => go("/account"),
  about: () => go("/about"),
  "new-drawing": () => {
    const draw = document.getElementById("nav-draw-button") as HTMLButtonElement | null;
    if (draw && page.oeeeDrawDialog && page.oeeeDrawDialog.open()) return;
    if (draw && draw.form) {
      const form = draw.form;
      if (page.oeeeLoadingBar) page.oeeeLoadingBar.leave(() => form.submit());
      else form.submit();
    } else {
      go("/login");
    }
  },
  "theme-light": () => setTheme("light"),
  "theme-dark": () => setTheme("dark"),
  "theme-system": () => setTheme("system"),
  shortcuts: () => toggleShortcuts(),
  // The quick switcher (jump.jinja).
  jump: () => {
    if (page.oeeeJump) page.oeeeJump.open();
  },
};

/** Every command the apps may ask for (appContract.json lists the same). */
export const COMMAND_NAMES: readonly string[] = Object.keys(commands);

function command(name: string): boolean {
  const run = Object.prototype.hasOwnProperty.call(commands, name) ? commands[name] : undefined;
  if (run) run();
  return !!run;
}

// Takes the skeleton back down, if one is up, for a page that is not coming
// after all: one left for in the back-forward cache.
let restoring: (() => void) | null = null;

function restoreContent(): void {
  if (restoring) restoring();
}

function loading(path?: string): void {
  // Offline, the next page is not coming: the browser says so, or the app
  // puts up its own "can't be reached" (Unreachable.swift and the rest), and
  // the page that was here stays as it was rather than hidden behind the
  // skeleton of one that never arrives.
  if (navigator.onLine === false) return;
  // Nor for a page that asks before it is left, which may be stayed on
  // without a word to it (loading_bar.jinja, wouldAsk).
  if (page.oeeeApp && page.oeeeApp.wouldLoseWork && page.oeeeApp.wouldLoseWork()) return;
  markCurrent(path || location.pathname);
  const content = document.querySelector<HTMLElement>("main.ds-content");
  if (!content || content.querySelector(".ds-skeleton")) return;
  // Laid over the page, where the reader is looking, and the page hidden
  // rather than taken out: nothing scrolls. Taken out, the page gave up its
  // height and the reader's place with it -- and that place is what htmx's
  // history cache keeps for going back, taken as the next page arrives,
  // before anything here is asked; so going back found the page at its top.
  // The skeleton sits at the top of what shows, wherever that is: the
  // content area's own scroll on a wide screen, the window's on a phone.
  const kept: [HTMLElement, string][] = [];
  for (let c = 0; c < content.children.length; c++) {
    const child = content.children[c] as HTMLElement;
    kept.push([child, child.style.visibility]);
    child.style.visibility = "hidden";
  }
  const skeleton = skeletonFor(path || location.pathname);
  const position = content.style.position;
  content.style.position = "relative";
  skeleton.style.position = "absolute";
  // Below the toolbar, which sticks over a phone's page and floats over the
  // Mac app's content area.
  const header = document.querySelector(".ds-page > header");
  const shown = header ? header.getBoundingClientRect().bottom : 0;
  skeleton.style.top = content.scrollTop + Math.max(0, shown - content.getBoundingClientRect().top) + "px";
  skeleton.style.right = "0";
  skeleton.style.left = "0";
  content.appendChild(skeleton);
  function restore(): void {
    if (!skeleton.parentNode) return;
    skeleton.parentNode.removeChild(skeleton);
    markCurrent(location.pathname);
    for (let k = 0; k < kept.length; k++) kept[k][0].style.visibility = kept[k][1];
    content!.style.position = position;
  }
  restoring = restore;
  setTimeout(restore, 10000);
}

function closestTo(target: EventTarget | null, selector: string): Element | null {
  const element = target as Element | null;
  return element && element.closest ? element.closest(selector) : null;
}

function closeMenus(except: Element | null): void {
  const open = document.querySelectorAll(".toolbar-menu[open]");
  for (let j = 0; j < open.length; j++) {
    if (open[j] !== except) open[j].removeAttribute("open");
  }
}

/** The document's half, once: what outlives a boosted swap. */
export function installToolbar(): void {
  page.oeeeMarkCurrent = markCurrent;
  page.oeeeFitToolbar = fitToolbar;

  // Safari, bringing a page back from the back/forward cache exactly as it
  // was left, then fires the Navigation API's traverse `navigate` in it,
  // interceptable. htmx takes that for one of its own trips back and puts up
  // the address again -- from its history cache, or fetched fresh -- over the
  // page already showing it: a feed came back as its first batch, at the
  // top. So htmx is let to restore only an address the page is not already
  // showing. `shown` follows every swap, which is where a boosted trip or one
  // of htmx's own restores lands.
  let shown = location.href;
  document.addEventListener("htmx:after:swap", () => {
    shown = location.href;
  });
  const alreadyShown = (event: Event) => {
    if (location.href === shown) event.preventDefault();
  };
  document.addEventListener("htmx:history:cache:hit", alreadyShown);
  document.addEventListener("htmx:before:history:restore", alreadyShown);

  // Refitted as the window changes size, and once the page's fonts have
  // arrived, since a fallback face measures differently.
  let fitPending = false;
  window.addEventListener("resize", () => {
    if (fitPending) return;
    fitPending = true;
    (window.requestAnimationFrame || setTimeout)(() => {
      fitPending = false;
      fitToolbar();
    });
  });
  if (document.fonts && document.fonts.ready) {
    document.fonts.ready.then(() => fitToolbar());
  }

  if (page.oeeeApp) page.oeeeApp.command = command;
  // For the quick switcher, whose rows are places to go and commands to run.
  page.oeeeCommand = command;
  page.oeeeGo = go;
  // For looking at a page's skeleton from the console without leaving:
  // oeeeShowSkeleton("/communities").
  page.oeeeShowSkeleton = loading;

  // The keys. Only on the site's own pages -- the painter and the drawing app
  // have keys of their own -- and never while typing, or with a modifier
  // held, which belong to the browser and the system.
  let awaitingGo = false;
  let goTimer: ReturnType<typeof setTimeout> | undefined;
  const goKeys: Record<string, string> = {
    r: "recent",
    f: "following",
    c: "communities",
    t: "together",
    h: "tags",
    n: "notifications",
    d: "drafts",
    p: "profile",
  };
  document.addEventListener("keydown", (event) => {
    if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
    if (!document.body || !document.body.classList.contains("ds-page")) return;
    const target = event.target as HTMLElement | null;
    if (target && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName))) return;
    const key = event.key;
    const panel = document.getElementById("ds-shortcuts");
    if (key === "Escape" && panel && !panel.hidden) {
      toggleShortcuts(false);
      return;
    }
    if (awaitingGo) {
      awaitingGo = false;
      clearTimeout(goTimer);
      const lower = key.toLowerCase();
      const name = Object.prototype.hasOwnProperty.call(goKeys, lower) ? goKeys[lower] : undefined;
      if (name) {
        event.preventDefault();
        command(name);
      }
      return;
    }
    // `?` is shift and `/` on most layouts, and some report it as such.
    if (key === "?" || (key === "/" && event.shiftKey)) {
      event.preventDefault();
      toggleShortcuts();
    } else if (key === "/") {
      event.preventDefault();
      command("search");
    } else if (key === "g" || key === "G") {
      awaitingGo = true;
      goTimer = setTimeout(() => {
        awaitingGo = false;
      }, 1500);
    } else if (key === "n" || key === "N") {
      if (document.getElementById("nav-draw-button")) {
        event.preventDefault();
        command("new-drawing");
      }
    }
  });
  document.addEventListener("click", (event) => {
    const target = event.target as Element | null;
    if (!target || !target.closest) return;
    if (target.closest("[data-shortcuts-close]") || target.id === "ds-shortcuts") {
      toggleShortcuts(false);
    }
  });
  document.addEventListener("click", (event) => {
    const button = closestTo(event.target, "[data-theme-choice]");
    if (!button) return;
    setTheme(button.getAttribute("data-theme-choice")!);
  });
  // The account menu closes the way a menu does, on a click anywhere else and
  // on Escape, which <details> alone does not.
  document.addEventListener("click", (event) => {
    closeMenus(closestTo(event.target, ".toolbar-menu"));
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closeMenus(null);
  });
  // The bar's search button opens the quick switcher in place of going to
  // /search, which it links to for want of a script. Taken in the capture
  // phase, ahead of htmx's boost and the skeleton below, both of which would
  // otherwise treat it as the trip its href describes.
  document.addEventListener(
    "click",
    (event) => {
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const button = closestTo(event.target, ".toolbar-search");
      if (!button || !page.oeeeJump) return;
      event.preventDefault();
      event.stopPropagation();
      page.oeeeJump.open();
    },
    true,
  );

  // htmx's history cache keeps the page being left, to put back when the
  // reader comes back to it, and it keeps it as it is at the moment the next
  // one arrives -- which is under the skeleton laid for that one. The
  // skeleton is taken down first, so what comes back is the page and not a
  // picture of it loading.
  document.addEventListener("htmx:history:cache:before:save", restoreContent);
  document.addEventListener("click", (event) => {
    if (event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const link = closestTo(event.target, "a[href]") as HTMLAnchorElement | null;
    if (!link || link.target || link.hasAttribute("download")) return;
    // htmx takes a boosted link's click for itself, which cancels it; that is
    // still a trip to another page. Any other cancelled click is a script
    // that kept the page where it is.
    const boosted = link.closest('[hx-boost\\:inherited="true"]') && !link.closest('[hx-boost="false"]');
    if (event.defaultPrevented && !boosted) return;
    if (link.origin !== location.origin) return;
    if (link.pathname === location.pathname && link.search === location.search) return;
    // Only the toolbar's links -- its sections, the apps' tabs, and its menus
    // -- lay the next page's skeleton down: each is a section of the site of
    // its own, drawn at the press. A link inside a page is an ordinary trip
    // to the next, the page it is on kept up until that one has come, so
    // nothing about where the reader was is disturbed for coming back to it.
    // The page's own pills switch it in place (pill_macro.jinja). Except the
    // pages in FROM_ANYWHERE, a long way from wherever they are linked; but
    // not from a pill, which switches its own page in place.
    if (!link.closest(".nav-bar") && !(FROM_ANYWHERE.test(link.pathname) && !link.closest(".ds-segmented"))) return;
    loading(link.pathname);
  });
  document.addEventListener("submit", (event) => {
    const form = event.target as HTMLFormElement | null;
    if (event.defaultPrevented || !form || !form.getAttribute) return;
    if (
      form.hasAttribute("hx-post") ||
      form.hasAttribute("hx-get") ||
      form.hasAttribute("hx-put") ||
      form.hasAttribute("hx-delete")
    ) {
      return;
    }
    const action = form.getAttribute("action");
    loading(action && action.charAt(0) === "/" ? action.split("?")[0] : location.pathname);
  });
  window.addEventListener("pageshow", (event) => {
    // Back to a page kept in memory: it is shown as it was left.
    if (event.persisted) restoreContent();
  });
}
