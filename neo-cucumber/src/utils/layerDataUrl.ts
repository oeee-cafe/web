/**
 * A layer as the `data:` URL a `.pch` restore frame carries, and back.
 *
 * NEO writes a restore with `canvas.toDataURL`, which goes through a canvas
 * and so premultiplies: a faint pixel of a translucent stroke comes out a few
 * levels off in colour. A drawing resumed from such a frame would carry on
 * from pixels the artist never had. These are written from the layer buffer
 * by `pngCodec` instead, which every PNG reader -- NEO's `Image`, a browser's
 * viewer -- reads as the same picture, and which `pngCodec` reads back exactly.
 */

import { encodeLayerPng } from "./pngCodec";
import { pngDataToLayer } from "./canvasSnapshot";

const PREFIX = "data:image/png;base64,";

/** A layer as a lossless PNG `data:` URL. */
export function layerToPngDataUrl(
  layer: Uint8ClampedArray,
  width: number,
  height: number,
): string {
  const png = encodeLayerPng(layer, width, height);
  // In slices: spreading a whole layer's bytes into one call overflows the
  // argument limit.
  let binary = "";
  for (let at = 0; at < png.length; at += 0x8000) {
    binary += String.fromCharCode.apply(
      null,
      Array.prototype.slice.call(png, at, at + 0x8000) as number[],
    );
  }
  return PREFIX + btoa(binary);
}

/**
 * A restore frame's `data:` URL as a layer: exactly, for one `pngCodec` can
 * read -- every one this painter wrote -- and through a canvas otherwise, as
 * NEO would have drawn it.
 */
export async function layerFromPngDataUrl(
  url: string,
  width: number,
  height: number,
): Promise<Uint8ClampedArray> {
  if (!url.startsWith(PREFIX)) throw new Error("Restore frame is not a PNG data URL");
  const binary = atob(url.slice(PREFIX.length));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return pngDataToLayer(bytes, width, height);
}
