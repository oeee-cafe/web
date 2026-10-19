import { afterEach, describe, expect, it } from "vitest";
import { userEvent } from "vitest/browser";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import { DefaultI18n } from "neo-cucumber";
import { SessionHeader } from "./components/SessionHeader";
import { setupI18n } from "./i18n";
import type { DiscordFriend, DiscordLinked } from "../shared/appBridge";
import "./app.css";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Inviting Discord friends from the session header, in the Windows app.
 *
 * The app is stood in for by what app_bridge.jinja gives the page --
 * `oeeeApp.discord`, keeping what the app last said and asking it things --
 * and the oeee:discord event it says each thing with; what the app does
 * with what it is asked is oeee-cafe-desktop's (discord.rs).
 */

let host: HTMLElement | null = null;
let root: Root | null = null;
let asked: { action: string; user?: string }[] = [];

const ROOM = "/collaborate/9c881320-2b43-4afa-b2bb-7128c8a3e985";

function app(state: DiscordLinked | null) {
  asked = [];
  (window as unknown as { oeeeApp: unknown }).oeeeApp = {
    discord: {
      known: () => ({ state, friends: null }),
      ask: (action: string, user?: string) => {
        asked.push(user ? { action, user } : { action });
        return true;
      },
    },
  };
}

function room(joinable: boolean) {
  const meta = document.createElement("meta");
  meta.name = "oeee-presence";
  meta.content = "collaborating";
  if (joinable) meta.setAttribute("data-join", ROOM);
  document.head.appendChild(meta);
}

async function say(kind: "state" | "friends" | "requests" | "invited", value: unknown) {
  await act(async () => {
    document.dispatchEvent(new CustomEvent("oeee:discord", { detail: { kind, value } }));
  });
}

async function render() {
  setupI18n("en");
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(
      <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
        <SessionHeader
          canvasMeta={{
            title: "Untitled",
            width: 300,
            height: 300,
            ownerId: "o",
            ownerLoginName: "oeee",
            maxUsers: 4,
            currentUserCount: 1,
          }}
          connectionState="connected"
          isCatchingUp={false}
        />
      </I18nProvider>,
    );
  });
}

function button(label: string): HTMLButtonElement | undefined {
  return [...document.body.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);
}

async function press(label: string) {
  const found = button(label);
  if (!found) throw new Error(`no ${label} button; there are ${[...document.body.querySelectorAll("button")].map((b) => b.textContent)}`);
  await act(async () => found.click());
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  document.head.querySelectorAll('meta[name="oeee-presence"]').forEach((meta) => meta.remove());
  delete (window as unknown as { oeeeApp?: unknown }).oeeeApp;
  host = null;
  root = null;
});

const FRIENDS: DiscordFriend[] = [
  { id: "80351110224678912", name: "넬리", status: "online", playing: true },
  { id: "2", name: "pickle", status: "idle", playing: false },
  { id: "3", name: "mochi", status: "offline", playing: false },
];

