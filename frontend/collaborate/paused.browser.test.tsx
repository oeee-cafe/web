import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import {
  FakeServer, installRoom, mountSession, settle, settleUntil, sockets, uninstallRoom,
} from "./test/fakeRoom";

/**
 * A room paused after half an hour without drawing: the page says so and
 * offers to resume it, and does not come back on its own -- coming back is
 * what resumes the room, and that is the person's to choose.
 */

afterEach(() => {
  uninstallRoom();
});

describe("a room that paused", () => {
  it("says so, offers to resume, and does not reconnect by itself", async () => {
    installRoom();
    const server = new FakeServer();
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle();

    await act(async () => socket.hangUp(4002));
    await settleUntil(() => (document.body.textContent ?? "").includes("Session Paused"));
    expect([...document.querySelectorAll("button")].some((b) => b.textContent === "Resume")).toBe(true);
    // Not the connection-lost dialog: there is nothing to reconnect to.
    expect([...document.querySelectorAll("button")].some((b) => b.textContent?.includes("Reconnect"))).toBe(false);

    await new Promise((resolve) => setTimeout(resolve, 1500));
    expect(sockets).toHaveLength(1);
  });
});
