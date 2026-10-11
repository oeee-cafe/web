import { afterEach, describe, expect, it } from "vitest";
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

async function say(kind: "state" | "friends" | "invited", value: unknown) {
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
  return [...host!.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);
}

async function press(label: string) {
  const found = button(label);
  if (!found) throw new Error(`no ${label} button; there are ${[...host!.querySelectorAll("button")].map((b) => b.textContent)}`);
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
    expect(host!.textContent).toContain("Connect Discord once to see your friends here");
    expect(host!.textContent).toContain("Or use + in any Discord chat.");
    await press("Connect Discord");
    expect(asked).toEqual([{ action: "connect" }]);

    // While Discord is asked, the button waits rather than asking again.
    await say("state", { connected: false, connecting: true, name: null });
    expect(host!.textContent).toContain("Connecting to Discord…");
    expect(button("Connect Discord")!.disabled).toBe(true);

    await press("Not now");
    expect(host!.textContent).not.toContain("Connecting to Discord…");
  });

  it("lists the friends once connected, and sends each one invitation", async () => {
    app({ connected: true, connecting: false, name: "oeee" });
    room(true);
    await render();
    await press("Invite");
    // Asked for as the picker opens.
    expect(asked).toEqual([{ action: "friends" }]);
    expect(host!.textContent).toContain("Loading friends…");

    await say("friends", FRIENDS);
    expect(host!.textContent).toContain("넬리");
    expect(host!.textContent).toContain("in Oeee Cafe");
    expect(host!.querySelectorAll("li")).toHaveLength(3);

    const first = host!.querySelector("li")!.querySelector("button")!;
    await act(async () => first.click());
    expect(asked.at(-1)).toEqual({ action: "invite", user: "80351110224678912" });
    expect(first.textContent).toBe("Sending…");
    expect(first.disabled).toBe(true);

    await say("invited", { user: "80351110224678912", sent: true });
    expect(first.textContent).toBe("Sent");
    expect(first.disabled).toBe(true);

    // One that did not go through can be tried again.
    const second = host!.querySelectorAll("li")[1].querySelector("button")!;
    await act(async () => second.click());
    await say("invited", { user: "2", sent: false });
    expect(second.textContent).toBe("Try again");
    expect(second.disabled).toBe(false);
  });

  it("finds a friend by name", async () => {
    app({ connected: true, connecting: false, name: "oeee" });
    room(true);
    await render();
    await press("Invite");
    await say("friends", FRIENDS);
    const search = host!.querySelector<HTMLInputElement>('input[type="search"]')!;
    const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => {
      setValue.call(search, "PICK");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(host!.querySelectorAll("li")).toHaveLength(1);
    expect(host!.textContent).toContain("pickle");

    await act(async () => {
      setValue.call(search, "nobody");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(host!.textContent).toContain("No friend by that name.");
  });
});

describe("inviting Steam friends from the session", () => {
  let invited: string[] = [];

  function steamApp() {
    invited = [];
    (window as unknown as { oeeeApp: unknown }).oeeeApp = {
      connected: () => true,
      discord: { known: () => ({ state: null, friends: null }), ask: () => true },
      steam: {
        invite: (join: string) => {
          invited.push(join);
          return true;
        },
      },
    };
  }

  afterEach(() => document.documentElement.removeAttribute("data-store"));

  it("opens Steam's own invitation for the room, in the Steam build", async () => {
    document.documentElement.setAttribute("data-store", "steam");
    steamApp();
    room(true);
    await render();
    expect(button("Invite")).toBeUndefined();
    await press("Steam");
    expect(invited).toEqual([ROOM]);
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
