import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { decodeMessage } from "./binaryProtocol";
import {
  FakeServer, type FakeSocket, installRoom, mountPage, mountSession, pointer, settle,
  settleUntil, sockets, uninstallRoom,
} from "./test/fakeRoom";

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
});
