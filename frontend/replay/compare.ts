/**
 * Whether two pictures are the same, pixel for pixel, and where they are not.
 *
 * The replay check's arithmetic, apart from the canvases it reads: the
 * recording played to its end on one side, the post the session was saved as
 * on the other. Both come from the same `exportPng` and the same PNG decoder,
 * so a picture that is the same is the same exactly -- there is no tolerance,
 * because the claim being checked is that a replay renders what was drawn,
 * and "nearly" is the failure.
 */

/** The shape of `ImageData`, which Node does not have. */
export type Pixels = { width: number; height: number; data: Uint8ClampedArray };

export type Comparison = {
  sameSize: boolean;
  /** Pixels that differ in any channel. */
  differing: number;
  total: number;
  /** Where they differ, as RGBA: opaque red where they do, clear elsewhere.
   * Empty when the sizes differ, since there is no pixel to pair. */
  mask: Uint8ClampedArray<ArrayBuffer>;
};

export function compare(a: Pixels, b: Pixels): Comparison {
  if (a.width !== b.width || a.height !== b.height) {
    return {
      sameSize: false,
      differing: Math.max(a.width * a.height, b.width * b.height),
      total: Math.max(a.width * a.height, b.width * b.height),
      mask: new Uint8ClampedArray(0),
    };
  }
  const total = a.width * a.height;
  const mask = new Uint8ClampedArray(total * 4);
  let differing = 0;
  for (let pixel = 0; pixel < total; pixel++) {
    const at = pixel * 4;
    // Nothing there on either side is the same nothing, whatever colour a
    // decoder left in the channels of a pixel with no alpha.
    if (a.data[at + 3] === 0 && b.data[at + 3] === 0) continue;
    if (
      a.data[at] !== b.data[at] ||
      a.data[at + 1] !== b.data[at + 1] ||
      a.data[at + 2] !== b.data[at + 2] ||
      a.data[at + 3] !== b.data[at + 3]
    ) {
      differing++;
      mask[at] = 255;
      mask[at + 3] = 255;
    }
  }
  return { sameSize: true, differing, total, mask };
}
