/**
 * Turning a layer into a PNG and back.
 *
 * This is what a checkpoint is made of, and a checkpoint covers a layer pair
 * per participant -- sixteen of these at the largest session size, in one go,
 * on whoever the room asked. They are encoded and decoded by `pngCodec`
 * rather than by the browser through a canvas, which changed every
 * translucent pixel it carried.
 */

import { decodeLayerPng, encodeLayerPng } from "./pngCodec";

/**
 * A layer as a PNG Blob, losslessly; see `pngCodec`.
 */
export async function layerToPngBlob(
  layer: Uint8ClampedArray,
  width: number,
  height: number
): Promise<Blob> {
  // A layer takes tens of milliseconds, and a checkpoint asks for up to
  // sixteen at once. A turn of the event loop before each lets the pointer
  // in between them rather than after all of them.
  await new Promise((resolve) => setTimeout(resolve, 0));
  const png = encodeLayerPng(layer, width, height);
  return new Blob([png as Uint8Array<ArrayBuffer>], { type: "image/png" });
}

/**
 * PNG data back to a layer, exactly as stored when `pngCodec` can read it --
 * which is every checkpoint a client has written -- and through a canvas,
 * premultiplication and all, only when it cannot.
 */
export async function pngDataToLayer(
  pngData: Uint8Array,
  width: number,
  height: number
): Promise<Uint8ClampedArray> {
  return decodeLayerPng(pngData, width, height) ?? decodeThroughCanvas(pngData, width, height);
}

/** The browser's decoder, for a PNG `decodeLayerPng` declines. */
async function decodeThroughCanvas(
  pngData: Uint8Array,
  width: number,
  height: number
): Promise<Uint8ClampedArray> {
  const blob = new Blob([pngData as unknown as BlobPart], { type: "image/png" });
  const url = URL.createObjectURL(blob);
  try {
    const image = await new Promise<HTMLImageElement>((resolve, reject) => {
      const img = new Image();
      img.onload = () => resolve(img);
      img.onerror = () => reject(new Error("Failed to load image from PNG data"));
      img.src = url;
    });

    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) {
      throw new Error("Failed to get canvas context");
    }

    // Disable image smoothing to preserve pixel-perfect data
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(image, 0, 0);
    return ctx.getImageData(0, 0, width, height).data;
  } finally {
    URL.revokeObjectURL(url);
  }
}
