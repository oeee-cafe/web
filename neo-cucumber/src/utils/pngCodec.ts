/**
 * PNG for a layer buffer, written and read without a canvas.
 *
 * A checkpoint is every participant's layers as PNG: one client encodes them,
 * every client that joins from the checkpoint decodes them, and the clients
 * that were in the room keep the buffers they had. Both ends used to go
 * through a 2D canvas -- `putImageData` and `toBlob` out, `drawImage` and
 * `getImageData` back -- and a canvas stores colour premultiplied by alpha, so
 * a translucent pixel came back changed: three in four of them, by as much as
 * 126 of 255 at low alpha. Opaque pixels survived, which is why nobody saw it,
 * but every joiner started from a canvas the room did not have, and every
 * stroke laid over a translucent one blended into something else.
 *
 * Here the bytes are the buffer's own, straight alpha in and straight alpha
 * out, and the same in every browser. DEFLATE is fflate's, already carried
 * for fills (`rasterCodec`).
 */

import { unzlibSync, zlibSync } from "fflate";

const SIGNATURE = [137, 80, 78, 71, 13, 10, 26, 10];

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes: Uint8Array, start: number, end: number): number {
  let c = 0xffffffff;
  for (let i = start; i < end; i++) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function paeth(a: number, b: number, c: number): number {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) return a;
  return pb <= pc ? b : c;
}

/**
 * Encodes an RGBA layer as an 8-bit RGBA PNG, losslessly.
 *
 * Each scanline gets whichever of the five filters leaves the smallest sum of
 * absolute differences -- libpng's own heuristic -- because a layer is mostly
 * transparent runs and flat colour, which is what the filters are for.
 */
export function encodeLayerPng(
  layer: Uint8ClampedArray,
  width: number,
  height: number,
): Uint8Array {
  const stride = width * 4;
  const filtered = new Uint8Array((stride + 1) * height);
  const candidates = [0, 1, 2, 3, 4].map(() => new Uint8Array(stride));
  for (let y = 0; y < height; y++) {
    const row = y * stride;
    const up = row - stride;
    // A layer is mostly transparent, and a row of zeros costs nothing under
    // the first filter, which no other can beat: `filtered` is already zero
    // there, filter byte included, so the four other passes are skipped.
    let blank = true;
    for (let i = 0; i < stride; i++) {
      if (layer[row + i] !== 0) {
        blank = false;
        break;
      }
    }
    if (blank) continue;
    let best = 0;
    let bestCost = Infinity;
    for (let type = 0; type < 5; type++) {
      const out = candidates[type];
      let cost = 0;
      for (let i = 0; i < stride; i++) {
        const x = layer[row + i];
        const a = i >= 4 ? layer[row + i - 4] : 0;
        const b = y > 0 ? layer[up + i] : 0;
        const c = i >= 4 && y > 0 ? layer[up + i - 4] : 0;
        const predicted =
          type === 0 ? 0 : type === 1 ? a : type === 2 ? b : type === 3 ? (a + b) >> 1 : paeth(a, b, c);
        const value = (x - predicted) & 0xff;
        out[i] = value;
        cost += value < 128 ? value : 256 - value;
      }
      if (cost < bestCost) {
        bestCost = cost;
        best = type;
      }
    }
    const at = y * (stride + 1);
    filtered[at] = best;
    filtered.set(candidates[best], at + 1);
  }
  const idat = zlibSync(filtered);

  const header = new Uint8Array(13);
  const view = new DataView(header.buffer);
  view.setUint32(0, width);
  view.setUint32(4, height);
  header.set([8, 6, 0, 0, 0], 8); // 8 bits, RGBA, deflate, adaptive filtering, no interlace

  const chunks: [string, Uint8Array][] = [["IHDR", header], ["IDAT", idat], ["IEND", new Uint8Array(0)]];
  const size = SIGNATURE.length + chunks.reduce((sum, [, data]) => sum + 12 + data.length, 0);
  const png = new Uint8Array(size);
  const out = new DataView(png.buffer);
  png.set(SIGNATURE, 0);
  let at = SIGNATURE.length;
  for (const [type, data] of chunks) {
    out.setUint32(at, data.length);
    for (let i = 0; i < 4; i++) png[at + 4 + i] = type.charCodeAt(i);
    png.set(data, at + 8);
    out.setUint32(at + 8 + data.length, crc32(png, at + 4, at + 8 + data.length));
    at += 12 + data.length;
  }
  return png;
}

/**
 * Decodes a PNG into a `width` by `height` RGBA layer, exactly as stored.
 *
 * Reads what browsers and this module write -- 8-bit greyscale, RGB, palette,
 * greyscale with alpha and RGBA, not interlaced -- which covers every
 * checkpoint a client has uploaded. Anything else is null, for the caller to
 * decode some other way. An image of another size lands in the top-left
 * corner and is cropped, as drawing it onto a canvas of the layer's size did.
 */
