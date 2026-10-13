import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import {
  FakeServer, installRoom, mountSession, pointer, settle, settleUntil, uninstallRoom,
} from "./test/fakeRoom";
import { layersFrame, stroke } from "./test/frames";

/**
 * Whether leaving the room asks first. Only where leaving loses something:
 * a mark not yet confirmed, or the owner walking away from an unsaved
 * drawing that nobody else is drawing in. Everybody else's marks are the
 * server's, and coming back replays them.
 */

const OTHER = "aaaaaaaa-0000-4000-8000-000000000002";
let server: FakeServer;

afterEach(() => {
  uninstallRoom();
});

/** Whether the page asks: what the browser's own dialog hangs on. */
function asks(): boolean {
  const event = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(event);
  return event.defaultPrevented;
}

async function room(options: { owner: boolean }) {
  installRoom(options.owner ? { ownerId: "umu-uuid" } : {});
  server = new FakeServer();
  const socket = await mountSession();
  await act(async () => server.admit(socket, 3));
  await settle();
  // Something on the canvas, drawn and confirmed.
  server.sequence(stroke(2, { x: 10, y: 10 }));
  await settleUntil(() => socket.delivered.length > 0);
  await settle();
  return socket;
}

describe("leaving a collaborative room", () => {
  it("does not ask somebody who is not the owner", async () => {
    await room({ owner: false });
    expect(asks()).toBe(false);
  });

  it("asks the owner of an unsaved drawing nobody else is drawing in", async () => {
    await room({ owner: true });
    expect(asks()).toBe(true);
  });

  it("does not ask the owner while somebody else is drawing", async () => {
    const socket = await room({ owner: true });
    await act(async () =>
      socket.deliver(layersFrame([{ userId: OTHER, sessionId: 2, name: "bob" }])),
    );
    await settle();
    expect(asks()).toBe(false);
  });

  it("asks the owner again when only watchers are left", async () => {
    const socket = await room({ owner: true });
    await act(async () =>
      socket.deliver(layersFrame([{ userId: OTHER, sessionId: 0, name: "bob", flags: 0b10 }])),
    );
    await settle();
    expect(asks()).toBe(true);
  });

  it("asks anybody whose last stroke the server has not confirmed yet", async () => {
    const socket = await room({ owner: false });
    // The server stops answering: nothing this tab sends comes back.
    socket.sink = () => {};
    await pointer().drag({ x: 20, y: 20 }, { x: 40, y: 20 });
    await settle();
    expect(asks()).toBe(true);
  });
});
