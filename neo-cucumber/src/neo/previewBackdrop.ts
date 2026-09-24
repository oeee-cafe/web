import type { DrawingEngine } from "../DrawingEngine";
import type { Backdrop } from "./xorOverlay";
import { inJoinOrder } from "./canvasStack";

/**
 * NEO draws XOR cursors into its destination canvas after compositing the
 * visible layers. Sample our mounted layer canvases for the same result: they
 * are the pixels the user currently sees, including the brief interval while
 * engine updates are being batched. Before the DOM canvases mount, the engine
 * buffers are an equivalent fallback.
 *
 * Every participant's pair, not only our own: what is on screen in a session
 * is the whole stack, and an outline computed against just our layers came
 * out wrong wherever somebody above us had drawn. Bottom first, which is the
 * *latest* joiner -- see `participantZIndex`.
 */
export function previewBackdrop(
  engine: DrawingEngine,
  width: number,
  height: number,
  scale: number,
  bgVisible: boolean,
  fgVisible: boolean,
  hiddenOwners: ReadonlySet<string> = new Set()
): Backdrop {
  const pixels = (owner: string, layer: "background" | "foreground") => {
    const context = engine.domContextFor(layer, owner);
    if (context) {
      // `getImageData` hands back a buffer nobody else holds, and this only
      // ever reads it, so copying it again was a second full-canvas
      // allocation per pointer move of every region, line and bezier drag.
      return context.getImageData(0, 0, width, height).data;
    }
    return engine.layersFor(owner)[layer];
  };

  const layers: Uint8ClampedArray[] = [];
  for (const owner of inJoinOrder(engine.ownerIds()).reverse()) {
    if (hiddenOwners.has(owner)) continue;
    if (bgVisible) layers.push(pixels(owner, "background"));
    if (fgVisible) layers.push(pixels(owner, "foreground"));
  }
  return { width, height, scale, layers };
}
