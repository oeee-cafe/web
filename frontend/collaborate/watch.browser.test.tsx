import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { decodeMessage, FLAG_SPECTATOR } from "./binaryProtocol";
import {
  FakeServer, type FakeSocket, installRoom, mountPage, mountSession, pointer, settle,
  settleUntil, sockets, uninstallRoom,
} from "./test/fakeRoom";
import { layersFrame, stroke } from "./test/frames";

/**
 * Watching a room rather than drawing in it: asked for with `?watch=1`, or
 * chosen when a room is full. The server is what refuses a spectator's marks;
 * the page is what keeps them from making any.
 */

let server: FakeServer;

beforeEach(() => {
  server = new FakeServer();
});

afterEach(() => {
  uninstallRoom();
});

const sentKinds = (socket: FakeSocket) =>
  socket.sent.map((frame) => decodeMessage(frame)).flatMap((message) => (message ? [message.type] : []));

const text = () => document.body.textContent ?? "";

/** Whose layers the toolbox lists, by the names on its rows. */
const layerRows = () =>
  Array.from(document.querySelectorAll("button[aria-label^='Draw on ']")).map((row) =>
    (row.getAttribute("aria-label") ?? "").replace(/^Draw on (.*)'s layers$/, "$1"),
  );

describe("watching a room", () => {
  it("asks the server for a place watching, and draws nothing", async () => {
    installRoom();
    window.history.replaceState(null, "", `${window.location.pathname}?watch=1`);
    const socket = await mountSession();
    expect(new URL(socket.url).searchParams.get("spectate")).toBe("1");
    await act(async () => server.admit(socket, 0));
    await settle();

    expect(text()).toContain("Watching");
    expect(text()).toContain("Join Drawing");

    await pointer().drag({ x: 10, y: 10 }, { x: 30, y: 10 });
    await settle();
    expect(sentKinds(socket).filter((kind) => kind !== "join")).toEqual([]);
  });

  // The engine keeps a pair for whoever is at the keyboard, and a spectator's
  // was listed as a blank layer named "0", the id they do not have.
  it("lists the room's layers and none of its own", async () => {
    installRoom();
    window.history.replaceState(null, "", `${window.location.pathname}?watch=1`);
    server.sequence(stroke(1, { x: 10, y: 10 }));
    server.sequence(stroke(2, { x: 20, y: 20 }));
    const socket = await mountSession();
    await act(async () => server.admit(socket, 0));
    await act(async () =>
      socket.deliver(
        layersFrame([
          { userId: "aaaaaaaa-0000-4000-8000-000000000001", sessionId: 1, name: "alice" },
          { userId: "aaaaaaaa-0000-4000-8000-000000000002", sessionId: 2, name: "bob" },
          { userId: "aaaaaaaa-0000-4000-8000-000000000003", sessionId: 0, name: "umu", flags: FLAG_SPECTATOR },
        ]),
      ),
    );
    await settleUntil(() => layerRows().length > 0);
    await settle();
    expect(layerRows().sort()).toEqual(["alice", "bob"]);
  });

  it("offers to watch a full room, and watches it", async () => {
    installRoom({ full: true });
    await mountPage();
    await settleUntil(() => text().includes("Session Full"));
    const watch = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent === "Watch",
    );
    expect(watch, "a Watch button beside Retry").toBeTruthy();

    await act(async () => watch!.click());
    await settleUntil(() => sockets.length > 0);
    expect(new URL(sockets[0].url).searchParams.get("spectate")).toBe("1");
    expect(window.location.search).toBe("?watch=1");
  });

  it("lets somebody who has drawn here back to draw in a full room", async () => {
    installRoom({ full: true, hasDrawn: true });
    const socket = await mountSession();
    expect(text()).not.toContain("Session Full");
    expect(new URL(socket.url).searchParams.get("spectate")).toBeNull();
  });

});

describe("a guest in the room", () => {
  it("is offered an account, and the sign-up comes back to this room", async () => {
    installRoom({ guest: true });
    window.history.replaceState(null, "", `${window.location.pathname}?watch=1`);
    const socket = await mountSession();
    await act(async () => server.admit(socket, 0));
    await settle();
    const link = [...document.querySelectorAll("a")].find((a) => a.textContent === "Sign up");
    expect(link, "a Sign up link in the header").toBeTruthy();
    const next = new URL(link!.href).searchParams.get("next");
    expect(next).toBe(`${window.location.pathname}?watch=1`);
  });

  it("is not offered one when signed in", async () => {
    installRoom();
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle();
    expect([...document.querySelectorAll("a")].some((a) => a.textContent === "Sign up")).toBe(false);
  });
});
