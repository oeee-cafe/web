import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { inkAt, installRoom, layerPng, mountSession, settle, uninstallRoom } from "./test/fakeRoom";
import {
  caughtUp, replayStart, resetPoint, sequenced, snapshot, stroke, welcome,
} from "./test/frames";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A join replays history; the canvas that comes out of it has to be the one
 * everybody else is looking at.
 *
 * This drives the real session view -- the real painter, the real drain, the
 * real decoder -- from the frames a server would send, because every part of
 * that path had tests and the failure lived between them. A room checkpoints
 * roughly every five hundred messages, so a full replay begins with somebody
 * else's snapshots and a RESET_POINT that arrives after them, and what those
 * two do to each other is not reachable from a test of either one.
 *
 * The shape is taken from a session that lost its canvas in production
 * (c9b8321d, 2026-08-20): four snapshots at the checkpoint sequence, a handful
 * of operations sequenced after the base while the upload was in flight, the
 * RESET_POINT, then ordinary drawing. The artwork is not -- these marks are
 * rectangles at known coordinates so a missing layer is an assertion rather
 * than something somebody has to notice.
 */


/** The checkpoint's base, and the sequence its RESET_POINT lands on. */
const BASE_SEQ = 100;
const RESET_POINT_SEQ = 105;

/** Where each participant's work sits, so "whose layer went missing" is a
 * question about pixels rather than about bookkeeping. */
const MARK = {
  ownerOneSnapshot: { x: 4, y: 4 },
  ownerThreeSnapshot: { x: 30, y: 4 },
  ownerOneAfterBase: { x: 4, y: 24 },
  ownerTwoAfterReset: { x: 30, y: 36 },
};

beforeEach(() => {
  installRoom();
});

afterEach(() => {
  uninstallRoom();
});

describe("a full replay that begins at a checkpoint", () => {
  it("puts the checkpoint's canvas back before the operations sequenced after it", async () => {
    const socket = await mountSession();
    await act(async () => socket.open());
    await settle();

    // This client is umu, session id 3 -- the same id the production session
    // gave the participant whose canvas came back empty.
    await act(async () => socket.deliver(welcome(3)));
    await settle();

    const [ownerOneFg, ownerThreeFg, blank] = await Promise.all([
      layerPng(MARK.ownerOneSnapshot),
      layerPng(MARK.ownerThreeSnapshot),
      layerPng(null),
    ]);

    await act(async () => {
      socket.deliver(replayStart(0, RESET_POINT_SEQ + 2));

      // The checkpoint: a pair per participant, all at the base sequence.
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "foreground", ownerOneFg)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "background", blank)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 3, "foreground", ownerThreeFg)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 3, "background", blank)));

      // Sequenced after the base while the upload was in flight, so history
      // keeps them and the replay has to apply them on top of the snapshots.
      socket.deliver(sequenced(BASE_SEQ + 1, stroke(1, MARK.ownerOneAfterBase)));

      // The point itself, which is what tells a replaying client how many
      // snapshots the checkpoint had.
      socket.deliver(sequenced(RESET_POINT_SEQ, resetPoint(BASE_SEQ, 4)));

      // Ordinary drawing afterwards.
      socket.deliver(sequenced(RESET_POINT_SEQ + 1, stroke(2, MARK.ownerTwoAfterReset)));
      socket.deliver(sequenced(RESET_POINT_SEQ + 2, stroke(2, { x: MARK.ownerTwoAfterReset.x + 6, y: MARK.ownerTwoAfterReset.y })));
      socket.deliver(caughtUp(RESET_POINT_SEQ + 2));
    });
    await settle(12);

    // Drawing after the checkpoint is the easy half, and it working is what
    // makes a client believe the replay succeeded.
    expect(inkAt(MARK.ownerTwoAfterReset), "work sequenced after the reset point").toBe(true);

    // The checkpoint itself, which is every stroke the room made before it.
    expect(inkAt(MARK.ownerOneSnapshot), "another participant's checkpointed layer").toBe(true);
    expect(inkAt(MARK.ownerThreeSnapshot), "this client's own checkpointed layer").toBe(true);

    // And the operations that raced in after the base was chosen.
    expect(inkAt(MARK.ownerOneAfterBase), "work sequenced after the checkpoint base").toBe(true);
  });

  /**
   * The other half of the same failure, and the reason it went unnoticed.
   *
   * If the snapshots cannot be assembled the canvas has no way to hold what
   * the checkpoint stands for, and the only safe thing is to stop. Stepping
   * the canonical position over the base anyway leaves a client drawing on
   * against a canvas missing everything below it, reporting itself caught up
   * the whole time -- and reporting itself caught up is what lets it be handed
   * the *next* checkpoint to upload, which is how one client's incomplete
   * canvas becomes the room's history.
   */
  it("asks for the replay again rather than drawing on past a checkpoint it could not apply", async () => {
    const socket = await mountSession();
    await act(async () => socket.open());
    await settle();
    await act(async () => socket.deliver(welcome(3)));
    await settle();

    const [ownerOneFg, blank] = await Promise.all([
      layerPng(MARK.ownerOneSnapshot),
      layerPng(null),
    ]);

    await act(async () => {
      socket.deliver(replayStart(0, RESET_POINT_SEQ + 1));
      // Two of the four the point announces: a checkpoint that cannot be made
      // whole, however long the drain waits for the rest.
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "foreground", ownerOneFg)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "background", blank)));
      socket.deliver(sequenced(RESET_POINT_SEQ, resetPoint(BASE_SEQ, 4)));
      socket.deliver(sequenced(RESET_POINT_SEQ + 1, stroke(2, MARK.ownerTwoAfterReset)));
      socket.deliver(caughtUp(RESET_POINT_SEQ + 1));
    });
    await settle(12);

    // Nothing after the checkpoint is drawn, because the position never moved
    // past it.
    expect(inkAt(MARK.ownerTwoAfterReset), "work drawn on a canvas missing its checkpoint").toBe(false);
    // And the gap is what closes the socket, which is what asks for the
    // history again.
    expect(socket.closedWith, "hung up on the canonical gap").toBe(4000);
  });
});