describe("inviting Discord friends from the session", () => {
  it("is offered only by an app with Discord, in a session a friend could enter", async () => {
    // A browser, or an app without Discord: the app never said where the
    // account stands.
    app(null);
    room(true);
    await render();
    expect(button("Invite")).toBeUndefined();
    expect(button("Share")).toBeDefined();
    act(() => root!.unmount());
    host!.remove();

    // A session in a private community, which a friend could not enter.
    app({ connected: false, connecting: false, name: null });
    document.head.querySelectorAll('meta[name="oeee-presence"]').forEach((meta) => meta.remove());
    room(false);
    await render();
    expect(button("Invite")).toBeUndefined();
  });

  it("asks to connect first, saying why, and offers Discord's own way too", async () => {
    app({ connected: false, connecting: false, name: null });
    room(true);
    await render();
    await press("Invite");
    expect(document.body.textContent).toContain("Connect Discord once to see your friends here");
    expect(document.body.textContent).toContain("Or use + in any Discord chat.");
    await press("Connect Discord");
    expect(asked).toEqual([{ action: "connect" }]);

    // While Discord is asked, the button waits rather than asking again.
    await say("state", { connected: false, connecting: true, name: null });
    expect(document.body.textContent).toContain("Connecting to Discord…");
    expect(button("Connect Discord")!.disabled).toBe(true);

    await press("Not now");
    expect(document.body.textContent).not.toContain("Connecting to Discord…");
  });

  it("lists the friends once connected, and sends each one invitation", async () => {
    app({ connected: true, connecting: false, name: "oeee" });
    room(true);
    await render();
    await press("Invite");
    // Asked for as the picker opens.
    expect(asked).toEqual([{ action: "friends" }]);
    expect(document.body.textContent).toContain("Loading friends…");

    await say("friends", FRIENDS);
    expect(document.body.textContent).toContain("넬리");
    expect(document.body.textContent).toContain("in Oeee Cafe");
    expect(document.body.querySelectorAll("li")).toHaveLength(3);

    const first = document.body.querySelector("li")!.querySelector("button")!;
    await act(async () => first.click());
    expect(asked.at(-1)).toEqual({ action: "invite", user: "80351110224678912" });
    expect(first.textContent).toBe("Sending…");
    expect(first.disabled).toBe(true);

    await say("invited", { user: "80351110224678912", sent: true });
    expect(first.textContent).toBe("Sent");
    expect(first.disabled).toBe(true);

    // One that did not go through can be tried again.
    const second = document.body.querySelectorAll("li")[1].querySelector("button")!;
    await act(async () => second.click());
    await say("invited", { user: "2", sent: false });
    expect(second.textContent).toBe("Try again");
    expect(second.disabled).toBe(false);
  });

  it("lists friends who asked to join, lets them in by an invitation, and puts them off", async () => {
    app({ connected: true, connecting: false, name: "oeee" });
    room(true);
    await render();
    expect(document.body.textContent).not.toContain("asks to join");

    await say("requests", [
      { id: "80351110224678912", name: "넬리" },
      { id: "2", name: "pickle" },
    ]);
    expect(document.body.textContent).toContain("넬리 asks to join");
    expect(document.body.textContent).toContain("pickle asks to join");

    const first = button("Let in")!;
    await act(async () => first.click());
    expect(asked.at(-1)).toEqual({ action: "letIn", user: "80351110224678912" });
    expect(first.textContent).toBe("Sending…");
    expect(first.disabled).toBe(true);

    // Not let in: said so, and offered again.
    await say("invited", { user: "80351110224678912", sent: false });
    expect(document.body.textContent).toContain("Couldn’t let 넬리 in");
    expect(first.textContent).toBe("Try again");

    // Put off: gone from the list at once, and the app told.
    const [, second] = [...document.body.querySelectorAll("button")].filter((b) => b.textContent === "Not now");
    await act(async () => second.click());
    expect(asked.at(-1)).toEqual({ action: "dismiss", user: "2" });
    expect(document.body.textContent).not.toContain("pickle");

    // The app takes a friend let in off the list.
    await say("requests", []);
    expect(document.body.textContent).not.toContain("asks to join");
  });

  it("shows no requests in a session a friend could not enter", async () => {
    app({ connected: true, connecting: false, name: "oeee" });
    room(false);
    await render();
    await say("requests", [{ id: "2", name: "pickle" }]);
    expect(document.body.textContent).not.toContain("pickle");
  });

  it("gives the buttons a row of their own, apart from who and whether", async () => {
    app({ connected: false, connecting: false, name: null });
    room(true);
    await render();
    const invite = button("Invite")!;
    const share = button("Share")!;
    const owner = [...host!.querySelectorAll("div")].find((d) => d.textContent?.trim() === "by @oeee")!;
    // Below the owner's name, side by side, and not touching.
    expect(invite.getBoundingClientRect().top).toBeGreaterThanOrEqual(owner.getBoundingClientRect().bottom);
    expect(share.getBoundingClientRect().left - invite.getBoundingClientRect().right).toBeGreaterThanOrEqual(4);
  });

  it("opens the friends in a window of their own, which × or Invite again puts away", async () => {
    app({ connected: true, connecting: false, name: "oeee" });
    room(true);
    await render();
    await press("Invite");
    const window_ = document.body.querySelector<HTMLElement>('[role="dialog"]')!;
    expect(window_).not.toBeNull();
    expect(window_.textContent).toContain("Discord friends");
    // On the page, not inside the header it was opened from.
    expect(host!.contains(window_)).toBe(false);
    expect(button("Invite")!.getAttribute("aria-expanded")).toBe("true");

    // Pressed as a pointer presses it, which is what the title bar's drag
    // takes for itself as it goes down: a click() alone never tells.
    await act(() => userEvent.click(window_.querySelector<HTMLButtonElement>('button[aria-label="Close"]')!));
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    expect(button("Invite")!.getAttribute("aria-expanded")).toBe("false");

    await press("Invite");
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();
    await press("Invite");
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  });

  it("finds a friend by name", async () => {
    app({ connected: true, connecting: false, name: "oeee" });
    room(true);
    await render();
    await press("Invite");
    await say("friends", FRIENDS);
    const search = document.body.querySelector<HTMLInputElement>('input[type="search"]')!;
    const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => {
      setValue.call(search, "PICK");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(document.body.querySelectorAll("li")).toHaveLength(1);
    expect(document.body.textContent).toContain("pickle");

    await act(async () => {
      setValue.call(search, "nobody");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(document.body.textContent).toContain("No friend by that name.");
  });
});

describe("inviting Steam friends from the session", () => {
  let askedSteam: { action: string; user?: string; join?: string }[] = [];

  /** The Steam build, as app_bridge.jinja stands for it. */
  function steamApp(discord: DiscordLinked | null = null) {
    askedSteam = [];
    (window as unknown as { oeeeApp: unknown }).oeeeApp = {
      connected: () => true,
      discord: { known: () => ({ state: discord, friends: null }), ask: () => true },
      steam: {
        known: () => ({ friends: null }),
        ask: (action: string, user?: string, join?: string) => {
          askedSteam.push({ action, ...(user ? { user } : {}), ...(join ? { join } : {}) });
          return true;
        },
      },
    };
  }

  async function sayFromSteam(kind: "friends" | "invited", value: unknown) {
    await act(async () => {
      document.dispatchEvent(new CustomEvent("oeee:steam", { detail: { kind, value } }));
    });
  }

  afterEach(() => document.documentElement.removeAttribute("data-store"));

  it("lists the Steam friends in the page, and sends each one Steam's invitation to the room", async () => {
    document.documentElement.setAttribute("data-store", "steam");
    steamApp();
    room(true);
    await render();
    expect(button("Invite")).toBeUndefined();
    await press("Steam");
    expect(askedSteam).toEqual([{ action: "friends" }]);
    const window_ = document.body.querySelector<HTMLElement>('[role="dialog"]')!;
    expect(window_.getAttribute("aria-label")).toBe("Invite Steam friends");
    expect(window_.textContent).toContain("Steam friends");
    expect(window_.textContent).toContain("Loading friends…");

    await sayFromSteam("friends", [
      { id: "76561197960287930", name: "넬리", status: "online", playing: true },
      { id: "76561197960287931", name: "pickle", status: "offline", playing: false },
    ]);
    expect(window_.textContent).toContain("in Oeee Cafe");
    const first = window_.querySelector("li")!.querySelector("button")!;
    await act(async () => first.click());
    expect(askedSteam.at(-1)).toEqual({ action: "invite", user: "76561197960287930", join: ROOM });
    expect(first.textContent).toBe("Sending…");
    await sayFromSteam("invited", { user: "76561197960287930", sent: true });
    expect(first.textContent).toBe("Sent");
    expect(first.disabled).toBe(true);

    // Not taken by Steam: offered again.
    const second = window_.querySelectorAll("li")[1].querySelector("button")!;
    await act(async () => second.click());
    await sayFromSteam("invited", { user: "76561197960287931", sent: false });
    expect(second.textContent).toBe("Try again");

    // Steam again puts it away.
    await press("Steam");
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  });

  it("says when there is nobody to invite", async () => {
    document.documentElement.setAttribute("data-store", "steam");
    steamApp();
    room(true);
    await render();
    await press("Steam");
    await sayFromSteam("friends", []);
    expect(document.body.textContent).toContain("No Steam friends to invite yet.");
  });

  it("swaps the one friends window between Discord's and Steam's", async () => {
    document.documentElement.setAttribute("data-store", "steam");
    steamApp({ connected: true, connecting: false, name: "oeee" });
    room(true);
    await render();
    await press("Invite");
    expect(document.body.querySelector('[role="dialog"]')!.textContent).toContain("Discord friends");
    await press("Steam");
    const dialogs = document.body.querySelectorAll('[role="dialog"]');
    expect(dialogs).toHaveLength(1);
    expect(dialogs[0].textContent).toContain("Steam friends");
    expect(button("Steam")!.getAttribute("aria-expanded")).toBe("true");
    expect(button("Invite")!.getAttribute("aria-expanded")).toBe("false");
  });

  it("is offered nowhere else, nor in a session a friend could not enter", async () => {
    // The Microsoft Store's build has no Steam.
    document.documentElement.setAttribute("data-store", "microsoft");
    steamApp();
    room(true);
    await render();
    expect(button("Steam")).toBeUndefined();
    act(() => root!.unmount());
    host!.remove();

    document.documentElement.setAttribute("data-store", "steam");
    document.head.querySelectorAll('meta[name="oeee-presence"]').forEach((meta) => meta.remove());
    room(false);
    await render();
    expect(button("Steam")).toBeUndefined();
  });
});
