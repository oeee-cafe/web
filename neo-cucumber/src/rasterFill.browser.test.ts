import { describe, expect, it } from "vitest";
import { DrawingEngine } from "./DrawingEngine";
import { CanvasHistory } from "./utils/canvasHistory";
import { deflateCoverage, inflateCoverage } from "./utils/rasterCodec";

describe("a flood fill sent as the ground it covered", () => {
  const boxed = () => {
    const engine = new DrawingEngine(64, 48);
    engine.setLocalOwner("1");
    const layer = engine.layersFor("1").background;
    // A box to flood inside, so the fill cannot reach the whole layer.
    for (let y = 10; y <= 29; y++) {
      for (const x of [10, 29]) layer[(y * 64 + x) * 4 + 3] = 255;
    }
    for (let x = 10; x <= 29; x++) {
      for (const y of [10, 29]) layer[(y * 64 + x) * 4 + 3] = 255;
    }
    return { engine, layer };
  };

  it("crops to the region it actually filled", () => {
    const { engine, layer } = boxed();
    const region = engine.floodFillCapturingRegion(layer, 20, 20, 200, 100, 50, 255);
    expect(region).not.toBeNull();
    expect(region!.width).toBeLessThan(64);
    expect(region!.height).toBeLessThan(48);
    expect(region!.x).toBeGreaterThanOrEqual(10);
    expect(region!.y).toBeGreaterThanOrEqual(10);
  });

  it("round-trips its coverage through the wire codec", async () => {
    const { engine, layer } = boxed();
    const region = engine.floodFillCapturingRegion(layer, 20, 20, 200, 100, 50, 255)!;
    const compressed = await deflateCoverage(region.coverage);
    // A contiguous shape is exactly what a flood makes, and it should be tiny.
    expect(compressed.length).toBeLessThan(region.coverage.length);
    const restored = await inflateCoverage(compressed, region.width, region.height);
    expect(Array.from(restored)).toEqual(Array.from(region.coverage));
  });

  it("refuses coverage that is not the size it claims", () => {
    const compressed = deflateCoverage(new Uint8Array(2));
    expect(() => inflateCoverage(compressed, 64, 64)).toThrow(/needs/);
  });

  /**
   * History written before this codec was synchronous was compressed by the
   * browser's `CompressionStream("deflate")`, and clients still running that
   * build read ours with its `DecompressionStream`. Both directions, against
   * the browser's own implementation.
   */
  it("reads and writes the same bytes as the browser's deflate streams", async () => {
    const { engine, layer } = boxed();
    const region = engine.floodFillCapturingRegion(layer, 20, 20, 200, 100, 50, 255)!;
    const through = async (bytes: Uint8Array, stream: TransformStream) =>
      new Uint8Array(
        await new Response(
          new Blob([bytes.slice().buffer as ArrayBuffer]).stream().pipeThrough(stream),
        ).arrayBuffer(),
      );

    const theirs = await through(region.coverage, new CompressionStream("deflate"));
    expect(Array.from(inflateCoverage(theirs, region.width, region.height)))
      .toEqual(Array.from(region.coverage));

    const ours = deflateCoverage(region.coverage);
    expect(Array.from(await through(ours, new DecompressionStream("deflate"))))
      .toEqual(Array.from(region.coverage));
  });

  /**
   * The reason it is coverage rather than a picture of the box.
   *
   * A picture carries whatever else lies inside the rectangle. Stamping that
   * back down on a replay puts other people's work back after they have undone
   * it -- the fill reaching across somebody's stroke is enough to preserve it
   * forever.
   */
  it("leaves work inside its box that somebody has since undone", async () => {
    const engine = new DrawingEngine(32, 24);
    engine.setLocalOwner("1");
    const history = new CanvasHistory(engine);
    history.setLocalUserId("1");
    const layer = engine.layersFor("1").background;
    const alphaAt = (x: number, y: number) => layer[(y * 32 + x) * 4 + 3];

    await history.handleCanonicalOperation({
      id: "b:1", actorId: "2", sequence: 1, operation: { kind: "undo-boundary" },
    });
    await history.handleCanonicalOperation({
      id: "b:2", actorId: "2", sequence: 2,
      operation: {
        kind: "stroke", targetActorId: "1", layer: "background",
        brushSize: 1, brush: "solid", color: { r: 0, g: 255, b: 0, a: 255 },
        points: [{ x: 10, y: 10 }], mask: { type: 0, r: 0, g: 0, b: 0 },
      },
    });
    expect(alphaAt(10, 10)).toBe(255);

    // Our fill, whose box covers that stroke without the flood reaching it.
    const region = engine.floodFillCapturingRegion(layer, 20, 12, 200, 100, 50, 255)!;
    await history.handleCanonicalOperation({
      id: "a:1", actorId: "1", sequence: 3, operation: { kind: "undo-boundary" },
    });
    await history.handleCanonicalOperation({
      id: "a:2", actorId: "1", sequence: 4,
      operation: {
        kind: "fill-region", targetActorId: "1", layer: "background",
        at: { x: region.x, y: region.y },
        width: region.width, height: region.height,
        color: { r: 200, g: 100, b: 50, a: 255 },
        coverage: await deflateCoverage(region.coverage),
        mask: { type: 0, r: 0, g: 0, b: 0 },
      },
    });

    // They undo their stroke. It goes, and stays gone.
    await history.handleCanonicalOperation({
      id: "b:3", actorId: "2", sequence: 5, operation: { kind: "undo", redo: false },
    });
    expect(alphaAt(10, 10)).toBe(0);
  });
});
