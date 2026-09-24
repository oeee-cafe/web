/**
 * Shrinks a participant's layers into a thumbnail that averages what it covers.
 *
 * One `drawImage` from a 1024-wide layer into a 24-wide thumbnail does not
 * average: bilinear filtering reads the four source pixels nearest each
 * destination pixel and ignores the rest, so at that ratio a thumbnail pixel
 * stands for about sixteen of the seventeen hundred it covers. Thin strokes
 * came out at double strength or not at all depending on which pixels they
 * crossed -- and since that changes as the drawing does, the thumbnail
 * flickered between wrong colours.
 *
 * Halving does average: at exactly half size every destination pixel centre
 * falls between four source pixels, so bilinear filtering is a 2x2 box. So
 * this halves until one more halving would pass the target, then takes the
 * last, smaller step -- which is off by less than a factor of two and reads
 * every pixel it stands for. `imageSmoothingQuality` would do the same in one
 * call where it exists, and it does not in Firefox 56.
 *
 * Composited over white, because that is the paper under every layer on the
 * canvas; showing it over the panel's colour tinted every soft edge.
 */

/** Scratch canvases, one per size, reused across thumbnails and refreshes. */
const scratch = new Map<string, HTMLCanvasElement>();

function scratchCanvas(width: number, height: number): CanvasRenderingContext2D | null {
  const key = `${width}x${height}`;
  let canvas = scratch.get(key);
  if (!canvas) {
    canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    scratch.set(key, canvas);
  }
  return canvas.getContext("2d");
}

export function paintThumbnail(
  target: HTMLCanvasElement,
  sources: readonly HTMLCanvasElement[],
): void {
  const context = target.getContext("2d");
  if (!context) return;
  const { width: targetWidth, height: targetHeight } = target;
  context.imageSmoothingEnabled = true;
  context.fillStyle = "#ffffff";
  context.fillRect(0, 0, targetWidth, targetHeight);
  if (sources.length === 0) return;

  let width = sources[0].width;
  let height = sources[0].height;
  // The layers go down together at the first halving, so the rest of the
  // steps move one image rather than one per layer. Each is averaged on its
  // own before they are stacked, which differs from stacking first only
  // where a translucent stroke overlaps another inside one 2x2 block.
  let current: CanvasImageSource | null = null;
  while (width / 2 >= targetWidth && height / 2 >= targetHeight) {
    const nextWidth = Math.max(1, Math.round(width / 2));
    const nextHeight = Math.max(1, Math.round(height / 2));
    const step = scratchCanvas(nextWidth, nextHeight);
    if (!step) break;
    step.imageSmoothingEnabled = true;
    step.clearRect(0, 0, nextWidth, nextHeight);
    for (const source of current ? [current] : sources) {
      step.drawImage(source, 0, 0, width, height, 0, 0, nextWidth, nextHeight);
    }
    current = step.canvas;
    width = nextWidth;
    height = nextHeight;
  }
  for (const source of current ? [current] : sources) {
    context.drawImage(source, 0, 0, width, height, 0, 0, targetWidth, targetHeight);
  }
}
