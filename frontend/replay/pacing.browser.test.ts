import { describe, expect, it } from "vitest";

/**
 * Playback has to keep the clock it recorded.
 *
 * These are timing assertions, which are usually a bad idea; they are here
 * because the bug they cover was invisible to every other kind of test. The
 * player scheduled one timer per message, and a nested timer cannot fire
 * sooner than the browser's clamp -- so a burst of messages the server
 * sequenced inside one millisecond replayed at four milliseconds each, and a
 * session that was recorded in bursts stuttered through all of them. Every
 * assertion about pixels still passed.
 *
 * The bounds are wide on purpose: what is being caught is a scheduler that is
 * out by orders of magnitude, not one that is out by a frame.
 */
import { mount, type PainterHandle, type PainterOperation } from "neo-cucumber";
import { encodePainterOperation } from "../collaborate/binaryProtocol";
import { decodeArchive, type ArchivedEntry } from "./archiveLog";
import { createReplay } from "./player";

const MAGIC = [0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x01];

function stroke(x: number): PainterOperation {
  return {
    kind: "stroke", layer: "foreground", brushSize: 2, brush: "solid",
    color: { r: 0, g: 0, b: 0, a: 255 },
    points: [{ x: x % 200, y: 10 }, { x: (x % 200) + 2, y: 12 }],
    mask: { type: 0, r: 0, g: 0, b: 0 },
  };
}

function archive(count: number, gap: number): Uint8Array {
  const bytes: number[] = [...MAGIC];
  for (let i = 0; i < count; i++) {
    const payload = new Uint8Array(encodePainterOperation(1, stroke(i)));
    const header = new TextEncoder().encode(`1|conn||${i + 1}|h\n`);
    const frame = [...header, ...payload];
    const prefix = new Uint8Array(12);
    const view = new DataView(prefix.buffer);
    view.setUint32(0, frame.length, true);
    view.setBigUint64(4, BigInt(1_700_000_000_000 + i * gap), true);
    bytes.push(...prefix, ...frame);
  }
  return Uint8Array.from(bytes);
}

describe("playback pacing", () => {
  async function play(count: number, gap: number): Promise<number> {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const entries = decodeArchive(archive(count, gap)) as ArchivedEntry[];
    let painter: PainterHandle | null = null;
    const newPainter = async () => {
      painter?.unmount();
      host.textContent = "";
      const next = mount(host, {
        width: 256,
        height: 64,
        mode: { kind: "standard" },
        controls: { kind: "none" },
        recordReplay: false,
        synchronization: { actorId: "v", onOperation: () => {} },
      });
      await next.ready;
      next.setInteractionEnabled(false);
      painter = next;
      return next;
    };
    const first = await newPainter();
    let applied = -1;
    const replay = createReplay({
      painter: first,
      entries,
      remount: newPainter,
      onProgress: (index) => {
        applied = index;
      },
    });
    const started = performance.now();
    replay.play();
    await new Promise<void>((resolve) => {
      const check = setInterval(() => {
        if (applied >= replay.length - 1) {
          clearInterval(check);
          resolve();
        }
      }, 5);
    });
    const wall = performance.now() - started;
    replay.destroy();
    painter?.unmount();
    host.remove();
    expect(applied).toBe(replay.length - 1);
    return wall;
  }

  /**
   * A burst was recorded in no time and has to replay in no time. This is the
   * case that was three and a half seconds of stutter.
   */
  it("replays messages recorded at the same instant together", async () => {
    const wall = await play(800, 0);
    expect(wall).toBeLessThan(500);
  }, 120_000);

  /** And a session drawn at a human pace still takes about as long as it did. */
  it("keeps the pace of a session that was drawn slowly", async () => {
    const recorded = 300 * 5;
    const wall = await play(300, 5);
    expect(wall).toBeGreaterThan(recorded * 0.7);
    expect(wall).toBeLessThan(recorded * 2);
  }, 120_000);
});
