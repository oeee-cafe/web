/**
 * The slice of a canvas context NEO's rasterisation actually uses.
 *
 * Every kernel reads a padded sub-rectangle, edits it, and writes it back, so
 * a surface only has to offer those two operations. `CanvasRenderingContext2D`
 * satisfies this structurally, which lets the same verified code render either
 * onto a canvas (the replay viewer) or straight into a layer buffer (the
 * painter) without a second implementation to keep in step.
 */
export interface PixelSurface {
  getImageData(x: number, y: number, width: number, height: number): ImageData;
  putImageData(data: ImageData, x: number, y: number): void;
}

/**
 * A PixelSurface over a plain RGBA buffer.
 *
 * Reproduces the canvas semantics the kernels lean on: reading outside the
 * bounds yields transparent pixels rather than throwing or wrapping, and
 * writing outside them is clipped. The returned sub-rectangle keeps the width
 * it was asked for, which matters -- blur walks its neighbours with
 * `index ± width * 4`, so a surface that quietly returned a narrower row would
 * blur against the wrong pixels.
 *
 * One thing it deliberately does *not* reproduce: a canvas stores pixels
 * premultiplied, so writing a translucent colour and reading it back loses a
 * little of it -- up to 12 of 255 at low alpha, converging to nothing as
 * strokes overlap and alpha climbs. A buffer keeps what it was given, and the
 * painter keeps that precision rather than throwing it away to match an
 * artefact. Playback therefore renders strokes very slightly differently from
 * the painter; the player applies the replay's restore frame when it reaches
 * the end so it still finishes on exactly the artwork.
 *
 * The one part of that it does reproduce is the part every browser agrees
 * on: under zero alpha a canvas has no colour to keep, so a pixel written
 * transparent reads back as 0x00000000. NEO's eraser only lowers alpha and
 * leaves the stroke's colour behind, and its flood fill compares whole
 * 32-bit pixels -- so on a canvas, ground the eraser cleared is the same
 * ground as ground never drawn on, and a buffer that kept the red under it
 * stopped the fill at an invisible wall the replay then ran straight
 * through.
 */
export class BufferSurface implements PixelSurface {
  private readonly buffer: Uint8ClampedArray;
  private readonly width: number;
  private readonly height: number;
  private readonly onWrite?: (x0: number, y0: number, x1: number, y1: number) => void;

  /**
   * `onWrite` reports the clipped, inclusive bounds of every write. Every
   * kernel ends by putting its padded sub-rectangle back, so a surface that
   * reports what it stored knows exactly which pixels changed -- which is what
   * lets a repaint upload a stroke's few hundred pixels instead of the canvas.
   * Deriving the same extent a second time from the operation's arguments
   * would be a copy of the tool table free to drift from it.
   */
  constructor(
    buffer: Uint8ClampedArray,
    width: number,
    height: number,
    onWrite?: (x0: number, y0: number, x1: number, y1: number) => void,
  ) {
    this.buffer = buffer;
    this.width = width;
    this.height = height;
    this.onWrite = onWrite;
  }

  getImageData(x: number, y: number, width: number, height: number): ImageData {
    const out = new ImageData(Math.max(1, width), Math.max(1, height));

    const startX = Math.max(0, -x);
    const startY = Math.max(0, -y);
    const endX = Math.min(width, this.width - x);
    const endY = Math.min(height, this.height - y);

    // Row at a time: a per-pixel loop here costs more than the kernels do.
    const span = (endX - startX) * 4;
    if (span > 0) {
      for (let row = startY; row < endY; row++) {
        const src = ((y + row) * this.width + x + startX) * 4;
        const dst = (row * width + startX) * 4;
        out.data.set(this.buffer.subarray(src, src + span), dst);
      }
    }
    return out;
  }

  putImageData(data: ImageData, x: number, y: number): void {
    const startX = Math.max(0, -x);
    const startY = Math.max(0, -y);
    const endX = Math.min(data.width, this.width - x);
    const endY = Math.min(data.height, this.height - y);

    const span = (endX - startX) * 4;
    if (span <= 0 || endY <= startY) return;
    const buffer = this.buffer;
    for (let row = startY; row < endY; row++) {
      const src = (row * data.width + startX) * 4;
      const dst = ((y + row) * this.width + x + startX) * 4;
      buffer.set(data.data.subarray(src, src + span), dst);
      for (let i = dst; i < dst + span; i += 4) {
        if (buffer[i + 3] === 0) {
          buffer[i] = 0;
          buffer[i + 1] = 0;
          buffer[i + 2] = 0;
        }
      }
    }
    this.onWrite?.(x + startX, y + startY, x + endX - 1, y + endY - 1);
  }
}
