import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { FLAG_GUEST } from "./binaryProtocol";
import { FakeServer, installRoom, mountSession, settle, settleUntil, uninstallRoom } from "./test/fakeRoom";
import { layersFrame, leaveFrame } from "./test/frames";

/**
 * Who the room says is here. Somebody who left, or whom the owner sent out,
 * used to stay listed until the next roster -- which came only when
 * somebody else joined -- so a banned guest looked as if they were still in
 * the room.
 */

const ME = "aaaaaaaa-0000-4000-8000-000000000001";
const BOB = "aaaaaaaa-0000-4000-8000-000000000002";

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
});
