import { describe, expect, it } from "vitest";
import { DEFAULT_SPEED_INDEX, ReplayPlayer, SPEEDS } from "./ReplayPlayer";
import { NeoReplay } from "../neo/NeoReplay";
import { describeDifference, firstPixelDifference } from "../test/neoHarness";

const W = 64;
const H = 64;

const header = (x: number, y: number, size = 4, r = 0, g = 0, b = 0) => [
  "freeHand", 0, r, g, b, 255, 0, 0, 0, size, 0, 1, x, y, x, y,
];

const items = [
  ["floodFill", 0, 0, 0, (255 << 24) | (0xd6 << 16) | (0xe0 << 8) | 0xf0],
  [...header(8, 8), 20, 18, 34, 30, 50, 46],
  [...header(50, 10, 8, 128, 0, 0), 30, 30, 12, 52],
  [...header(32, 32, 1), 33, 33],
];

function displayContext() {
  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("no 2d context");
  return ctx;
}

const pixels = (ctx: CanvasRenderingContext2D) =>
  new Uint8ClampedArray(ctx.getImageData(0, 0, W, H).data);

/** The bottom-right pixel, which none of the strokes below reach. */
const cornerOf = (px: Uint8ClampedArray) => {
  const i = ((H - 1) * W + (W - 1)) * 4;
  return [px[i], px[i + 1], px[i + 2], px[i + 3]];
};

/** Flattens a NeoReplay's two layers the way the player composites them. */
function compositeOf(replay: NeoReplay) {
  const ctx = displayContext();
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, W, H);
  ctx.drawImage(replay.painter.canvas[0], 0, 0);
  ctx.drawImage(replay.painter.canvas[1], 0, 0);
  return pixels(ctx);
}

/** Generated rather than hand-written, so the pixels are what we think. */
function pngOf(paint: (ctx: CanvasRenderingContext2D) => void): string {
  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d")!;
  paint(ctx);
  return canvas.toDataURL("image/png");
}

const RED = pngOf((ctx) => {
  ctx.fillStyle = "rgb(255, 0, 0)";
  ctx.fillRect(0, 0, W, H);
});
const GREEN = pngOf((ctx) => {
  ctx.fillStyle = "rgb(0, 255, 0)";
  ctx.fillRect(0, 0, W, H);
});
const TRANSPARENT = pngOf(() => {});

describe("ReplayPlayer", () => {
  it("counts one step per stroke segment", () => {
    // A stroke's steps are (length - 14) / 2: the opening dot, recorded as a
    // duplicated point at slots 14/15, plus one per move. Matches the loop in
    // NEO's freeHandFast.
    expect(NeoReplay.stepsFor(items[0])).toBe(1);
    expect(NeoReplay.stepsFor(items[1])).toBe(4);
    expect(NeoReplay.stepsFor(items[2])).toBe(3);
    expect(NeoReplay.stepsFor(items[3])).toBe(2);

    const player = new ReplayPlayer(items, W, H, displayContext(), () => {});
    expect(player.total).toBe(10);
  });

  it("ends on exactly the image the bulk path produces", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(items, W, H, ctx, () => {});
    await player.skipToEnd();

    const reference = new NeoReplay(W, H);
    await reference.playAll(items);

    const stepped = pixels(ctx);
    const bulk = compositeOf(reference);
    expect(
      firstPixelDifference(stepped, bulk),
      describeDifference(stepped, bulk, W)
    ).toBe(-1);
  });

  it("seeking backwards then forwards lands on the same image", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(items, W, H, ctx, () => {});

    await player.seekTo(player.total);
    const atEnd = pixels(ctx);

    await player.seekTo(2);
    await player.seekTo(player.total);
    expect(
      firstPixelDifference(pixels(ctx), atEnd),
      describeDifference(pixels(ctx), atEnd, W)
    ).toBe(-1);
  });

  it("rewinds to a blank canvas", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(items, W, H, ctx, () => {});
    await player.seekTo(player.total);
    await player.rewind();

    const blank = displayContext();
    blank.fillStyle = "#ffffff";
    blank.fillRect(0, 0, W, H);

    expect(firstPixelDifference(pixels(ctx), pixels(blank))).toBe(-1);
    expect(player.getState().position).toBe(0);
  });

  it("reports progress and stops at the end", async () => {
    const states: number[] = [];
    const player = new ReplayPlayer(items, W, H, displayContext(), (s) =>
      states.push(s.position)
    );

    await player.seekTo(3);
    expect(player.getState().position).toBe(3);
    expect(player.getState().finished).toBe(false);

    await player.seekTo(999);
    expect(player.getState().position).toBe(player.total);
    expect(player.getState().finished).toBe(true);
    expect(states.length).toBeGreaterThan(0);
  });

  it("holds back the trailing restore so the strokes are what plays", () => {
    const withRestore = [...items, ["restore", "data:image/png;base64,A", "data:image/png;base64,B"]];
    const player = new ReplayPlayer(withRestore, W, H, displayContext(), () => {});
    expect(player.total).toBe(10);
  });

  it("labels its speeds honestly", () => {
    // The ×4 setting was once a batching branch rather than a faster rate, so
    // it ran an order of magnitude past its label. Playback is driven from
    // steps per second now, and the multipliers have to stay exact.
    const base = SPEEDS[DEFAULT_SPEED_INDEX].rate;
    expect(SPEEDS.map((s) => s.rate / base)).toEqual([0.5, 1, 2, 4]);
    expect(SPEEDS[DEFAULT_SPEED_INDEX].label).toBe("×1");
  });

  it("advances roughly in proportion to the rate", async () => {
    const run = async (rate: number) => {
      const player = new ReplayPlayer(items, W, H, displayContext(), () => {});
      player.setSpeed(rate);
      player.play();
      await new Promise((r) => setTimeout(r, 120));
      const reached = player.getState().position;
      player.dispose();
      return reached;
    };

    // Generous bounds: this is real browser timing, not a fake clock. The
    // point is that a faster setting is actually faster, which is what broke.
    const slow = await run(40);
    const fast = await run(400);
    expect(fast).toBeGreaterThan(slow);
  });

  it("plays to completion on its own", async () => {
    const ctx = displayContext();
    let finished = false;
    const player = new ReplayPlayer(items, W, H, ctx, (s) => {
      if (s.finished && !s.playing) finished = true;
    });
    player.setSpeed(100000); // effectively immediate
    player.play();

    const deadline = Date.now() + 5000;
    while (!finished && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 20));
    }
    expect(finished).toBe(true);
    expect(player.getState().position).toBe(player.total);
    player.dispose();
  });
});

