import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { FLAG_GUEST, FLAG_LEFT, FLAG_SPECTATOR } from "./binaryProtocol";
import {
  FakeServer, installRoom, mountPage, mountSession, settle, settleUntil, uninstallRoom,
} from "./test/fakeRoom";
import { layersFrame, leaveFrame, stroke } from "./test/frames";

/**
 * Who the room says is here. Somebody who left, or whom the owner sent out,
 * used to stay listed until the next roster -- which came only when
 * somebody else joined -- so a banned guest looked as if they were still in
 * the room.
 */

const ME = "aaaaaaaa-0000-4000-8000-000000000001";
const BOB = "aaaaaaaa-0000-4000-8000-000000000002";
const CAROL = "aaaaaaaa-0000-4000-8000-000000000003";
const DAN = "aaaaaaaa-0000-4000-8000-000000000004";

let server: FakeServer;

beforeEach(() => {
  installRoom();
  server = new FakeServer();
});

afterEach(() => {
  uninstallRoom();
});

const count = () =>
  document.querySelector('[aria-label^="Participants ("]')?.textContent ?? null;
const listed = (name: string) =>
  Array.from(document.querySelectorAll(".flex-wrap > div")).some((chip) =>
    chip.textContent?.startsWith(name),
  );

describe("the participant list", () => {
  it("drops somebody as soon as the room says they left", async () => {
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await act(async () =>
      socket.deliver(
        layersFrame([
          { userId: ME, sessionId: 3, name: "umu" },
          { userId: BOB, sessionId: 2, name: "bob", flags: FLAG_GUEST },
        ]),
      ),
    );
    await settleUntil(() => count() === "2");
    expect(listed("bob")).toBe(true);

    await act(async () => socket.deliver(leaveFrame(BOB, "bob", FLAG_GUEST)));
    await settleUntil(() => count() === "1");
    expect(listed("bob"), "gone from who is here").toBe(false);
    expect(listed("umu")).toBe(true);

    // Back in: the server's next roster has them, and so does the list.
    await act(async () =>
      socket.deliver(
        layersFrame([
          { userId: ME, sessionId: 3, name: "umu" },
          { userId: BOB, sessionId: 2, name: "bob", flags: FLAG_GUEST },
        ]),
      ),
    );
    await settle();
    expect(count()).toBe("2");
  });

  // The roster used to list only who was here, so a page that came in or
  // reconnected after somebody left knew their layers by id alone.
  it("names the layers of somebody who has left, without counting them", async () => {
    server.sequence(stroke(2, { x: 10, y: 10 }));
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await act(async () =>
      socket.deliver(
        layersFrame([
          { userId: BOB, sessionId: 2, name: "bob", flags: FLAG_GUEST | FLAG_LEFT },
          { userId: ME, sessionId: 3, name: "umu" },
        ]),
      ),
    );
    await settleUntil(() => count() === "1");
    expect(listed("bob"), "not among who is here").toBe(false);
    await settleUntil(() => document.querySelector("button[aria-label=\"Draw on bob's layers\"]") !== null);
    expect(document.querySelector("button[aria-label=\"Draw on 2's layers\"]")).toBeNull();
  });
});

describe("somebody the owner removed", () => {
  it("is told so on coming back, rather than that sign-in failed", async () => {
    uninstallRoom();
    installRoom({ removed: true });
    await mountPage();
    await settleUntil(() => document.body.textContent?.includes("Removed from Session") ?? false);
    expect(document.body.textContent).not.toContain("Authentication Failed");
    // The room's canvas is not theirs to take a copy of.
    expect(document.body.textContent).not.toContain("Save as PNG");
    expect(document.body.textContent).toContain("Return to Lobby");
  });
});

describe("the party the app shows beside a Discord invite", () => {
  it("counts the people drawing, not the ones watching", async () => {
    const meta = document.createElement("meta");
    meta.setAttribute("name", "oeee-presence");
    meta.setAttribute("data-join", "/collaborate/room");
    meta.setAttribute("data-seats", "2");
    document.head.appendChild(meta);
    try {
      const socket = await mountSession();
      await act(async () => server.admit(socket, 3));
      await act(async () =>
        socket.deliver(
          layersFrame([
            { userId: ME, sessionId: 3, name: "umu" },
            { userId: BOB, sessionId: 2, name: "bob" },
            { userId: CAROL, sessionId: 0, name: "carol", flags: FLAG_SPECTATOR },
            { userId: DAN, sessionId: 0, name: "dan", flags: FLAG_GUEST | FLAG_SPECTATOR },
          ]),
        ),
      );
      await settleUntil(() => meta.getAttribute("data-party") === "2");

      await act(async () => socket.deliver(leaveFrame(BOB, "bob")));
      await settleUntil(() => meta.getAttribute("data-party") === "1");
    } finally {
      meta.remove();
    }
  });
});
