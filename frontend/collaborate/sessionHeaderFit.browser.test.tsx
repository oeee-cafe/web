import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import { DefaultI18n, NEO_PANEL } from "neo-cucumber";
import "neo-cucumber/style.css";
import { SessionHeader } from "./components/SessionHeader";
import { setupI18n } from "./i18n";
import "./app.css";

/**
 * The chat window's header at the width the window opens at, with every
 * control it can carry: Discord's Invite and Steam's (the desktop app's),
 * Just Watch, Share, and the owner's two checkboxes. Nothing in it may run
 * past its edge, and no button may break onto a second line.
 */

const CHAT_WIDTH = 224;
const ROOM = "/collaborate/9c881320-2b43-4afa-b2bb-7128c8a3e985";

let host: HTMLDivElement | null = null;
let root: Root | null = null;

function inTheApp() {
  document.documentElement.setAttribute("data-store", "steam");
  (window as unknown as { oeeeApp: unknown }).oeeeApp = {
    connected: () => true,
    steam: { known: () => ({ friends: null }), ask: () => true },
    discord: {
      known: () => ({ state: { connected: true, connecting: false, name: "oeee" }, friends: null }),
      ask: () => true,
    },
  };
  const meta = document.createElement("meta");
  meta.name = "oeee-presence";
  meta.content = "collaborating";
  meta.setAttribute("data-join", ROOM);
  document.head.appendChild(meta);
}

async function render(locale: string, owner: boolean) {
  setupI18n(locale);
  host = document.createElement("div");
  host.className = NEO_PANEL;
  host.style.width = `${CHAT_WIDTH}px`;
  document.body.appendChild(host);
  root = createRoot(host);
  const toggle = { allowed: true, busy: false, onToggle: () => {} };
  await act(async () => {
    root!.render(
      <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
        <SessionHeader
          canvasMeta={{
            title: "Untitled",
            width: 300,
            height: 300,
            ownerId: "o",
            ownerLoginName: "a_rather_long_login_name",
            maxUsers: 4,
            currentUserCount: 1,
          }}
          guests={owner ? toggle : undefined}
          spectators={owner ? toggle : undefined}
          mode={owner ? undefined : { watching: false, seatFree: true, canWatch: true, onSwitch: () => {} }}
          signUp={owner ? undefined : { href: "/signup?next=%2Fcollaborate%2Froom", onFollow: () => {} }}
          connectionState="connected"
          isCatchingUp={false}
        />
      </I18nProvider>,
    );
  });
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  document.head.querySelectorAll('meta[name="oeee-presence"]').forEach((meta) => meta.remove());
  document.documentElement.removeAttribute("data-store");
  delete (window as unknown as { oeeeApp?: unknown }).oeeeApp;
  host = null;
  root = null;
});

describe("the session header at the chat window's opening width", () => {
  for (const locale of ["en", "ko", "ja", "zh"]) {
    for (const owner of [true, false]) {
      it(`fits in ${locale} for ${owner ? "the owner" : "everybody else"}`, async () => {
        inTheApp();
        await render(locale, owner);
        const header = host!.firstElementChild as HTMLElement;
        const buttons = [...header.querySelectorAll("button")];
        expect(buttons.length, "Invite, Steam, Share, and Just Watch for others").toBe(owner ? 3 : 4);
        const box = header.getBoundingClientRect();
        for (const element of header.querySelectorAll<HTMLElement>("*")) {
          const rect = element.getBoundingClientRect();
          if (rect.width === 0) continue;
          expect(rect.right, `${element.tagName} "${element.textContent}" past the edge`).toBeLessThanOrEqual(box.right + 0.5);
        }
        const heights = buttons.map((button) => Math.round(button.getBoundingClientRect().height));
        for (const button of buttons) {
          expect(button.scrollWidth, `"${button.textContent}" cut off`).toBeLessThanOrEqual(button.clientWidth + 1);
        }
        const report = buttons.map((button, i) => `${button.textContent?.trim()}=${heights[i]}`).join(", ");
        // One line each: an icon a pixel shorter than another is not a wrap.
        expect(Math.max(...heights) - Math.min(...heights), `buttons on one line: ${report}`).toBeLessThanOrEqual(2);
        if (owner) {
          const labels = [...header.querySelectorAll("label")].map((label) => label.getBoundingClientRect());
          expect(labels).toHaveLength(2);
          for (const label of labels) expect(label.height).toBeLessThanOrEqual(16);
        }
      });
    }
  }
});
