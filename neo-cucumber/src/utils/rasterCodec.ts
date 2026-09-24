/**
 * A fill's coverage for the wire, compressed with what the browser already has.
 *
 * DEFLATE, which measured six times smaller than PNG on the rasters a flood
 * actually makes -- PNG filters each scanline and browser encoders tune for
 * speed, where DEFLATE's window eats the long repeats a filled region is made
 * of. A mask of set bits is more of the same, only more so.
 *
 * Not zstd, which would be smaller again by perhaps a fifth: `CompressionStream`
 * offers gzip and deflate and nothing else, so zstd means carrying a compressor
 * into every page load to save a few hundred bytes on a message that already
 * fits in two kilobytes.
 */

import { unzlibSync, zlibSync } from "fflate";

/*
 * Synchronous, through fflate, rather than the browser's `CompressionStream`.
 * The streams are asynchronous, and a fill's operation could only go out once
 * they had finished: for those few milliseconds the fill was on the canvas
 * and in no fork, so anything emitted meanwhile -- the next stroke's
 * boundary, an undo -- was sequenced ahead of it, and a checkpoint asked for
 * in that window was exported settled with the fill's pixels already in it.
 * The bytes are the same zlib-wrapped DEFLATE the streams produced, so
 * history written either way reads either way.
 */

/** Compresses a coverage mask for transport. */
export function deflateCoverage(coverage: Uint8Array): Uint8Array {
  return zlibSync(coverage);
}

/**
 * Restores raw RGBA, refusing anything that is not the size it claims -- a
 * short buffer would otherwise be blitted as a band of transparent pixels
 * across somebody's drawing.
 */
export function inflateCoverage(
  compressed: Uint8Array,
  width: number,
  height: number,
): Uint8Array {
  const bytes = unzlibSync(compressed);
  const expected = Math.ceil((width * height) / 8);
  if (bytes.length !== expected) {
    throw new Error(
      `Coverage is ${bytes.length} bytes; ${width}x${height} needs ${expected}`,
    );
  }
  return bytes;
}
