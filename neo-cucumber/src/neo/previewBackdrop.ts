import type { DrawingEngine } from "../DrawingEngine";
import type { Backdrop } from "./xorOverlay";
import { bottomFirst } from "./canvasStack";

const NOBODY: ReadonlySet<string> = new Set();

/**
 * What was last read off each DOM canvas, and at which upload.
 *
 * The previews run from the pointermove handler, and in a session there is a
 * pair of canvases per participant to read: reading them all back on every
 * move was up to sixteen readbacks, each a full canvas, per sample of a drag.
 * A canvas changes only when the engine paints onto it, so its last reading
 * is good until the upload count moves on. Held per engine, and let go when
 * the drag ends, so no reading outlives the gesture that took it.
 */
const readings = new WeakMap<
  DrawingEngine,
  Map<string, { upload: number; data: Uint8ClampedArray }>
>();

/** Drops the cached readings; call when a preview ends. */
export function releaseBackdrop(engine: DrawingEngine): void {
  readings.delete(engine);
}

/**
 * NEO draws XOR cursors into its destination canvas after compositing the
 * visible layers. Sample our mounted layer canvases for the same result: they
 * are the pixels the user currently sees, including the brief interval while
 * engine updates are being batched. Before the DOM canvases mount, the engine
 * buffers are an equivalent fallback.
 *
 * Every participant's pair, not only our own: what is on screen in a session
 * is the whole stack, and an outline computed against just our layers came
 * out wrong wherever somebody above us had drawn.
 */
export function previewBackdrop(
  engine: DrawingEngine,
  width: number,
  height: number,
  scale: number,
  bgVisible: boolean,
  fgVisible: boolean,
  hiddenOwners: ReadonlySet<string> = NOBODY
): Backdrop {
  let held = readings.get(engine);
  if (!held) {
    held = new Map();
    readings.set(engine, held);
  }
  const pixels = (owner: string, layer: "background" | "foreground") => {
    const context = engine.domContextFor(layer, owner);
    if (!context) return engine.layersFor(owner)[layer];
    const key = `${owner} ${layer}`;
    const upload = engine.domUploadCount(layer, owner);
    const reading = held.get(key);
    if (reading && reading.upload === upload) return reading.data;
    // `getImageData` hands back a buffer nobody else holds, and this only
    // ever reads it, so it is kept as it is.
    const data = context.getImageData(0, 0, width, height).data;
    held.set(key, { upload, data });
    return data;
  };

  const layers: Uint8ClampedArray[] = [];
  for (const owner of bottomFirst(engine.ownerIds())) {
    if (hiddenOwners.has(owner)) continue;
    if (bgVisible) layers.push(pixels(owner, "background"));
    if (fgVisible) layers.push(pixels(owner, "foreground"));
  }
  return { width, height, scale, layers };
}