export function decodeLayerPng(
  png: Uint8Array,
  width: number,
  height: number,
): Uint8ClampedArray | null {
  if (png.length < 8 || SIGNATURE.some((byte, i) => png[i] !== byte)) return null;
  const view = new DataView(png.buffer, png.byteOffset, png.byteLength);
  let imageWidth = 0;
  let imageHeight = 0;
  let colorType = -1;
  let palette: Uint8Array | null = null;
  let transparency: Uint8Array | null = null;
  const idat: Uint8Array[] = [];
  for (let at = 8; at + 8 <= png.length; ) {
    const length = view.getUint32(at);
    const type = String.fromCharCode(png[at + 4], png[at + 5], png[at + 6], png[at + 7]);
    const data = png.subarray(at + 8, at + 8 + length);
    if (type === "IHDR") {
      imageWidth = view.getUint32(at + 8);
      imageHeight = view.getUint32(at + 12);
      const depth = data[8];
      colorType = data[9];
      const interlace = data[12];
      if (depth !== 8 || interlace !== 0 || ![0, 2, 3, 4, 6].includes(colorType)) return null;
    } else if (type === "PLTE") {
      palette = data;
    } else if (type === "tRNS") {
      transparency = data;
    } else if (type === "IDAT") {
      idat.push(data);
    } else if (type === "IEND") {
      break;
    }
    at += 12 + length;
  }
  if (colorType < 0 || idat.length === 0 || (colorType === 3 && !palette)) return null;

  const channels = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 }[colorType]!;
  const stride = imageWidth * channels;
  let compressed: Uint8Array;
  if (idat.length === 1) {
    compressed = idat[0];
  } else {
    compressed = new Uint8Array(idat.reduce((sum, part) => sum + part.length, 0));
    let at = 0;
    for (const part of idat) {
      compressed.set(part, at);
      at += part.length;
    }
  }
  let raw: Uint8Array;
  try {
    raw = unzlibSync(compressed);
  } catch {
    return null;
  }
  if (raw.length < (stride + 1) * imageHeight) return null;

  // Unfilter in place, a scanline at a time, against the one above.
  const rows = new Uint8Array(stride * imageHeight);
  for (let y = 0; y < imageHeight; y++) {
    const filter = raw[y * (stride + 1)];
    const source = y * (stride + 1) + 1;
    const row = y * stride;
    for (let i = 0; i < stride; i++) {
      const a = i >= channels ? rows[row + i - channels] : 0;
      const b = y > 0 ? rows[row - stride + i] : 0;
      const c = i >= channels && y > 0 ? rows[row - stride + i - channels] : 0;
      const predicted =
        filter === 0 ? 0 : filter === 1 ? a : filter === 2 ? b : filter === 3 ? (a + b) >> 1 : filter === 4 ? paeth(a, b, c) : -1;
      if (predicted < 0) return null;
      rows[row + i] = (raw[source + i] + predicted) & 0xff;
    }
  }

  const layer = new Uint8ClampedArray(width * height * 4);
  const keyed = transparency && (colorType === 0 || colorType === 2)
    ? colorType === 0
      ? [transparency[1]]
      : [transparency[1], transparency[3], transparency[5]]
    : null;
  for (let y = 0; y < Math.min(height, imageHeight); y++) {
    for (let x = 0; x < Math.min(width, imageWidth); x++) {
      const from = y * stride + x * channels;
      const to = (y * width + x) * 4;
      let r: number, g: number, b: number, alpha: number;
      switch (colorType) {
        case 0:
          r = g = b = rows[from];
          alpha = keyed && rows[from] === keyed[0] ? 0 : 255;
          break;
        case 2:
          r = rows[from]; g = rows[from + 1]; b = rows[from + 2];
          alpha = keyed && r === keyed[0] && g === keyed[1] && b === keyed[2] ? 0 : 255;
          break;
        case 3: {
          const index = rows[from];
          r = palette![index * 3]; g = palette![index * 3 + 1]; b = palette![index * 3 + 2];
          alpha = transparency && index < transparency.length ? transparency[index] : 255;
          break;
        }
        case 4:
          r = g = b = rows[from];
          alpha = rows[from + 1];
          break;
        default:
          r = rows[from]; g = rows[from + 1]; b = rows[from + 2];
          alpha = rows[from + 3];
      }
      layer[to] = r;
      layer[to + 1] = g;
      layer[to + 2] = b;
      layer[to + 3] = alpha;
    }
  }
  return layer;
}
