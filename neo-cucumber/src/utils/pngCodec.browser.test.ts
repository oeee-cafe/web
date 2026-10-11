import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { mount, type PainterHandle } from "../public";
import { decodeLayerPng, encodeLayerPng } from "./pngCodec";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/*
 * Through the browser, where the trouble was: a checkpoint used to be encoded
 * and decoded through a canvas, which stores colour premultiplied.
 */

let painters: PainterHandle[] = [];
let hosts: HTMLElement[] = [];
afterEach(() => {
  for (const painter of painters) act(() => painter.unmount());
  for (const host of hosts) host.remove();
  painters = [];
  hosts = [];
});

async function controlled(): Promise<PainterHandle> {
  const host = document.createElement("div");
  document.body.appendChild(host);
  hosts.push(host);
  let painter!: PainterHandle;
  act(() => {
    painter = mount(host, {
      width: 64,
      height: 48,
      mode: { kind: "standard" },
      controls: { kind: "none" },
      recordReplay: false,
      synchronization: { actorId: "9", onOperation: () => {} },
    });
  });
  await act(async () => painter.ready);
  painters.push(painter);
  return painter;
}

const bytes = async (blob: Blob) => new Uint8Array(await blob.arrayBuffer());

describe("a checkpoint", () => {
  /**
   * The property the room needs: a client that joins from a checkpoint holds
   * exactly the layers in it, and passes exactly those on when it uploads the
   * next. Translucent pixels are the ones a canvas changed, so the layers
   * here are every alpha from 0 to 255 under a spread of colours.
   */
  it("hands a joiner the uploader's translucent layers exactly", async () => {
    const known = (seed: number) => {
      const layer = new Uint8ClampedArray(64 * 48 * 4);
      for (let i = 0; i < layer.length; i += 4) {
        const p = i / 4;
        layer[i] = (p * 37 + seed) & 255;
        layer[i + 1] = (p * 91 + 13 * seed) & 255;
        layer[i + 2] = (p * 151 + 7) & 255;
        layer[i + 3] = (p * 5 + seed) & 255;
      }
      return layer;
    };
    const png = (layer: Uint8ClampedArray) =>
      new Blob([encodeLayerPng(layer, 64, 48) as Uint8Array<ArrayBuffer>], { type: "image/png" });
    const layers = [
      { actorId: "1", background: known(1), foreground: known(2) },
      { actorId: "2", background: known(3), foreground: known(4) },
    ];

    const joined = await controlled();
    await joined.applyCheckpoint({
      sequence: 10, width: 64, height: 48,
      layers: layers.map((entry) => ({
        actorId: entry.actorId, background: png(entry.background), foreground: png(entry.foreground),
      })),
    });
    const relayed = await joined.exportCheckpoint(10);

    for (const entry of layers) {
      const held = relayed.layers.find((candidate) => candidate.actorId === entry.actorId)!;
      for (const layer of ["foreground", "background"] as const) {
        const decoded = decodeLayerPng(await bytes(held[layer]), 64, 48)!;
        expect(Array.from(decoded), `${entry.actorId} ${layer}`).toEqual(Array.from(entry[layer]));
      }
    }
  });

  /**
   * Checkpoints already in rooms' histories were written by the browser's
   * encoder, and a client joining such a room has to read them.
   */
  it("reads the browser's own PNGs", async () => {
    const canvas = document.createElement("canvas");
    canvas.width = 8;
    canvas.height = 4;
    const context = canvas.getContext("2d")!;
    const opaque = new ImageData(new Uint8ClampedArray(8 * 4 * 4).map((_, i) => (i % 4 === 3 ? 255 : (i * 7) & 255)), 8, 4);
    context.putImageData(opaque, 0, 0);
    const blob = await new Promise<Blob>((resolve) => canvas.toBlob((b) => resolve(b!), "image/png"));
    const decoded = decodeLayerPng(await bytes(blob), 8, 4);
    expect(decoded).not.toBeNull();
    expect(Array.from(decoded!)).toEqual(Array.from(opaque.data));
  });
});
