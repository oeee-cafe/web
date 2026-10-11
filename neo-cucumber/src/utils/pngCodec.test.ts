import { describe, expect, it } from "vitest";
import { unzlibSync } from "fflate";
import { decodeLayerPng, encodeLayerPng } from "./pngCodec";

/** Every alpha from 0 to 255, each under a spread of colours. */
function translucentLayer(width: number, height: number): Uint8ClampedArray {
  const layer = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      layer[i] = (y * 37 + x) & 255;
      layer[i + 1] = (y * 91 + 13) & 255;
      layer[i + 2] = (x * 151 + 7) & 255;
      layer[i + 3] = x & 255;
    }
  }
  return layer;
}

describe("a layer as PNG", () => {
  /**
   * The point of the module. Through a canvas, three in four translucent
   * pixels came back changed, by up to 126 of 255.
   */
  it("comes back exactly, translucent pixels included", () => {
    const layer = translucentLayer(256, 40);
    const back = decodeLayerPng(encodeLayerPng(layer, 256, 40), 256, 40);
    expect(back).not.toBeNull();
    expect(Array.from(back!)).toEqual(Array.from(layer));
  });

  it("is a PNG anything can read: signature, 8-bit RGBA, checksummed chunks", () => {
    const png = encodeLayerPng(translucentLayer(9, 5), 9, 5);
    expect(Array.from(png.subarray(0, 8))).toEqual([137, 80, 78, 71, 13, 10, 26, 10]);
    const view = new DataView(png.buffer, png.byteOffset);
    expect(String.fromCharCode(...png.subarray(12, 16))).toBe("IHDR");
    expect([view.getUint32(16), view.getUint32(20), png[24], png[25], png[28]]).toEqual([9, 5, 8, 6, 0]);
    // Each scanline carries one of the five filters.
    const idatLength = view.getUint32(33);
    const raw = unzlibSync(png.subarray(41, 41 + idatLength));
    for (let y = 0; y < 5; y++) expect(raw[y * (9 * 4 + 1)]).toBeLessThanOrEqual(4);
  });

  it("is the same bytes for the same layer, wherever it is encoded", () => {
    const layer = translucentLayer(64, 16);
    expect(Array.from(encodeLayerPng(layer, 64, 16))).toEqual(Array.from(encodeLayerPng(layer, 64, 16)));
  });

  it("puts an image of another size in the corner, cropped, as a canvas did", () => {
    const small = translucentLayer(4, 3);
    const back = decodeLayerPng(encodeLayerPng(small, 4, 3), 6, 2)!;
    expect(Array.from(back.subarray(0, 16))).toEqual(Array.from(small.subarray(0, 16)));
    expect(Array.from(back.subarray(16, 24))).toEqual(new Array(8).fill(0));
  });

  it("declines what it does not read, for the caller to decode another way", () => {
    expect(decodeLayerPng(new Uint8Array([1, 2, 3]), 1, 1)).toBeNull();
    const png = encodeLayerPng(translucentLayer(2, 2), 2, 2);
    const interlaced = png.slice();
    interlaced[28] = 1;
    expect(decodeLayerPng(interlaced, 2, 2)).toBeNull();
  });
});