describe("settling on the artwork", () => {
  const withRestore = () => [...items, ["restore", RED, TRANSPARENT]];

  it("applies the final image when playback reaches the end", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(withRestore(), W, H, ctx, () => {});

    await player.skipToEnd();
    const px = pixels(ctx);
    // The whole canvas is the restore image, not what the strokes drew
    expect([px[0], px[1], px[2], px[3]]).toEqual([255, 0, 0, 255]);
  });

  it("does not apply it before the end", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(withRestore(), W, H, ctx, () => {});

    await player.seekTo(2);
    const px = pixels(ctx);
    expect([px[0], px[1], px[2]]).not.toEqual([255, 0, 0]);
  });

  it("re-applies it after seeking back and forward", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(withRestore(), W, H, ctx, () => {});

    await player.skipToEnd();
    await player.seekTo(1);
    await player.skipToEnd();
    const px = pixels(ctx);
    expect([px[0], px[1], px[2]]).toEqual([255, 0, 0]);
  });

  it("settles on a file whose only restore is the trailing one", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(withRestore(), W, H, ctx, () => {});
    expect(player.total).toBe(10);
    await player.skipToEnd();
    expect(cornerOf(pixels(ctx))).toEqual([255, 0, 0, 255]);
  });

  it("keeps the strokes when the final image will not load", async () => {
    const ctx = displayContext();
    const broken = [...items, ["restore", "data:image/png;base64,NOT_A_PNG", TRANSPARENT]];
    const player = new ReplayPlayer(broken, W, H, ctx, () => {});

    await player.skipToEnd();

    // Falls back to what the strokes drew rather than blanking the canvas
    const reference = displayContext();
    const plain = new ReplayPlayer(items, W, H, reference, () => {});
    await plain.skipToEnd();
    expect(firstPixelDifference(pixels(ctx), pixels(reference))).toBe(-1);
  });
});

/**
 * A drawing continued from an earlier session opens with a restore holding the
 * canvas it was continued from -- a tenth of the archive does. That frame is
 * not a final image and has to play in place, the way NEO's play() runs it,
 * or the whole replay animates over a blank canvas.
 */
describe("a replay that opens on a restore", () => {
  // The flood fill is dropped: it would repaint the restored layer from (0, 0)
  // and hide whether the restore ever landed.
  const strokes = items.slice(1);
  const continued = () => [
    ["restore", GREEN, TRANSPARENT],
    ...strokes,
    ["restore", RED, TRANSPARENT],
  ];

  it("counts the opening restore as a step of its own", () => {
    const player = new ReplayPlayer(continued(), W, H, displayContext(), () => {});
    expect(player.total).toBe(10);
  });

  it("paints the canvas the artist started from, before the strokes", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(continued(), W, H, ctx, () => {});

    await player.seekTo(1);
    expect(cornerOf(pixels(ctx))).toEqual([0, 255, 0, 255]);
  });

  it("keeps it under the strokes for the rest of playback", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(continued(), W, H, ctx, () => {});

    await player.seekTo(player.total - 1);
    const px = pixels(ctx);
    expect(cornerOf(px)).toEqual([0, 255, 0, 255]);

    // and the strokes are on top of it, rather than the restore alone
    const flat = displayContext();
    flat.fillStyle = "rgb(0, 255, 0)";
    flat.fillRect(0, 0, W, H);
    expect(firstPixelDifference(px, pixels(flat))).not.toBe(-1);
  });

  it("re-applies it after seeking backwards", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(continued(), W, H, ctx, () => {});

    await player.skipToEnd();
    await player.seekTo(0);
    expect(cornerOf(pixels(ctx))).toEqual([255, 255, 255, 255]);

    await player.seekTo(2);
    expect(cornerOf(pixels(ctx))).toEqual([0, 255, 0, 255]);
  });

  it("still settles on the trailing restore at the end", async () => {
    const ctx = displayContext();
    const player = new ReplayPlayer(continued(), W, H, ctx, () => {});

    await player.skipToEnd();
    expect(cornerOf(pixels(ctx))).toEqual([255, 0, 0, 255]);
  });

  it("plays on when the opening image will not load", async () => {
    const ctx = displayContext();
    const broken = [
      ["restore", "data:image/png;base64,NOT_A_PNG", TRANSPARENT],
      ...strokes,
    ];
    const player = new ReplayPlayer(broken, W, H, ctx, () => {});

    await player.skipToEnd();
    expect(player.getState().position).toBe(player.total);

    // The strokes still drew; only the ground they sit on is missing
    const reference = displayContext();
    const plain = new ReplayPlayer(strokes, W, H, reference, () => {});
    await plain.skipToEnd();
    expect(
      firstPixelDifference(pixels(ctx), pixels(reference)),
      describeDifference(pixels(ctx), pixels(reference), W)
    ).toBe(-1);
  });
});
